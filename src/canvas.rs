//! Das Zeichen-Widget. Kein Cairo im Frame-Pfad: `snapshot()` hängt nur fertige
//! Texturen (Kacheln + Teilstücke des aktiven Strichs) als GSK-Knoten an, die
//! GTK auf der GPU zusammensetzt.

use std::cell::{Cell, RefCell, RefMut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use glib::translate::IntoGlib;

use crate::geom::{Color, Pt, Rect};
use crate::history::{Change, History};
use crate::model::*;
use crate::render;
use crate::tiles::{self, Cache, Job, Kind, Key, Pool, TILE};
use crate::settings::{CursorStyle, PalmMode, Settings};
use crate::tools::*;

mod edit;
mod guide;
mod pages;
mod place;
mod search;
pub use guide::{ink_lines, snap_to_ink, Mark, MarkKind};
pub use pages::THUMB_W;
pub use edit::Request;
use edit::{RadialMenu, Selection, TextEdit};

/// Abstand zwischen Seiten (pt).
pub const GAP: f64 = 18.0;
/// 100 % Zoom = Originalgröße bei 96 dpi.
pub const ZOOM_100: f64 = 96.0 / 72.0;
const MIN_ZOOM: f64 = 0.15;
/// Abstandsänderung zweier Finger (px), ab der aus Schieben ein Zoom wird
/// (Android/Chromium: „span slop“). Darunter bleibt der Zoom fest.
const PINCH_SLOP: f64 = 26.0;
/// Zwei echte Fingerkuppen liegen nie näher als ~1 cm beisammen. Darunter ist der
/// zweite Kontakt ein Geist (verlorenes "Finger hoch") und es wird nicht gezoomt.
const MIN_PINCH_SPAN: f64 = 48.0;
/// Ein Kontakt ohne Lebenszeichen seit so langer Zeit gilt beim nächsten Aufsetzen als
/// verloren (sein "Finger hoch" kam nie an).
const STALE_TOUCH: Duration = Duration::from_millis(1500);
/// Echte Finger zoomen pro Bild höchstens um diesen Faktor.
const MAX_ZOOM_STEP: f64 = 1.35;
/// Größte Vorhersage-Verschiebung (px), damit ruckartige Bewegungen nicht überschießen.
const PREDICT_MAX: f64 = 48.0;
const MAX_ZOOM: f64 = 30.0;
/// Segmente pro Live-Teilstück, bevor ein neues begonnen wird.
const CHUNK_SEGS: usize = 24;

// ------------------------------------------------------------------------ Live-Strich

struct Live {
    page: PageRef,
    origin: Pt,
    tool: StrokeTool,
    color: Color,
    width: f64,
    pressure: bool,
    attrs: Attrs,
    pts: Vec<Pt>,
    ws: Vec<f64>,
    /// fertige Teilstücke (Textur, Rechteck in Layout-Koordinaten)
    chunks: Vec<(gdk::MemoryTexture, Rect)>,
    open: Option<(gdk::MemoryTexture, Rect)>,
    open_from: usize,
    scale: f64,
    /// Rohposition des letzten Samples (ungeglättet)
    raw: Pt,
    raw_w: f64,
    /// Zeitpunkt/Ort der letzten nennenswerten Bewegung (für "Halten zum Begradigen")
    still_since: Instant,
    still_at: Pt,
    straight: bool,
    /// begradigte/erkannte Form folgt dem Stift nicht mehr
    frozen: bool,
    started: Instant,
    start_screen: (f64, f64),
    /// Vorhersage (wie Apples "predicted touches"): kurzer Strich in Bewegungsrichtung,
    /// der etwa einen Frame Latenz kaschiert und beim nächsten Sample ersetzt wird.
    vel: (f64, f64),
    vel_t: Instant,
    /// One-Euro-Filter: geglättete Geschwindigkeit (pt/s) und Zeit des letzten Updates
    pub(crate) fv: (f64, f64),
    pub(crate) ft: Instant,
    /// Rohpunkte der letzten ~80 ms mit Zeit (s) für die verbesserte Vorhersage
    pub(crate) hist: std::collections::VecDeque<(f64, Pt)>,
    pub(crate) htime: f64,
    pred: Option<(gdk::MemoryTexture, Rect)>,
}

impl Live {
    fn stroke_of(&self, pts: &[Pt], ws: &[f64], opaque: bool) -> Stroke {
        let c = if opaque { self.color.with_alpha(1.0) } else { self.color };
        Stroke::new(self.tool, c, self.width, pts.to_vec(), self.pressure.then(|| ws.to_vec()), self.attrs.clone())
    }

    fn render_range(&self, from: usize) -> Option<(gdk::MemoryTexture, Rect)> {
        let st = self.stroke_of(&self.pts[from..], &self.ws[from..], true);
        texture_for(|cr| render::draw_stroke(cr, &st, None), st.bbox, self.scale, self.origin)
    }

    /// Aktualisiert das offene Teilstück; schließt es nach CHUNK_SEGS Segmenten.
    fn update_open(&mut self) {
        self.open = self.render_range(self.open_from);
        if self.pts.len() - self.open_from > CHUNK_SEGS {
            if let Some(o) = self.open.take() {
                self.chunks.push(o);
            }
            self.open_from = self.pts.len() - 1;
        }
    }

    fn rerender_all(&mut self) {
        self.chunks.clear();
        self.open_from = 0;
        self.open = self.render_range(0);
    }

    fn all_textures(&self) -> impl Iterator<Item = &(gdk::MemoryTexture, Rect)> {
        self.chunks.iter().chain(self.open.iter())
    }

    fn update_prediction(&mut self, zoom: f64, ms: f64) {
        self.pred = None;
        if self.straight || ms <= 0.0 {
            return;
        }
        let hist: Vec<(f64, Pt)> = self.hist.iter().copied().collect();
        // höchstens ~30 px voraus, Ruhe unter ~60 px/s
        let path = predict_path(&hist, ms / 1000.0, 30.0 / zoom, 60.0 / zoom);
        if path.is_empty() {
            return;
        }
        let last = *self.pts.last().unwrap();
        let w = *self.ws.last().unwrap();
        let mut pts = vec![last, self.raw];
        pts.extend(path);
        let ws = vec![w; pts.len()];
        let st = self.stroke_of(&pts, &ws, true);
        self.pred = texture_for(|cr| render::draw_stroke(cr, &st, None), st.bbox, self.scale, self.origin);
    }
}

/// Verbesserte Stift-Vorhersage: gewichtete quadratische Ausgleichskurve über die
/// letzten 50 ms liefert Geschwindigkeit und Beschleunigung. Daraus wird ein kurzes
/// Kurvenstück vorausgerechnet, das beim Abbremsen kürzer wird (kein Überschießen
/// über den Haltepunkt), Kurven folgt statt geradeaus zu laufen und bei unruhigen
/// Daten zurückhaltender ist. Ergebnis: bis zu vier Punkte nach dem letzten Rohpunkt.
fn predict_path(hist: &[(f64, Pt)], ahead: f64, max_dist: f64, min_speed: f64) -> Vec<Pt> {
    let Some(&(t_end, p_end)) = hist.last() else { return Vec::new() };
    let pts: Vec<(f64, Pt)> = hist.iter().filter(|h| t_end - h.0 <= 0.05).map(|h| (h.0 - t_end, h.1)).collect();
    if pts.len() < 4 || -pts[0].0 < 0.012 {
        return Vec::new();
    }
    // gewichtete kleinste Quadrate für x(t) = a + b·t + c·t² (neuere Punkte zählen mehr)
    let mut m = [[0.0f64; 3]; 3];
    let mut rx = [0.0f64; 3];
    let mut ry = [0.0f64; 3];
    for &(t, p) in &pts {
        let w = (t / 0.025).exp();
        let f = [1.0, t, t * t];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += w * f[i] * f[j];
            }
            rx[i] += w * f[i] * p.x;
            ry[i] += w * f[i] * p.y;
        }
    }
    let (Some(cx), Some(cy)) = (solve3(m, rx), solve3(m, ry)) else { return Vec::new() };
    let v = Pt::new(cx[1], cy[1]);
    let a = Pt::new(2.0 * cx[2], 2.0 * cy[2]);
    let speed = v.x.hypot(v.y);
    if speed < min_speed {
        return Vec::new();
    }
    // Beschleunigung in Bewegungsrichtung und quer dazu
    let (ux, uy) = (v.x / speed, v.y / speed);
    let a_par = a.x * ux + a.y * uy;
    let mut a_perp = -a.x * uy + a.y * ux;
    // Abbremsen: höchstens bis zum (geschätzten) Stillstand vorausrechnen
    let mut h = ahead;
    if a_par < 0.0 {
        h = h.min(speed / -a_par);
    }
    // unruhige Daten (Zittern, Messfehler): Vorhersage zurücknehmen
    let resid = pts
        .iter()
        .map(|&(t, p)| {
            let fx = cx[0] + cx[1] * t + cx[2] * t * t;
            let fy = cy[0] + cy[1] * t + cy[2] * t * t;
            (fx - p.x).powi(2) + (fy - p.y).powi(2)
        })
        .sum::<f64>()
        / pts.len() as f64;
    let step = speed * 0.008;
    if step > 0.0 {
        h *= (1.0 / (1.0 + resid.sqrt() / step)).clamp(0.3, 1.0);
    }
    // Kurven nicht übertreiben: Querversatz höchstens halb so groß wie der Weg
    let max_perp = speed / h.max(1e-3);
    a_perp = a_perp.clamp(-max_perp, max_perp);
    let a_par = a_par.max(-speed / h.max(1e-3));
    let at = |t: f64| {
        let along = speed * t + 0.5 * a_par * t * t;
        let across = 0.5 * a_perp * t * t;
        Pt::new(p_end.x + ux * along - uy * across, p_end.y + uy * along + ux * across)
    };
    let mut out: Vec<Pt> = (1..=4).map(|k| at(h * k as f64 / 4.0)).collect();
    // Gesamtlänge begrenzen
    let len = out.last().unwrap().dist(p_end);
    if len > max_dist {
        let k = max_dist / len;
        for p in &mut out {
            *p = Pt::new(p_end.x + (p.x - p_end.x) * k, p_end.y + (p.y - p_end.y) * k);
        }
    }
    out
}

/// Finger-Vorhersage für Schieben und Zoomen, nach demselben Prinzip wie beim Stift:
/// gewichtete quadratische Ausgleichskurve über Schwerpunkt (x, y) und Fingerabstand.
/// `pts` = (Zeit relativ zum letzten Stand in s, [x, y, Abstand]). Liefert den
/// vorhergesagten Versatz in `ahead` Sekunden. Abbremsen wird erkannt (höchstens bis
/// zum Stillstand), unruhige Daten verkürzen die Vorhersage. Kurven werden bewusst
/// nicht vorhergesagt: ein Fehler verschiebt hier die ganze Seite.
fn predict_touch(pts: &[(f64, [f64; 3])], ahead: f64) -> Option<[f64; 3]> {
    if pts.len() < 4 || -pts[0].0 < 0.012 {
        return None;
    }
    let mut m = [[0.0f64; 3]; 3];
    let mut r = [[0.0f64; 3]; 3];
    for &(t, v) in pts {
        let w = (t / 0.025).exp();
        let f = [1.0, t, t * t];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += w * f[i] * f[j];
            }
            for k in 0..3 {
                r[k][i] += w * f[i] * v[k];
            }
        }
    }
    let c = [solve3(m, r[0])?, solve3(m, r[1])?, solve3(m, r[2])?];
    // mittlere Abweichung der Messpunkte von der Kurve für die Komponenten `ks`
    let resid = |ks: &[usize]| {
        let e = pts
            .iter()
            .map(|&(t, v)| ks.iter().map(|&k| (c[k][0] + c[k][1] * t + c[k][2] * t * t - v[k]).powi(2)).sum::<f64>())
            .sum::<f64>();
        (e / pts.len() as f64).sqrt()
    };
    // Weg entlang einer Richtung mit Geschwindigkeit `speed` und Beschleunigung `acc`
    let travel = |speed: f64, acc: f64, noise: f64| {
        if speed < 20.0 {
            return 0.0;
        }
        let mut h = ahead;
        if acc < 0.0 {
            h = h.min(speed / -acc);
        }
        h *= (1.0 / (1.0 + noise / (speed * 0.008))).clamp(0.3, 1.0);
        let acc = acc.clamp(-speed / h.max(1e-3), speed / h.max(1e-3));
        speed * h + 0.5 * acc * h * h
    };
    // Schieben: nur in Bewegungsrichtung
    let (vx, vy) = (c[0][1], c[1][1]);
    let speed = vx.hypot(vy);
    let (dx, dy) = if speed > 0.0 {
        let (ux, uy) = (vx / speed, vy / speed);
        let along = travel(speed, 2.0 * (c[0][2] * ux + c[1][2] * uy), resid(&[0, 1]));
        let k = along.min(PREDICT_MAX);
        (ux * k, uy * k)
    } else {
        (0.0, 0.0)
    };
    // Zoomen: Fingerabstand
    let vs = c[2][1];
    let ds = vs.signum() * travel(vs.abs(), 2.0 * c[2][2] * vs.signum(), resid(&[2])).min(PREDICT_MAX);
    Some([dx, dy, ds])
}

/// 3×3-Gleichungssystem (Cramersche Regel); None, wenn singulär.
fn solve3(m: [[f64; 3]; 3], r: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(&m);
    if d.abs() < 1e-18 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut mk = m;
        for i in 0..3 {
            mk[i][k] = r[i];
        }
        *o = det(&mk) / d;
    }
    Some(out)
}

/// Rastert eine Zeichenfunktion für den Bereich `bb` (Seitenkoordinaten) in eine Textur.
fn texture_for(f: impl FnOnce(&cairo::Context), bb: Rect, scale: f64, origin: Pt) -> Option<(gdk::MemoryTexture, Rect)> {
    let x0 = (bb.x0 * scale).floor();
    let y0 = (bb.y0 * scale).floor();
    let w = ((bb.x1 * scale).ceil() - x0).max(1.0) as i32;
    let h = ((bb.y1 * scale).ceil() - y0).max(1.0) as i32;
    if w > 8192 || h > 8192 {
        return None;
    }
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    {
        let cr = cairo::Context::new(&surf).ok()?;
        cr.translate(-x0, -y0);
        cr.scale(scale, scale);
        f(&cr);
    }
    let stride = surf.stride() as usize;
    let data = surf.take_data().ok()?;
    let tex = gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &glib::Bytes::from_owned(data), stride);
    let r = Rect::new(x0 / scale, y0 / scale, (x0 + w as f64) / scale, (y0 + h as f64) / scale);
    Some((tex, r.translate(origin.x, origin.y)))
}

/// Overlay eines gerade abgeschlossenen Strichs, bis die Kacheln ihn enthalten.
struct Pending {
    page: u64,
    ver: u64,
    rect: Rect,
    alpha: f64,
    tex: Vec<(gdk::MemoryTexture, Rect)>,
}

enum Action {
    Draw(Live),
    Erase { page: PageRef, origin: Pt, orig: Vec<ItemList>, last: Pt },
    Pan { last: (f64, f64) },
    Lasso { page: PageRef, origin: Pt, pts: Vec<Pt>, rect: bool },
    SelMove { start: Pt, xf0: crate::geom::Xform },
    SelScale { anchor: Pt, start_d: f64, xf0: crate::geom::Xform },
    SelRotate { center: Pt, a0: f64, xf0: crate::geom::Xform },
    /// Radierer in ausgewählten Bildern: macht Pixel durchsichtig
    ImageErase { parts: Vec<edit::ImageErase>, last: Pt },
    Shape { page: PageRef, origin: Pt, kind: ShapeKind, a: Pt, b: Pt },
    /// Platz einfügen (nach unten ziehen) oder entfernen (nach oben, höchstens bis `min_y`)
    Space { page: PageRef, origin: Pt, y0: f64, y: f64, min_y: f64 },
    PageHeight { page: PageRef, h0: f64, min_h: f64, y0: f64 },
    Radial { center: (f64, f64), moved: bool },
    /// Marker an PDF-Text: Zeichenbereich [ia, ib]
    TextMark { page: PageRef, origin: Pt, text: Arc<crate::pdftext::PageText>, ia: usize, ib: usize },
}

impl Action {
    fn name(&self) -> &'static str {
        match self {
            Action::Draw(_) => "Zeichnen",
            Action::Erase { .. } => "Radieren",
            Action::Pan { .. } => "Verschieben (Maus/Stift)",
            Action::Lasso { .. } => "Lasso",
            Action::SelMove { .. } => "Auswahl verschieben",
            Action::SelScale { .. } => "Auswahl skalieren",
            Action::SelRotate { .. } => "Auswahl drehen",
            Action::ImageErase { .. } => "Im Bild radieren",
            Action::Shape { .. } => "Form",
            Action::Space { .. } => "Platz einfügen/entfernen",
            Action::PageHeight { .. } => "Seitenhöhe",
            Action::Radial { .. } => "Radialmenü offen",
            Action::TextMark { .. } => "Text markieren",
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    x: f64,
    y: f64,
    pressure: Option<f64>,
    /// Neigung aus der Senkrechten in Grad (0 = aufrecht)
    tilt: f64,
}

// ------------------------------------------------------------------------ Zustand

pub struct State {
    pub doc: Document,
    pub history: History,
    pub tools: ToolState,
    /// Werkzeug, zu dem das Einkreisen zurückkehrt
    pub before_ask: Option<Tool>,
    pub zoom: f64,
    /// Layout-Koordinate der linken oberen Ecke des Sichtbereichs.
    pub cam: Pt,
    view: (f64, f64),
    sf: f64,
    /// Oberkanten der Seiten im Layout
    tops: Vec<f64>,
    total_h: f64,
    max_w: f64,
    /// Rasterskala für Kacheln; hinkt beim Zoomen absichtlich hinterher.
    render_scale: f64,
    cache: Cache,
    pool: Pool,
    last_submit: u64,
    action: Option<Action>,
    pending: Vec<Pending>,
    pub dark_paper: bool,
    pub sel: Option<Selection>,
    last_stroke_end: Option<Instant>,
    thumbs_wanted: std::collections::HashSet<u64>,
    /// PDF-Text je PDF-Seitenindex (lazy)
    text_cache: std::collections::HashMap<usize, Option<Arc<crate::pdftext::PageText>>>,
    /// Suchtreffer: (Seiten-ID, Rechtecke in Seitenkoordinaten)
    search: Vec<(u64, Vec<Rect>)>,
    /// temporäres Zeichen von Claude
    guide: Option<guide::Guide>,
    search_idx: usize,
    pub text: Option<TextEdit>,
    radial: Option<RadialMenu>,
    /// laufender Tipp-Kandidat bei Radierer/Auswahl/Form/Platz/Hand: (Start, Bildschirmpunkt, zu weit bewegt)
    tap: Option<(Instant, (f64, f64), bool)>,
    /// gemerkte Leseposition, wird beim ersten Layout angefahren
    restore: Option<crate::positions::Position>,
    /// zuletzt gespeicherte Leseposition (um unnötiges Schreiben zu sparen)
    remembered: Option<crate::positions::Position>,
    clip: Vec<Item>,
    pub cfg: Settings,
    pen_near: bool,
    /// zuletzt mit Maus oder Touchpad bewegt: normaler Mauszeiger statt Stift-Cursor
    mouse_pointer: bool,
    last_pen: Option<Instant>,
    barrel: u32,
    hover: Option<(f64, f64)>,
    needs_fit: bool,
    /// Seitenbreite halten, bis der Nutzer selbst zoomt
    auto_fit: bool,
    pub readonly: bool,
}

impl State {
    fn new(pool: Pool) -> State {
        let mut st = State {
            doc: Document::new_blank("plain"),
            history: History::default(),
            tools: ToolState::default(),
            zoom: ZOOM_100,
            cam: Pt::new(0.0, 0.0),
            view: (800.0, 600.0),
            sf: 1.0,
            tops: Vec::new(),
            total_h: 0.0,
            max_w: 0.0,
            render_scale: ZOOM_100,
            cache: Cache::new(384),
            pool,
            last_submit: 0,
            action: None,
            pending: Vec::new(),
            dark_paper: false,
            sel: None,
            last_stroke_end: None,
            thumbs_wanted: Default::default(),
            text_cache: Default::default(),
            search: Vec::new(),
            guide: None,
            search_idx: 0,
            text: None,
            radial: None,
            tap: None,
            restore: None,
            remembered: None,
            clip: Vec::new(),
            cfg: Settings::load(),
            pen_near: false,
            mouse_pointer: false,
            last_pen: None,
            barrel: 0,
            hover: None,
            before_ask: None,
            needs_fit: true,
            auto_fit: true,
            readonly: false,
        };
        st.dark_paper = st.cfg.dark_paper;
        if st.cfg.eraser_whole {
            st.tools.eraser_mode = EraserMode::Stroke;
        }
        st.doc = Document::new_blank(&st.cfg.paper_style);
        st.relayout();
        render::CLASSIC_INK.store(st.cfg.ink_style == crate::settings::InkStyle::Classic, std::sync::atomic::Ordering::Relaxed);
        st
    }

    // ---------------------------------------------------------------- Layout & Kamera

    pub fn relayout(&mut self) {
        self.tops.clear();
        let mut y = GAP;
        let mut mw: f64 = 0.0;
        for p in &self.doc.pages {
            let p = p.borrow();
            self.tops.push(y);
            y += p.height + GAP;
            mw = mw.max(p.width);
        }
        self.total_h = y;
        self.max_w = mw;
    }

    pub fn page_origin(&self, i: usize) -> Pt {
        let w = self.doc.pages[i].borrow().width;
        Pt::new(-w / 2.0, self.tops[i])
    }

    pub fn page_layout_rect(&self, i: usize) -> Rect {
        let p = self.doc.pages[i].borrow();
        Rect::new(-p.width / 2.0, self.tops[i], p.width / 2.0, self.tops[i] + p.height)
    }

    pub(crate) fn view_layout(&self) -> Rect {
        Rect::new(self.cam.x, self.cam.y, self.cam.x + self.view.0 / self.zoom, self.cam.y + self.view.1 / self.zoom)
    }

    pub fn to_layout(&self, wx: f64, wy: f64) -> Pt {
        Pt::new(self.cam.x + wx / self.zoom, self.cam.y + wy / self.zoom)
    }

    /// Seite unter einem Widget-Punkt und die Position in Seitenkoordinaten.
    pub fn page_at(&self, wx: f64, wy: f64, margin: f64) -> Option<(usize, Pt)> {
        let l = self.to_layout(wx, wy);
        (0..self.doc.pages.len()).find_map(|i| {
            let r = self.page_layout_rect(i);
            r.expand(margin).contains(l).then(|| (i, Pt::new(l.x - r.x0, l.y - r.y0)))
        })
    }

    fn cam_bounds(&self) -> (f64, f64, f64, f64) {
        let vw = self.view.0 / self.zoom;
        let vh = self.view.1 / self.zoom;
        // Völlig frei verschieben und zoomen; einzige Regel: ein Streifen des
        // Schreibbereichs (ca. 60 px) bleibt immer sichtbar.
        let (cx0, cx1) = (-self.max_w / 2.0, self.max_w / 2.0);
        let (cy0, cy1) = (0.0, self.total_h);
        let m = 60.0 / self.zoom;
        let mx = m.min((cx1 - cx0) / 2.0);
        let my = m.min((cy1 - cy0) / 2.0);
        (cx0 - vw + mx, cx1 - mx, cy0 - vh + my, cy1 - my)
    }

    pub fn clamp_cam(&mut self) {
        let (x0, x1, y0, y1) = self.cam_bounds();
        self.cam.x = self.cam.x.clamp(x0, x1);
        self.cam.y = self.cam.y.clamp(y0, y1);
    }

    pub fn set_zoom_at(&mut self, z: f64, ax: f64, ay: f64) {
        self.auto_fit = false;
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        let l = self.to_layout(ax, ay);
        self.zoom = z;
        self.cam = Pt::new(l.x - ax / z, l.y - ay / z);
        self.clamp_cam();
    }

    pub fn fit_width(&mut self) {
        if self.max_w <= 0.0 {
            return;
        }
        let z = ((self.view.0 - 24.0) / self.max_w).clamp(MIN_ZOOM, MAX_ZOOM);
        let center_y = self.cam.y + self.view.1 / self.zoom / 2.0;
        self.zoom = z;
        self.cam.y = center_y - self.view.1 / z / 2.0;
        self.cam.x = -self.view.0 / z / 2.0;
        self.clamp_cam();
        self.render_scale = self.zoom * self.sf;
    }

    /// Index der Seite in der Mitte des Sichtbereichs.
    pub fn current_page(&self) -> usize {
        let cy = self.cam.y + self.view.1 / self.zoom * 0.4;
        match self.tops.iter().rposition(|t| *t <= cy) {
            Some(i) => i,
            None => 0,
        }
    }

    pub fn scroll_to_page(&mut self, i: usize) {
        if i < self.tops.len() {
            self.cam.y = self.tops[i] - GAP / 2.0;
            self.clamp_cam();
        }
    }

    pub fn set_document(&mut self, doc: Document) {
        self.doc = doc;
        self.history = History::default();
        self.cache.clear();
        self.pending.clear();
        self.action = None;
        self.sel = None;
        self.text = None;
        self.radial = None;
        self.text_cache.clear();
        self.search.clear();
        self.guide = None;
        self.cam = Pt::new(0.0, 0.0);
        self.relayout();
        self.needs_fit = true;
        self.auto_fit = true;
        self.restore = self.doc_key().and_then(|k| crate::positions::get(&k));
        self.remembered = self.restore;
    }

    /// Datei, unter der die Leseposition gemerkt wird (.xopp, sonst das PDF).
    pub fn doc_key(&self) -> Option<std::path::PathBuf> {
        self.doc.path.clone().or_else(|| self.doc.pdf_path.clone())
    }

    /// Aktuelle Leseposition: Seite an der Oberkante der Ansicht und Versatz darin.
    pub fn position(&self) -> Option<crate::positions::Position> {
        if self.tops.is_empty() || self.needs_fit {
            return None;
        }
        let y = self.cam.y + GAP / 2.0;
        let page = self.tops.iter().rposition(|t| *t <= y).unwrap_or(0);
        Some(crate::positions::Position {
            page,
            offset: self.cam.y - self.tops[page],
            zoom: (!self.auto_fit).then_some((self.zoom, self.cam.x)),
        })
    }

    /// Gemerkte Position anfahren (nach dem ersten Layout).
    fn apply_restore(&mut self) {
        let Some(p) = self.restore.take() else { return };
        let Some(&top) = self.tops.get(p.page.min(self.tops.len().saturating_sub(1))) else { return };
        if let Some((z, x)) = p.zoom {
            self.zoom = z.clamp(MIN_ZOOM, MAX_ZOOM);
            self.cam.x = x;
            self.auto_fit = false;
            self.render_scale = self.zoom * self.sf;
        }
        self.cam.y = top + p.offset;
        self.clamp_cam();
    }

    // ---------------------------------------------------------------- Bearbeitung

    fn push_items_change(&mut self, page: &PageRef, layer: usize, new: ItemList) {
        let old = page.borrow().layers[layer].items.clone();
        page.borrow_mut().set_items(layer, new.clone());
        self.history.push(vec![Change::Items { page: page.clone(), layer, old, new }]);
    }

    pub fn undo(&mut self) -> bool {
        self.action = None;
        let r = self.history.undo(&mut self.doc);
        self.relayout();
        r
    }

    pub fn redo(&mut self) -> bool {
        self.action = None;
        let r = self.history.redo(&mut self.doc);
        self.relayout();
        r
    }

    /// Läuft gerade eine Stift-Aktion? (Dock wird dann ausgeblendet.)
    pub fn is_busy(&self) -> bool {
        matches!(
            self.action,
            Some(Action::Draw(_)) | Some(Action::Erase { .. }) | Some(Action::ImageErase { .. }) | Some(Action::TextMark { .. })
        )
    }

    fn width_for(&self, base: f64, p: Option<f64>, tilt: f64) -> f64 {
        // Druck-Kalibrierung: gemessener Bereich wird auf 0…1 gestreckt
        let (lo, hi) = (self.cfg.pressure_lo, self.cfg.pressure_hi.max(self.cfg.pressure_lo + 0.05));
        let p = p.map(|p| ((p - lo) / (hi - lo)).clamp(0.0, 1.0));
        // Bleistift: ab 35° Neigung wird der Strich breiter, bei 70° dreimal so breit
        let shade = if self.cfg.tilt_shading { 1.0 + 2.0 * ((tilt - 35.0) / 35.0).clamp(0.0, 1.0) } else { 1.0 };
        shade * self.width_raw(base, p)
    }

    fn width_raw(&self, base: f64, p: Option<f64>) -> f64 {
        match p {
            Some(p) if self.cfg.pressure => base * (0.22 + 1.05 * p.clamp(0.0, 1.0).powf(self.cfg.pressure_curve)),
            _ => base,
        }
    }

    // ---------------------------------------------------------------- Kacheln

    /// Hängt die Kacheln einer Seite an und sammelt fehlende/veraltete als Jobs.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_tiles(
        &mut self,
        snap: &gtk::Snapshot,
        i: usize,
        kind: Kind,
        vis: &Rect,
        pre: &Rect,
        jobs: &mut Vec<(f64, Job)>,
    ) {
        let page = self.doc.pages[i].clone();
        let pg = page.borrow();
        let origin = self.page_origin(i);
        let s = self.render_scale;
        let level = tiles::level_of(s);
        let s = tiles::scale_of(level);
        let (pw, ph) = ((pg.width * s).ceil() as i32, (pg.height * s).ceil() as i32);
        let t = TILE as f64 / s;
        let range = |r: &Rect| {
            let lr = r.translate(-origin.x, -origin.y);
            let tx0 = ((lr.x0 / t).floor() as i32).max(0);
            let ty0 = ((lr.y0 / t).floor() as i32).max(0);
            let tx1 = ((lr.x1 / t).floor() as i32).min((pw - 1) / TILE);
            let ty1 = ((lr.y1 / t).floor() as i32).min((ph - 1) / TILE);
            (tx0, ty0, tx1, ty1)
        };
        let want_ver = if kind == Kind::Pdf { pg.bg_ver } else { pg.ink_ver };
        let frame = self.cache.frame;
        let mut data: Option<Arc<PageData>> = None;
        let make_job = |key: Key, data: &mut Option<Arc<PageData>>| Job {
            key,
            ver: want_ver,
            data: data.get_or_insert_with(|| pg.snapshot(self.doc.page_pdf(&pg))).clone(),
        };

        // 1) sichtbare Kacheln prüfen
        let (tx0, ty0, tx1, ty1) = range(vis);
        let mut missing = false;
        let mut present = Vec::new();
        let center = vis.center();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let key = Key { page: pg.id, kind, level, tx, ty };
                let valid = match self.cache.tiles.get_mut(&key) {
                    Some(tile) => {
                        tile.used = frame;
                        let ok = if kind != Kind::Pdf && kind != Kind::Thumb { pg.tile_valid(tile.ver, &key.page_rect()) } else { tile.ver >= want_ver };
                        if ok {
                            tile.ver = tile.ver.max(want_ver);
                        }
                        present.push(key);
                        ok
                    }
                    None => {
                        missing = true;
                        false
                    }
                };
                if !valid {
                    let r = key.page_rect().translate(origin.x, origin.y);
                    jobs.push((r.center().dist(center), make_job(key, &mut data)));
                }
            }
        }
        // 2) Vorladen rund um den Sichtbereich (niedrigere Priorität)
        let (px0, py0, px1, py1) = range(pre);
        for ty in py0..=py1 {
            for tx in px0..=px1 {
                if tx >= tx0 && tx <= tx1 && ty >= ty0 && ty <= ty1 {
                    continue;
                }
                let key = Key { page: pg.id, kind, level, tx, ty };
                let ok = match self.cache.tiles.get_mut(&key) {
                    Some(tile) => {
                        tile.used = frame;
                        if kind != Kind::Pdf && kind != Kind::Thumb { pg.tile_valid(tile.ver, &key.page_rect()) } else { tile.ver >= want_ver }
                    }
                    None => false,
                };
                if !ok {
                    let r = key.page_rect().translate(origin.x, origin.y);
                    jobs.push((1e6 + r.center().dist(center), make_job(key, &mut data)));
                }
            }
        }

        // 3) Ersatz aus anderen Zoomstufen, solange etwas fehlt
        if missing {
            let mut fb: Vec<(u32, Key)> = self
                .cache
                .tiles
                .keys()
                .filter(|k| k.page == pg.id && k.kind == kind && k.level != level)
                .filter(|k| k.page_rect().translate(origin.x, origin.y).intersects(vis))
                .map(|k| ((k.level as i64 - level as i64).unsigned_abs() as u32, *k))
                .collect();
            fb.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            for (_, k) in fb {
                self.append_tile(snap, &k, origin);
            }
        }
        for k in present {
            self.append_tile(snap, &k, origin);
        }
    }

    fn append_tile(&self, snap: &gtk::Snapshot, key: &Key, origin: Pt) {
        let Some(tile) = self.cache.tiles.get(key) else { return };
        let s = tiles::scale_of(key.level);
        let k = self.zoom * self.sf / s;
        // auf Geräte-Pixel runden, damit benachbarte Kacheln nahtlos sind
        let ox = (origin.x - self.cam.x) * self.zoom * self.sf;
        let oy = (origin.y - self.cam.y) * self.zoom * self.sf;
        let px = (key.tx * TILE) as f64;
        let py = (key.ty * TILE) as f64;
        let x0 = (ox + px * k).round() / self.sf;
        let y0 = (oy + py * k).round() / self.sf;
        let x1 = (ox + (px + tile.w as f64) * k).round() / self.sf;
        let y1 = (oy + (py + tile.h as f64) * k).round() / self.sf;
        let filter = if (k - 1.0).abs() < 1e-3 {
            gsk::ScalingFilter::Nearest
        } else if k < 0.7 {
            gsk::ScalingFilter::Trilinear
        } else {
            gsk::ScalingFilter::Linear
        };
        snap.append_scaled_texture(&tile.tex, filter, &graphene::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32));
    }

    fn to_screen_rect(&self, r: &Rect) -> graphene::Rect {
        let x0 = (r.x0 - self.cam.x) * self.zoom;
        let y0 = (r.y0 - self.cam.y) * self.zoom;
        graphene::Rect::new(x0 as f32, y0 as f32, (r.width() * self.zoom) as f32, (r.height() * self.zoom) as f32)
    }

    /// Prüft, ob alle Kacheln unter einem Pending-Overlay schon aktuell sind.
    /// Inhaltskacheln passend zum Papiermodus.
    fn ink_kind(&self) -> Kind {
        if self.dark_paper { Kind::InkDark } else { Kind::Ink }
    }

    fn pending_done(&self, p: &Pending) -> bool {
        let Some(i) = self.doc.pages.iter().position(|pg| pg.borrow().id == p.page) else { return true };
        let origin = self.page_origin(i);
        let level = tiles::level_of(self.render_scale);
        let t = TILE as f64 / tiles::scale_of(level);
        let lr = p.rect.translate(-origin.x, -origin.y);
        let pg = self.doc.pages[i].borrow();
        let (pw, ph) = ((pg.width / t).ceil() as i32, (pg.height / t).ceil() as i32);
        for ty in ((lr.y0 / t).floor() as i32).max(0)..=((lr.y1 / t).floor() as i32).min(ph - 1) {
            for tx in ((lr.x0 / t).floor() as i32).max(0)..=((lr.x1 / t).floor() as i32).min(pw - 1) {
                let key = Key { page: p.page, kind: self.ink_kind(), level, tx, ty };
                match self.cache.tiles.get(&key) {
                    Some(tile) if tile.ver >= p.ver => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

// ------------------------------------------------------------------------ Stift-Messung

/// Messdaten eines Strichs für GRAPHIT_PENLOG: Gerätezeiten aller Punkte,
/// Zustellverzögerung (Ereigniszeit → Ankunft in Graphit) und Druck beim Aufsetzen.
#[derive(Default)]
pub struct PenLog {
    times: Vec<u32>,
    delays: Vec<i64>,
    press_pressure: f64,
    first_pressures: Vec<f64>,
    /// Druckwerte der Schwebe-Punkte in den 60 ms vor dem Aufsetzen
    before: Vec<String>,
    events: usize,
}

impl PenLog {
    fn enabled() -> bool {
        std::env::var("GRAPHIT_PENLOG").is_ok()
    }

    /// Ereigniszeit (ms, Uhr des Compositors = CLOCK_MONOTONIC) gegen jetzt.
    fn delay(ev: &gdk::Event) -> i64 {
        let now = (glib::monotonic_time() / 1000) as u32;
        now.wrapping_sub(ev.time()) as i32 as i64
    }

    fn add_event(&mut self, ev: &gdk::Event) {
        self.events += 1;
        self.delays.push(Self::delay(ev));
        if ev.event_type() == gdk::EventType::MotionNotify {
            let pp = gdk::AxisUse::Pressure.into_glib() as usize;
            for tc in ev.history() {
                self.times.push(tc.time());
                if self.first_pressures.len() < 6 && tc.flags().contains(gdk::AxisFlags::PRESSURE) {
                    self.first_pressures.push(tc.axes()[pp]);
                }
            }
        }
        self.times.push(ev.time());
    }

    fn report(mut self) {
        self.times.dedup();
        if self.times.len() < 3 {
            return;
        }
        let dur = self.times.last().unwrap().wrapping_sub(self.times[0]) as f64;
        let gaps: Vec<u32> = self.times.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
        let mut sorted = gaps.clone();
        sorted.sort_unstable();
        let rate = if dur > 0.0 { (self.times.len() - 1) as f64 * 1000.0 / dur } else { 0.0 };
        let d: Vec<i64> = self.delays.iter().copied().filter(|d| d.abs() < 10_000).collect();
        let davg = if d.is_empty() { f64::NAN } else { d.iter().sum::<i64>() as f64 / d.len() as f64 };
        let dmax = d.iter().copied().max().unwrap_or(-1);
        let line = format!(
            "Strich: {} Punkte in {:.0} ms → {:.0} Hz (Abstand Median {} ms, max {} ms) | {} Ereignisse | Zustellung Ø {:.1} ms, max {} ms | Druck beim Aufsetzen {:.3}, danach {:?} | davor schwebend [{}]\n",
            self.times.len(),
            dur,
            rate,
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1],
            self.events,
            davg,
            dmax,
            self.press_pressure,
            self.first_pressures.iter().map(|p| (p * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
            self.before.join(", "),
        );
        eprint!("{line}");
        let path = glib::user_cache_dir().join("graphit").join("penlog.txt");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }
}

// ------------------------------------------------------------------------ Gesten-Protokoll

/// Eine Fingergeste für ~/.cache/graphit/gesten.log. Auffällige Gesten (Zoom mit nur
/// einem Finger, extreme Sprünge, entfernte Geisterkontakte) werden markiert, damit
/// sich Fehlerkennungen später gezielt finden lassen.
/// Ein angewendetes Bild einer Geste (für den Verlauf in gesten-verlauf.log).
struct GestureFrame {
    ms: f64,
    contacts: usize,
    /// echter Finger-Schwerpunkt (ohne Vorhersage)
    finger: (f64, f64),
    /// Vorhersage-Versatz: x, y, Fingerabstand
    pred: [f64; 3],
    zoom: f64,
    /// wie weit sich die Ansicht in diesem Bild bewegt hat (Bildschirm-px)
    view_step: f64,
    /// wie weit sich der Finger seit dem letzten Bild bewegt hat
    finger_step: f64,
    /// Bild beim Loslassen (Vorhersage wird zurückgenommen)
    lift: bool,
}

pub struct GestureLog {
    started: Instant,
    frames: Vec<GestureFrame>,
    wall: String,
    zoom0: f64,
    zoom_min: f64,
    zoom_max: f64,
    first: Option<(f64, f64)>,
    pan_max: f64,
    span0: Option<f64>,
    contacts: usize,
    fling: bool,
    flags: Vec<&'static str>,
}

impl GestureLog {
    fn start(zoom: f64) -> GestureLog {
        let wall = glib::DateTime::now_local().ok().and_then(|d| d.format("%F %T").ok()).map(|s| s.to_string()).unwrap_or_default();
        GestureLog {
            started: Instant::now(),
            frames: Vec::new(),
            wall,
            zoom0: zoom,
            zoom_min: zoom,
            zoom_max: zoom,
            first: None,
            pan_max: 0.0,
            span0: None,
            contacts: 0,
            fling: false,
            flags: Vec::new(),
        }
    }

    /// Ein Finger wurde gar nicht erst angenommen.
    fn ignored(reason: &str) {
        let wall = glib::DateTime::now_local().ok().and_then(|d| d.format("%F %T").ok()).map(|s| s.to_string()).unwrap_or_default();
        let dir = glib::user_cache_dir().join("graphit");
        for name in ["gesten.log", "gesten-verlauf.log"] {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(name)) {
                use std::io::Write;
                let _ = writeln!(f, "{}{wall} | IGNORIERT: {reason}", if name == "gesten.log" { "" } else { "== " });
            }
        }
    }

    fn flag(&mut self, f: &'static str) {
        if !self.flags.contains(&f) {
            self.flags.push(f);
        }
    }

    fn track(&mut self, zoom: f64, c: (f64, f64), span0: Option<f64>) {
        self.zoom_min = self.zoom_min.min(zoom);
        self.zoom_max = self.zoom_max.max(zoom);
        let f = *self.first.get_or_insert(c);
        self.pan_max = self.pan_max.max((c.0 - f.0).hypot(c.1 - f.1));
        if self.span0.is_none() {
            self.span0 = span0;
        }
    }

    /// Ein Bild der Geste festhalten. `view_step` = Bewegung der Ansicht in Bildschirm-px.
    fn frame(&mut self, contacts: usize, finger: (f64, f64), pred: [f64; 3], zoom: f64, view_step: f64, lift: bool) {
        let ms = self.started.elapsed().as_secs_f64() * 1000.0;
        let prev = self.frames.last().map(|p| (p.contacts, p.finger, p.zoom, p.ms));
        let finger_step = prev
            .filter(|p| p.0 == contacts)
            .map(|p| (finger.0 - p.1 .0).hypot(finger.1 - p.1 .1))
            .unwrap_or(0.0);
        if let Some((p_contacts, _, p_zoom, p_ms)) = prev {
            // Ansicht springt deutlich weiter als der Finger (gleiche Fingerzahl, kein Zoom)
            let same_zoom = (zoom / p_zoom - 1.0).abs() < 0.01;
            if p_contacts == contacts && same_zoom && view_step > finger_step + 40.0 {
                self.flag(if lift { "rücksprung-beim-loslassen" } else { "sprung" });
            }
            // lange kein Bild, obwohl der Finger sich bewegt hat
            if ms - p_ms > 90.0 && finger_step > 30.0 {
                self.flag("ruckler");
            }
        }
        if self.frames.len() < 2000 {
            self.frames.push(GestureFrame { ms, contacts, finger, pred, zoom, view_step, finger_step, lift });
        }
    }

    fn finish(mut self, zoom_end: f64) {
        let ms = self.started.elapsed().as_millis();
        let ratio = (self.zoom_max / self.zoom0).max(self.zoom0 / self.zoom_min);
        // Auffällig: Zoom mit nur einem Kontakt, sehr kurze Geste mit großem Zoom,
        // oder ein Schutz hat eingegriffen
        if self.contacts < 2 && ratio > 1.02 {
            self.flag("zoom-ohne-zwei-finger");
        }
        if ms < 300 && ratio > 1.5 {
            self.flag("kurz-aber-starker-zoom");
        }
        let line = format!(
            "{} | {} ms | Finger {} | Zoom {:.2}→{:.2} (max ×{:.2}) | Schieben {:.0} px | Startabstand {} | Schwung {} | {}\n",
            self.wall,
            ms,
            self.contacts,
            self.zoom0,
            zoom_end,
            ratio,
            self.pan_max,
            self.span0.map(|d| format!("{d:.0} px")).unwrap_or_else(|| "–".into()),
            if self.fling { "ja" } else { "nein" },
            if self.flags.is_empty() { "ok".to_string() } else { format!("AUFFÄLLIG: {}", self.flags.join(", ")) },
        );
        let dir = glib::user_cache_dir().join("graphit");
        let path = dir.join("gesten.log");
        // höchstens ~2 MB, dann die alte Datei als .1 behalten
        if std::fs::metadata(&path).map(|m| m.len() > 2 << 20).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join("gesten.log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
        // Verlauf Bild für Bild, um Sprünge nachträglich genau anzusehen
        let path = dir.join("gesten-verlauf.log");
        if std::fs::metadata(&path).map(|m| m.len() > 8 << 20).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join("gesten-verlauf.log.1"));
        }
        let mut text = format!("== {line}");
        for f in &self.frames {
            text += &format!(
                "  +{:5.0} ms | Finger {} | Pos {:5.0},{:5.0} | Vorhersage {:+4.0},{:+4.0} Abstand {:+4.0} | Zoom {:.3} | Ansicht {:4.0} px | Finger {:4.0} px{}\n",
                f.ms,
                f.contacts,
                f.finger.0,
                f.finger.1,
                f.pred[0],
                f.pred[1],
                f.pred[2],
                f.zoom,
                f.view_step,
                f.finger_step,
                if f.lift { " | losgelassen" } else { "" }
            );
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = f.write_all(text.as_bytes());
        }
    }
}

// ------------------------------------------------------------------------ Touch

/// Finger-Zustand: beliebig viele Finger, Verschieben und Zoomen in einem.
#[derive(Default)]
pub struct Touch {
    /// Kontakte: (Nummer, x, y, letztes Lebenszeichen)
    pts: Vec<(usize, f64, f64, Instant)>,
    /// Protokoll der laufenden Geste (für ~/.cache/graphit/gesten.log)
    log: Option<GestureLog>,
    ignored: Vec<usize>,
    /// Zoom und Abstand beim Start + Layout-Punkt unter dem Schwerpunkt
    base: Option<(f64, f64, Pt)>,
    /// letzte Schwerpunkt-Positionen (für die Wischgeschwindigkeit)
    samples: std::collections::VecDeque<(Instant, [f64; 3])>,
    /// Zwei Finger: erst ab deutlicher Abstandsänderung wird gezoomt, vorher nur geschoben
    pinching: bool,
    /// Touch-Updates seit dem letzten Bild noch nicht angewendet
    pending: bool,
    pinch_base: Option<(f64, Pt)>,
}

impl Touch {
    fn centroid(&self) -> (f64, f64) {
        let n = self.pts.len().max(1) as f64;
        (self.pts.iter().map(|p| p.1).sum::<f64>() / n, self.pts.iter().map(|p| p.2).sum::<f64>() / n)
    }
    /// Trend (Änderung pro Sekunde) von Schwerpunkt x, y und Fingerabstand als
    /// Ausgleichsgerade über die letzten `window_ms`; None, wenn zu wenig Daten
    /// vorliegen oder der letzte Stand älter als `fresh_ms` ist (Finger steht).
    fn trend(&self, window_ms: u64, fresh_ms: u64) -> Option<[f64; 3]> {
        let &(t_end, _) = self.samples.back()?;
        if t_end.elapsed() > Duration::from_millis(fresh_ms) {
            return None;
        }
        let pts: Vec<(f64, [f64; 3])> = self
            .samples
            .iter()
            .filter(|s| t_end.duration_since(s.0) <= Duration::from_millis(window_ms))
            .map(|s| (-(t_end.duration_since(s.0).as_secs_f64()), s.1))
            .collect();
        if pts.len() < 3 || -pts[0].0 < 0.012 {
            return None;
        }
        let n = pts.len() as f64;
        let mt = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let var = pts.iter().map(|p| (p.0 - mt).powi(2)).sum::<f64>();
        if var <= 0.0 {
            return None;
        }
        let slope = |k: usize| {
            let m = pts.iter().map(|p| p.1[k]).sum::<f64>() / n;
            pts.iter().map(|p| (p.0 - mt) * (p.1[k] - m)).sum::<f64>() / var
        };
        Some([slope(0), slope(1), slope(2)])
    }

    /// Vorhergesagter Versatz von Schwerpunkt und Fingerabstand in `ahead` Sekunden;
    /// None, wenn der Finger steht (>30 ms ohne Update) oder zu wenig Daten vorliegen.
    fn prediction(&self, ahead: f64) -> Option<[f64; 3]> {
        let &(t_end, _) = self.samples.back()?;
        if t_end.elapsed() > Duration::from_millis(30) {
            return None;
        }
        let pts: Vec<(f64, [f64; 3])> = self
            .samples
            .iter()
            .filter(|s| t_end.duration_since(s.0) <= Duration::from_millis(50))
            .map(|s| (-(t_end.duration_since(s.0).as_secs_f64()), s.1))
            .collect();
        predict_touch(&pts, ahead).or_else(|| {
            // zu wenige Punkte für die Kurve: Ausgleichsgerade
            self.trend(50, 30).map(|v| v.map(|x| (x * ahead).clamp(-PREDICT_MAX, PREDICT_MAX)))
        })
    }

    /// Wischgeschwindigkeit (px/s) über die letzten 80 ms; None, wenn der Finger vor
    /// dem Loslassen stillstand.
    fn fling_velocity(&self) -> Option<(f64, f64)> {
        self.trend(80, 50).map(|v| (v[0], v[1]))
    }

    fn spread(&self) -> f64 {
        if self.pts.len() < 2 {
            return 0.0;
        }
        let (a, b) = (self.pts[0], self.pts[1]);
        (a.1 - b.1).hypot(a.2 - b.2).max(1.0)
    }
}

// ------------------------------------------------------------------------ Widget

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub state: RefCell<Option<State>>,
        pub hadj: RefCell<Option<gtk::Adjustment>>,
        pub vadj: RefCell<Option<gtk::Adjustment>>,
        pub syncing: Cell<bool>,
        pub settle: RefCell<Option<glib::SourceId>>,
        pub hold_timer: RefCell<Option<glib::SourceId>>,
        pub listeners: RefCell<Vec<Box<dyn Fn(&super::Canvas)>>>,
        /// Stift-Messung (GRAPHIT_PENLOG): Daten des laufenden Strichs
        pub penlog: RefCell<Option<PenLog>>,
        /// Stift-Messung: letzte Schwebe-Punkte vor dem Aufsetzen (Zeit, Druck)
        pub pen_hover: RefCell<std::collections::VecDeque<(u32, Option<f64>)>>,
        /// Listener-Aufruf ist schon eingeplant (höchstens einmal pro Durchlauf)
        pub notify_pending: Cell<bool>,
        pub touch: RefCell<Touch>,
        pub kinetic: RefCell<Option<gtk::TickCallbackId>>,
        pub cursor_key: RefCell<String>,
        pub request: RefCell<Option<Box<dyn Fn(&super::Canvas, Request)>>>,
        pub text_source: RefCell<Option<Box<dyn Fn() -> String>>>,
        pub thumb_listeners: RefCell<Vec<Box<dyn Fn(&super::Canvas)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "FkCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let (pool, rx) = Pool::new(std::thread::available_parallelism().map(|n| n.get().clamp(2, 4)).unwrap_or(2));
            self.state.replace(Some(State::new(pool)));
            obj.set_focusable(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.update_cursor();

            // fertige Kacheln vom Render-Pool übernehmen
            let weak = obj.downgrade();
            glib::spawn_future_local(async move {
                while let Ok(done) = rx.recv().await {
                    let Some(c) = weak.upgrade() else { break };
                    let t0 = Instant::now();
                    let mut thumbs = done.key.kind == Kind::Thumb;
                    c.st().cache.insert(done);
                    // weitere fertige Kacheln gleich mitnehmen, dann einmal zeichnen
                    while let Ok(more) = rx.try_recv() {
                        thumbs |= more.key.kind == Kind::Thumb;
                        c.st().cache.insert(more);
                    }
                    c.queue_draw();
                    if thumbs {
                        c.thumbs_updated();
                    }
                    if std::env::var("GRAPHIT_PROFILE").is_ok() {
                        eprintln!("PROFILE Kacheln übernommen in {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            });

            let hadj = gtk::Adjustment::new(0.0, 0.0, 1.0, 20.0, 200.0, 1.0);
            let vadj = gtk::Adjustment::new(0.0, 0.0, 1.0, 20.0, 200.0, 1.0);
            for (adj, vertical) in [(&hadj, false), (&vadj, true)] {
                let weak = obj.downgrade();
                adj.connect_value_changed(move |a| {
                    let Some(c) = weak.upgrade() else { return };
                    if c.imp().syncing.get() {
                        return;
                    }
                    {
                        let mut st = c.st();
                        let z = st.zoom;
                        if vertical { st.cam.y = a.value() / z } else { st.cam.x = a.value() / z }
                    }
                    c.changed();
                });
            }
            self.hadj.replace(Some(hadj));
            self.vadj.replace(Some(vadj));
            obj.setup_input();
        }
    }

    impl WidgetImpl for Canvas {
        fn size_allocate(&self, w: i32, h: i32, b: i32) {
            self.parent_size_allocate(w, h, b);
            let obj = self.obj();
            let fitted = {
                let mut st = obj.st();
                let width_changed = (st.view.0 - w as f64).abs() > 1.0;
                st.view = (w as f64, h as f64);
                if st.auto_fit && width_changed && !st.needs_fit && w > 50 {
                    // Seitenleiste/Viewer auf- oder zugeklappt: Seitenbreite halten
                    st.fit_width();
                    st.zoom = st.zoom.min(1.6 * ZOOM_100);
                    st.cam.x = -st.view.0 / st.zoom / 2.0;
                    st.render_scale = st.zoom * st.sf;
                }
                st.sf = obj.scale_factor() as f64;
                let fitted = st.needs_fit && w > 50;
                if fitted {
                    st.needs_fit = false;
                    st.fit_width();
                    st.zoom = st.zoom.min(1.6 * ZOOM_100);
                    st.cam.x = -st.view.0 / st.zoom / 2.0;
                    st.render_scale = st.zoom * st.sf;
                    st.cam.y = 0.0;
                    if let Some(p) = std::env::var("GRAPHIT_START_PAGE").ok().and_then(|v| v.parse::<usize>().ok()) {
                        st.scroll_to_page(p.saturating_sub(1));
                    } else {
                        st.apply_restore();
                    }
                }
                st.clamp_cam();
                fitted
            };
            obj.sync_adjustments();
            if fitted {
                // Listener (Titel, Zoomanzeige) außerhalb des Layout-Durchlaufs informieren
                let weak = obj.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(c) = weak.upgrade() {
                        c.changed();
                    }
                });
            }
        }

        fn snapshot(&self, snap: &gtk::Snapshot) {
            self.obj().draw(snap);
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    pub fn new() -> Canvas {
        glib::Object::new()
    }

    pub fn st(&self) -> RefMut<'_, State> {
        RefMut::map(self.imp().state.borrow_mut(), |s| s.as_mut().unwrap())
    }

    pub fn hadjustment(&self) -> gtk::Adjustment {
        self.imp().hadj.borrow().clone().unwrap()
    }
    pub fn vadjustment(&self) -> gtk::Adjustment {
        self.imp().vadj.borrow().clone().unwrap()
    }

    /// Wird nach jeder Zustandsänderung aufgerufen (Titel, Seitenzahl, Zoom, ...).
    pub fn connect_changed(&self, f: impl Fn(&Canvas) + 'static) {
        self.imp().listeners.borrow_mut().push(Box::new(f));
    }

    pub fn changed(&self) {
        self.st().clamp_cam();
        self.sync_adjustments();
        self.update_cursor();
        self.queue_draw();
        // Titel, Tabs, Seitenleiste & Co. nur einmal nachziehen, auch wenn pro Bild
        // viele Änderungen ankommen (Touch, Schwung, Zoom)
        if !self.imp().notify_pending.replace(true) {
            let weak = self.downgrade();
            glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                if let Some(c) = weak.upgrade() {
                    c.imp().notify_pending.set(false);
                    for l in c.imp().listeners.borrow().iter() {
                        l(&c);
                    }
                }
                glib::ControlFlow::Break
            });
        }
    }

    fn sync_adjustments(&self) {
        let (h, v) = {
            let st = self.st();
            let (x0, x1, y0, y1) = st.cam_bounds();
            let z = st.zoom;
            (
                (x0 * z, x1 * z + st.view.0, st.view.0, st.cam.x * z),
                (y0 * z, y1 * z + st.view.1, st.view.1, st.cam.y * z),
            )
        };
        let imp = self.imp();
        imp.syncing.set(true);
        for (adj, (lo, hi, page, val)) in [(self.hadjustment(), h), (self.vadjustment(), v)] {
            adj.configure(val, lo, hi, 40.0, page * 0.9, page);
        }
        imp.syncing.set(false);
    }

    pub fn set_document(&self, doc: Document) {
        self.st().set_document(doc);
        self.changed();
    }

    /// Anderes Hintergrund-PDF verwenden (Seiten und Notizen bleiben).
    pub fn set_pdf_path(&self, p: std::path::PathBuf) {
        {
            let mut st = self.st();
            st.doc.pdf_path = Some(p);
            st.cache.clear();
            st.pending.clear();
            st.text_cache.clear();
            st.search.clear();
            st.last_submit = 0;
        }
        self.changed();
    }

    /// Leseposition speichern, falls sie sich geändert hat.
    pub fn remember_position(&self) {
        let (key, pos) = {
            let st = self.st();
            let (Some(k), Some(p)) = (st.doc_key(), st.position()) else { return };
            if st.remembered == Some(p) {
                return;
            }
            (k, p)
        };
        self.st().remembered = Some(pos);
        crate::positions::set(&key, pos);
    }

    /// Zoomstufe ändern; die Kacheln werden erst nach kurzer Ruhe neu gerastert,
    /// bis dahin skaliert die GPU die vorhandenen.
    pub fn zoom_at(&self, z: f64, ax: f64, ay: f64) {
        self.st().set_zoom_at(z, ax, ay);
        self.schedule_settle();
        self.changed();
    }

    pub fn zoom_by(&self, f: f64) {
        let (z, w, h) = {
            let st = self.st();
            (st.zoom, st.view.0, st.view.1)
        };
        self.zoom_at(z * f, w / 2.0, h / 2.0);
    }

    pub fn fit_width(&self) {
        {
            let mut st = self.st();
            st.fit_width();
            st.auto_fit = true;
        }
        self.changed();
    }

    fn schedule_settle(&self) {
        if let Some(id) = self.imp().settle.take() {
            id.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(Duration::from_millis(140), move || {
            if let Some(c) = weak.upgrade() {
                c.imp().settle.take();
                {
                    let mut st = c.st();
                    st.render_scale = st.zoom * st.sf;
                }
                c.queue_draw();
            }
        });
        self.imp().settle.replace(Some(id));
    }

    pub fn undo(&self) {
        self.commit_edits();
        if self.st().undo() {
            self.changed();
        }
    }

    pub fn redo(&self) {
        self.commit_edits();
        if self.st().redo() {
            self.changed();
        }
    }

    /// Neue Einstellungen übernehmen und speichern.
    pub fn apply_settings(&self, cfg: Settings) {
        let classic = cfg.ink_style == crate::settings::InkStyle::Classic;
        cfg.save();
        {
            let mut st = self.st();
            if st.cfg.eraser_whole != cfg.eraser_whole {
                st.tools.eraser_mode = if cfg.eraser_whole { EraserMode::Stroke } else { EraserMode::Standard };
            }
            st.cfg = cfg;
            if render::CLASSIC_INK.swap(classic, std::sync::atomic::Ordering::Relaxed) != classic {
                st.cache.clear();
                st.pending.clear();
            }
        }
        self.imp().cursor_key.replace(String::new());
        self.changed();
    }

    pub fn set_tool(&self, t: Tool) {
        // Radierer behält eine Auswahl mit Bildern: darin wird dann radiert
        let keep = t == Tool::Select
            || (t == Tool::Eraser
                && self.st().sel.as_ref().is_some_and(|s| s.entries.iter().any(|e| matches!(&*e.2, Item::Image(_)))));
        if !keep {
            self.commit_selection();
        }
        self.commit_text();
        {
            let mut st = self.st();
            if t == Tool::Ask && st.tools.tool != Tool::Ask {
                st.before_ask = Some(st.tools.tool);
            }
            st.tools.tool = t;
        }
        self.update_cursor();
        self.changed();
    }

    /// Cursor wie auf dem iPad: beim Schweben ein Punkt in Stiftfarbe und -größe
    /// (zeigt, wo die Tinte landet), beim Schreiben gar keiner. Der Cursor wird vom
    /// Compositor gezeichnet und kostet daher keinen Frame.
    /// Maus/Touchpad oder Stift wurde zuletzt benutzt; passt den Cursor bei einem Wechsel an.
    fn set_mouse_pointer(&self, on: bool) {
        let changed = {
            let mut st = self.st();
            std::mem::replace(&mut st.mouse_pointer, on) != on
        };
        if changed {
            self.update_cursor();
        }
    }

    pub fn update_cursor(&self) {
        let (key, dot) = {
            let st = self.st();
            if st.mouse_pointer {
                let name = match st.tools.tool {
                    Tool::Hand => "grab",
                    Tool::Text => "text",
                    _ => "default",
                };
                (name.to_string(), None)
            } else if matches!(st.action, Some(Action::Draw(_))) {
                ("none".to_string(), None)
            } else {
                match st.tools.tool {
                    Tool::Pen | Tool::Highlighter | Tool::Shape if st.cfg.cursor == CursorStyle::Crosshair => ("crosshair".into(), None),
                    Tool::Pen | Tool::Highlighter | Tool::Shape if st.cfg.cursor == CursorStyle::None => ("none".into(), None),
                    Tool::Pen | Tool::Highlighter | Tool::Shape => {
                        let p = if st.tools.tool == Tool::Highlighter { &st.tools.highlighter } else { &st.tools.pen };
                        let d = (p.width * st.zoom).clamp(3.0, 48.0).round();
                        let hl = st.tools.tool == Tool::Highlighter;
                        (format!("dot{}{}{}", p.color.to_hex(), d, hl), Some((p.color, d, hl)))
                    }
                    Tool::Hand => ("grab".into(), None),
                    Tool::Text => ("text".into(), None),
                    Tool::Eraser => ("none".into(), None),
                    _ => ("default".into(), None),
                }
            }
        };
        if *self.imp().cursor_key.borrow() == key {
            return;
        }
        let cursor = match dot {
            Some((c, d, hl)) => dot_cursor(c, d, hl),
            None => gdk::Cursor::from_name(&key, None),
        };
        self.set_cursor(cursor.as_ref());
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE Cursor: {key}");
        }
        self.imp().cursor_key.replace(key);
    }

    // ---------------------------------------------------------------- Zeichnen

    fn draw(&self, snap: &gtk::Snapshot) {
        let t0 = Instant::now();
        self.draw_inner(snap);
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let up = crate::tiles::UPLOADS.swap(0, std::sync::atomic::Ordering::Relaxed);
            let busy = crate::tiles::BUSY.load(std::sync::atomic::Ordering::Relaxed);
            eprintln!("PROFILE draw {ms:.2} ms, neue Kacheln: {up}, Worker beschäftigt: {busy}");
        }
    }

    fn draw_inner(&self, snap: &gtk::Snapshot) {
        let dark_ui = adw::StyleManager::default().is_dark();
        let mut st = self.st();
        st.cache.frame += 1;
        let (w, h) = st.view;
        let bg = if dark_ui { Color::rgb(0.14, 0.14, 0.15) } else { Color::rgb(0.87, 0.87, 0.88) };
        snap.append_color(&bg.to_gdk(), &graphene::Rect::new(0.0, 0.0, w as f32, h as f32));

        let vis = st.view_layout();
        let pre = vis.expand(vis.height() * 0.5);
        let mut jobs: Vec<(f64, Job)> = Vec::new();
        for i in 0..st.doc.pages.len() {
            let lr = st.page_layout_rect(i);
            if !lr.intersects(&pre) {
                continue;
            }
            let is_pdf = st.doc.pages[i].borrow().bg.is_pdf() && st.doc.pdf_path.is_some();
            if !lr.intersects(&vis) {
                // nur vorladen
                let dummy = gtk::Snapshot::new();
                if is_pdf {
                    st.snapshot_tiles(&dummy, i, Kind::Pdf, &Rect::new(0., 0., -1., -1.), &pre, &mut jobs);
                }
                let ink = st.ink_kind();
                st.snapshot_tiles(&dummy, i, ink, &Rect::new(0., 0., -1., -1.), &pre, &mut jobs);
                continue;
            }
            let sr = st.to_screen_rect(&lr);
            let rr = gsk::RoundedRect::from_rect(sr, 2.0);
            snap.append_outset_shadow(&rr, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25), 0.0, 1.0, 0.0, 6.0);
            snap.push_clip(&sr);
            // Seitenfläche und PDF über den GPU-Farbfilter; die Inhaltskacheln sind
            // für dunkles Papier schon umgefärbt (Fotos bleiben so unverändert)
            if st.dark_paper {
                let (m, o) = dark_matrix();
                snap.push_color_matrix(&m, &o);
            }
            snap.append_color(&gdk::RGBA::WHITE, &sr);
            let vis_page = vis.intersect_or_empty(&lr);
            if is_pdf {
                st.snapshot_tiles(snap, i, Kind::Pdf, &vis_page, &pre, &mut jobs);
            }
            if st.dark_paper {
                snap.pop();
            }
            let ink = st.ink_kind();
            st.snapshot_tiles(snap, i, ink, &vis_page, &pre, &mut jobs);
            snap.pop();
        }

        // gerade abgeschlossene und aktive Striche: im Dunkelmodus durch denselben
        // Farbfilter wie die Kacheln, sonst wechselt die Farbe beim Loslassen
        if st.dark_paper {
            let (m, o) = dark_matrix();
            snap.push_color_matrix(&m, &o);
        }
        // gerade abgeschlossene Striche, bis ihre Kacheln da sind
        let pending = std::mem::take(&mut st.pending);
        let mut keep = Vec::new();
        for p in pending {
            if st.pending_done(&p) {
                continue;
            }
            if p.alpha < 1.0 {
                snap.push_opacity(p.alpha);
            }
            for (tex, r) in &p.tex {
                snap.append_texture(tex, &st.to_screen_rect(r));
            }
            if p.alpha < 1.0 {
                snap.pop();
            }
            keep.push(p);
        }
        st.pending = keep;

        // aktiver Strich
        if let Some(Action::Draw(live)) = &st.action {
            let a = live.color.a;
            if a < 1.0 {
                snap.push_opacity(a);
            }
            for (tex, r) in live.all_textures().chain(live.pred.iter()) {
                snap.append_texture(tex, &st.to_screen_rect(r));
            }
            if a < 1.0 {
                snap.pop();
            }
        }
        if st.dark_paper {
            snap.pop();
        }

        self.draw_overlays(&mut st, snap, dark_ui);

        // Radierer-Cursor
        let image_erase = matches!(st.action, Some(Action::ImageErase { .. }));
        if st.tools.tool == Tool::Eraser || image_erase || matches!(st.action, Some(Action::Erase { .. })) {
            if let Some((hx, hy)) = st.hover {
                let r = if st.tools.eraser_mode == EraserMode::Stroke && !image_erase {
                    3.0
                } else {
                    (st.tools.eraser_size * st.zoom) as f32
                };
                let rect = graphene::Rect::new(hx as f32 - r, hy as f32 - r, 2.0 * r, 2.0 * r);
                let rr = gsk::RoundedRect::from_rect(rect, r);
                let c = if dark_ui { gdk::RGBA::new(1.0, 1.0, 1.0, 0.8) } else { gdk::RGBA::new(0.0, 0.0, 0.0, 0.7) };
                snap.append_border(&rr, &[1.0; 4], &[c, c, c, c]);
            }
        }

        st.thumb_jobs(&mut jobs);
        // Jobs nach Priorität einreihen (nur wenn sich etwas geändert hat)
        jobs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut hs = std::collections::hash_map::DefaultHasher::new();
            for (_, j) in &jobs {
                j.key.hash(&mut hs);
                j.ver.hash(&mut hs);
            }
            hs.finish()
        };
        if sig != st.last_submit {
            st.last_submit = sig;
            let jobs = jobs.into_iter().map(|j| j.1).collect();
            st.pool.submit(jobs);
        }
    }

    // ---------------------------------------------------------------- Eingabe

    fn setup_input(&self) {
        let legacy = gtk::EventControllerLegacy::new();
        let weak = self.downgrade();
        legacy.connect_event(move |_, ev| match weak.upgrade() {
            Some(c) => c.handle_event(ev),
            None => glib::Propagation::Proceed,
        });
        self.add_controller(legacy);

        // Mausrad / Touchpad
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = self.downgrade();
        scroll.connect_scroll(move |ctl, dx, dy| {
            let Some(c) = weak.upgrade() else { return glib::Propagation::Proceed };
            let ctrl = ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            let wheel = ctl.unit() == gdk::ScrollUnit::Wheel;
            if ctrl {
                let (z, (hx, hy)) = {
                    let st = c.st();
                    (st.zoom, st.hover.unwrap_or((st.view.0 / 2.0, st.view.1 / 2.0)))
                };
                let f = if wheel { 1.15f64.powf(-dy) } else { (1.0 - dy * 0.01).clamp(0.5, 2.0) };
                c.zoom_at(z * f, hx, hy);
            } else {
                {
                    let mut st = c.st();
                    let step = if wheel { 70.0 } else { 1.0 };
                    let z = st.zoom;
                    let shift = ctl.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
                    let (dx, dy) = if shift && wheel { (dy, dx) } else { (dx, dy) };
                    st.cam.x += dx * step / z;
                    st.cam.y += dy * step / z;
                }
                c.changed();
            }
            glib::Propagation::Stop
        });
        self.add_controller(scroll);

        let motion = gtk::EventControllerMotion::new();
        let weak = self.downgrade();
        motion.connect_motion(move |_, x, y| {
            if let Some(c) = weak.upgrade() {
                let redraw = {
                    let mut st = c.st();
                    st.hover = Some((x, y));
                    if let Some(r) = &mut st.radial {
                        r.hover = edit::radial_hit(r.center, (x, y));
                    }
                    st.tools.tool == Tool::Eraser || st.radial.is_some()
                };
                if redraw {
                    c.queue_draw();
                }
            }
        });
        let weak = self.downgrade();
        motion.connect_leave(move |_| {
            if let Some(c) = weak.upgrade() {
                c.st().hover = None;
                c.queue_draw();
            }
        });
        self.add_controller(motion);

    }

    fn handle_touch(&self, ev: &gdk::Event) {
        use gdk::EventType as E;
        use glib::translate::ToGlibPtr;
        let seq = ev.event_sequence();
        let id = {
            let p: *const gdk::ffi::GdkEventSequence = seq.to_glib_none().0;
            p as usize
        };
        let Some((x, y)) = ev.position().and_then(|(x, y)| self.event_pos(ev, x, y)) else { return };
        let et = ev.event_type();
        if et == E::TouchBegin {
            // GTK vergibt die Nummern unter Wayland immer wieder neu (erster Finger = 0).
            // Gibt es die Nummer noch, ist ihr "Finger hoch" verloren gegangen: weg damit,
            // sonst bleibt ein Geisterfinger stehen und jedes Tippen wird zum Zoom.
            // Ebenso Kontakte, die seit langem kein Lebenszeichen mehr gegeben haben.
            let now = Instant::now();
            let dropped = {
                let mut t = self.imp().touch.borrow_mut();
                let before = t.pts.len();
                t.pts.retain(|p| p.0 != id && now.duration_since(p.3) < STALE_TOUCH);
                t.ignored.retain(|v| *v != id);
                if t.pts.is_empty() {
                    t.base = None;
                    t.pinching = false;
                }
                before - t.pts.len()
            };
            // Stift aktiv, oder schon zwei Finger da (dritter Kontakt = meist Handballen)
            let reason = if self.palm_blocked() {
                Some("Handballen-Sperre (Stift nah)".to_string())
            } else if let Some(a) = self.st().action.as_ref() {
                Some(format!("laufende Aktion: {}", a.name()))
            } else {
                let t = self.imp().touch.borrow();
                (t.pts.len() >= 2).then(|| {
                    let ages: Vec<String> = t.pts.iter().map(|p| format!("{} ms", now.duration_since(p.3).as_millis())).collect();
                    format!("schon {} Kontakte, zuletzt bewegt vor {}", t.pts.len(), ages.join(" / "))
                })
            };
            if let Some(r) = reason {
                GestureLog::ignored(&r);
                self.imp().touch.borrow_mut().ignored.push(id);
                return;
            }
            self.stop_kinetic();
            let mut t = self.imp().touch.borrow_mut();
            if t.pts.is_empty() {
                t.log = Some(GestureLog::start(self.st().zoom));
            }
            if dropped > 0 {
                if let Some(l) = &mut t.log {
                    l.flag("geist-entfernt");
                }
            }
            t.pts.push((id, x, y, now));
            let n = t.pts.len();
            if let Some(l) = &mut t.log {
                l.contacts = l.contacts.max(n);
            }
            t.samples.clear();
            drop(t);
            self.reset_touch_base();
            return;
        }
        {
            let mut t = self.imp().touch.borrow_mut();
            if let Some(i) = t.ignored.iter().position(|v| *v == id) {
                if et != E::TouchUpdate {
                    t.ignored.remove(i);
                }
                return;
            }
        }
        match et {
            E::TouchUpdate => {
                // nur Position merken; angewendet wird einmal pro Bild (GTK 4.14 fasst
                // Touch-Updates nicht zusammen, ein Digitizer liefert mehrere pro Bild)
                let mut t = self.imp().touch.borrow_mut();
                if let Some(p) = t.pts.iter_mut().find(|p| p.0 == id) {
                    p.1 = x;
                    p.2 = y;
                    p.3 = Instant::now();
                }
                let (cx, cy) = t.centroid();
                let spread = t.spread();
                let now = Instant::now();
                // Messzeit des Touchscreens statt Verarbeitungszeit: GTK liefert Updates
                // gebündelt, mit der Ankunftszeit lägen mehrere Punkte fast zeitgleich
                // und die Vorhersage würde zittern
                let at = now - Duration::from_millis(PenLog::delay(ev).clamp(0, 100) as u64);
                // gleiche Messung (zweiter Finger desselben Abtastschritts): nur ersetzen,
                // sonst springt der Schwerpunkt zwischen halb und ganz aktualisiert hin und her
                match t.samples.back_mut() {
                    Some(last) if at.saturating_duration_since(last.0) < Duration::from_micros(1500) => {
                        *last = (last.0.max(at), [cx, cy, spread]);
                    }
                    _ => t.samples.push_back((at, [cx, cy, spread])),
                }
                while t.samples.front().map(|s| now.duration_since(s.0) > Duration::from_millis(150)).unwrap_or(false) {
                    t.samples.pop_front();
                }
                if !t.pending {
                    t.pending = true;
                    drop(t);
                    self.add_tick_callback(|c, _| {
                        c.apply_touch(true);
                        glib::ControlFlow::Break
                    });
                }
            }
            _ => {
                // Schwung? (nur ein Finger, schnell genug gewischt)
                let fling = {
                    let t = self.imp().touch.borrow();
                    let cfg = self.st().cfg.clone();
                    let known = t.pts.iter().any(|p| p.0 == id);
                    match (known && t.pts.len() == 1 && cfg.kinetic, t.fling_velocity()) {
                        (true, Some(mut v)) => {
                            // überwiegend senkrecht gewischt: seitliches Driften weglassen
                            if cfg.kinetic_axis_lock {
                                if v.1.abs() > 2.0 * v.0.abs() {
                                    v.0 = 0.0;
                                } else if v.0.abs() > 2.0 * v.1.abs() {
                                    v.1 = 0.0;
                                }
                            }
                            let v = (v.0 * cfg.kinetic_boost, v.1 * cfg.kinetic_boost);
                            let speed = v.0.hypot(v.1);
                            (speed > 220.0).then(|| {
                                let k = (12000.0 / speed).min(1.0);
                                ((v.0 * k, v.1 * k), cfg.kinetic_decay())
                            })
                        }
                        _ => None,
                    }
                };
                // ohne Schwung: auf die echte Fingerposition zurück (Vorhersage weg),
                // mit Schwung läuft die Seite ohnehin in Wischrichtung weiter
                if fling.is_none() {
                    self.apply_touch(false);
                }
                let empty = {
                    let mut t = self.imp().touch.borrow_mut();
                    t.pts.retain(|p| p.0 != id);
                    t.pts.is_empty()
                };
                if empty {
                    let log = self.imp().touch.borrow_mut().log.take();
                    if let Some(mut log) = log {
                        log.fling = fling.is_some();
                        log.finish(self.st().zoom);
                    }
                }
                if empty {
                    self.imp().touch.borrow_mut().base = None;
                    if let Some((v, decay)) = fling {
                        self.start_kinetic(v, decay);
                    }
                } else {
                    self.reset_touch_base();
                }
            }
        }
    }

    /// Fingerstand auf Kamera und Zoom anwenden (einmal pro Bild). Mit `predict` wird
    /// die Bewegung um die eingestellte Zeit vorausgerechnet (wie beim Stift), damit
    /// die Seite nicht hinter dem Finger herzieht.
    fn apply_touch(&self, predict: bool) {
        let ahead = if predict { self.st().cfg.touch_predict_ms / 1000.0 } else { 0.0 };
        let (old_zoom, old_cam) = {
            let st = self.st();
            (st.zoom, st.cam)
        };
        let (zoom, cam, zoomed) = {
            let mut t = self.imp().touch.borrow_mut();
            t.pending = false;
            let Some((z0, d0, anchor)) = t.base else { return };
            let (mut cx, mut cy) = t.centroid();
            let finger = (cx, cy);
            // Vorhersage aus den letzten 50 ms; steht der Finger, nichts vorhersagen
            let pred = if ahead > 0.0 { t.prediction(ahead) } else { None };
            if let Some(v) = pred {
                cx += v[0];
                cy += v[1];
            }
            let mut zoom = z0;
            let small = d0 < MIN_PINCH_SPAN;
            if t.pts.len() >= 2 && small {
                if let Some(l) = &mut t.log {
                    l.flag("abstand-zu-klein");
                }
            }
            if t.pts.len() >= 2 && !small {
                let s = t.spread();
                if !t.pinching && (s - d0).abs() > PINCH_SLOP {
                    // ab hier zoomen, nahtlos vom aktuellen Abstand aus
                    t.pinching = true;
                    t.base = Some((z0, s, anchor));
                }
                if t.pinching {
                    let d = t.base.map(|b| b.1).unwrap_or(d0);
                    let s = s + pred.map(|v| v[2]).unwrap_or(0.0);
                    zoom = (z0 * s.max(1.0) / d).clamp(MIN_ZOOM, MAX_ZOOM);
                }
            }
            // unmöglich schnelle Zoomsprünge (Messfehler, Geisterkontakt) abfangen
            let cur = self.st().zoom;
            if zoom > cur * MAX_ZOOM_STEP || zoom < cur / MAX_ZOOM_STEP {
                zoom = zoom.clamp(cur / MAX_ZOOM_STEP, cur * MAX_ZOOM_STEP);
                if let Some(l) = &mut t.log {
                    l.flag("zoom-gebremst");
                }
            }
            let span0 = t.pinching.then(|| t.base.map(|b| b.1).unwrap_or(d0));
            let cam = Pt::new(anchor.x - cx / zoom, anchor.y - cy / zoom);
            let n = t.pts.len();
            if let Some(l) = &mut t.log {
                l.track(zoom, (cx, cy), span0);
                // Bewegung der Ansicht: Inhaltspunkt unter dem Finger, alt gegen neu
                let p = Pt::new(old_cam.x + finger.0 / old_zoom, old_cam.y + finger.1 / old_zoom);
                let (sx, sy) = ((p.x - cam.x) * zoom, (p.y - cam.y) * zoom);
                let step = (sx - finger.0).hypot(sy - finger.1);
                l.frame(n, finger, pred.unwrap_or_default(), zoom, step, !predict);
            }
            (zoom, cam, (zoom - z0).abs() > 1e-9)
        };
        {
            let mut st = self.st();
            if zoomed {
                st.auto_fit = false;
            }
            st.zoom = zoom;
            st.cam = cam;
        }
        if zoomed {
            self.schedule_settle();
        }
        self.changed();
    }

    fn reset_touch_base(&self) {
        let (z, l) = {
            let t = self.imp().touch.borrow();
            let (cx, cy) = t.centroid();
            let st = self.st();
            (st.zoom, st.to_layout(cx, cy))
        };
        let mut t = self.imp().touch.borrow_mut();
        let d = t.spread();
        t.base = Some((z, d, l));
        t.samples.clear();
        t.pinching = false;
    }

    /// Schwung nach dem Wischen; läuft nur, bis er ausgebremst ist (spart Akku).
    /// Schwung nach dem Wischen; `decay` = Abklingrate pro Sekunde.
    fn start_kinetic(&self, vel: (f64, f64), decay: f64) {
        self.stop_kinetic();
        let v = Cell::new(vel);
        let last = Cell::new(None::<i64>);
        let id = self.add_tick_callback(move |c, clock| {
            let now = clock.frame_time();
            let dt = last.replace(Some(now)).map(|l| (now - l) as f64 / 1e6).unwrap_or(1.0 / 60.0).min(0.05);
            let (vx, vy) = v.get();
            let f = (-dt * decay).exp();
            v.set((vx * f, vy * f));
            {
                let mut st = c.st();
                let z = st.zoom;
                st.cam.x -= vx * dt / z;
                st.cam.y -= vy * dt / z;
            }
            c.changed();
            if vx.hypot(vy) < 20.0 {
                c.imp().kinetic.take();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        self.imp().kinetic.replace(Some(id));
    }

    fn stop_kinetic(&self) {
        if let Some(id) = self.imp().kinetic.take() {
            id.remove();
        }
    }

    /// Zwei-Finger-Zoom auf dem Touchpad.
    fn handle_pinch(&self, ev: &gdk::Event) {
        let Some(tp) = ev.downcast_ref::<gdk::TouchpadEvent>() else { return };
        let (x, y) = ev.position().and_then(|(x, y)| self.event_pos(ev, x, y)).unwrap_or_else(|| {
            let st = self.st();
            (st.view.0 / 2.0, st.view.1 / 2.0)
        });
        match tp.gesture_phase() {
            gdk::TouchpadGesturePhase::Begin => {
                let st = self.st();
                let base = (st.zoom, st.to_layout(x, y));
                drop(st);
                self.imp().touch.borrow_mut().pinch_base = Some(base);
            }
            gdk::TouchpadGesturePhase::Update => {
                let Some((z0, anchor)) = self.imp().touch.borrow().pinch_base else { return };
                {
                    let mut st = self.st();
                    st.auto_fit = false;
                    st.zoom = (z0 * tp.pinch_scale()).clamp(MIN_ZOOM, MAX_ZOOM);
                    let z = st.zoom;
                    st.cam = Pt::new(anchor.x - x / z, anchor.y - y / z);
                }
                self.schedule_settle();
                self.changed();
            }
            _ => self.imp().touch.borrow_mut().pinch_base = None,
        }
    }

    /// Handballen-Erkennung: Touch wird ignoriert, solange der Stift nah ist.
    fn palm_blocked(&self) -> bool {
        let st = self.st();
        let recent = |t: Option<Instant>, ms| t.map(|t| t.elapsed() < Duration::from_millis(ms)).unwrap_or(false);
        match st.cfg.palm {
            PalmMode::Off => false,
            // nur sperren, solange der Stift aufliegt
            PalmMode::Contact => st.action.as_ref().map(|a| !matches!(a, Action::Pan { .. } | Action::Radial { .. })).unwrap_or(false),
            // ProximityOut kommt nicht auf jedem Gerät an - daher zeitlich begrenzen
            PalmMode::Proximity => (st.pen_near && recent(st.last_pen, 1500)) || recent(st.last_pen, 350),
        }
    }

    fn event_pos(&self, ev: &gdk::Event, x: f64, y: f64) -> Option<(f64, f64)> {
        let native = self.native()?;
        let (sx, sy) = native.surface_transform();
        let p = native.compute_point(self, &graphene::Point::new((x - sx) as f32, (y - sy) as f32))?;
        let _ = ev;
        Some((p.x() as f64, p.y() as f64))
    }

    fn samples(&self, ev: &gdk::Event, stylus: bool) -> Vec<Sample> {
        let mut out = Vec::new();
        let px = gdk::AxisUse::X.into_glib() as usize;
        let py = gdk::AxisUse::Y.into_glib() as usize;
        let pp = gdk::AxisUse::Pressure.into_glib() as usize;
        let (tx, ty) = (gdk::AxisUse::Xtilt.into_glib() as usize, gdk::AxisUse::Ytilt.into_glib() as usize);
        // GDK liefert Neigung je nach Backend in Grad oder normiert (−1…1)
        let tilt_of = |x: Option<f64>, y: Option<f64>| -> f64 {
            let (Some(x), Some(y)) = (x, y) else { return 0.0 };
            let k = if x.abs() <= 1.0001 && y.abs() <= 1.0001 { 90.0 } else { 1.0 };
            (x * k).hypot(y * k).min(90.0)
        };
        if ev.event_type() == gdk::EventType::MotionNotify {
            for tc in ev.history() {
                let f = tc.flags();
                if !(f.contains(gdk::AxisFlags::X) && f.contains(gdk::AxisFlags::Y)) {
                    continue;
                }
                let a = tc.axes();
                if let Some((x, y)) = self.event_pos(ev, a[px], a[py]) {
                    let pressure = (stylus && f.contains(gdk::AxisFlags::PRESSURE)).then(|| a[pp]);
                    let tilt = if stylus && f.contains(gdk::AxisFlags::XTILT) { tilt_of(Some(a[tx]), Some(a[ty])) } else { 0.0 };
                    out.push(Sample { x, y, pressure, tilt });
                }
            }
        }
        if let Some((x, y)) = ev.position().and_then(|(x, y)| self.event_pos(ev, x, y)) {
            let pressure = if stylus { ev.axis(gdk::AxisUse::Pressure) } else { None };
            let tilt = if stylus { tilt_of(ev.axis(gdk::AxisUse::Xtilt), ev.axis(gdk::AxisUse::Ytilt)) } else { 0.0 };
            out.push(Sample { x, y, pressure, tilt });
        }
        out
    }

    fn handle_event(&self, ev: &gdk::Event) -> glib::Propagation {
        use gdk::EventType as E;
        let et = ev.event_type();
        let source = ev.device().map(|d| d.source());
        let tool_type = ev.device_tool().map(|t| t.tool_type());
        let stylus = tool_type.is_some() || matches!(source, Some(gdk::InputSource::Pen));
        match et {
            E::ProximityIn => {
                self.st().pen_near = true;
                self.set_mouse_pointer(false);
                return glib::Propagation::Proceed;
            }
            E::ProximityOut => {
                let mut st = self.st();
                st.pen_near = false;
                st.last_pen = Some(Instant::now());
                st.barrel = 0;
                return glib::Propagation::Proceed;
            }
            E::TouchBegin | E::TouchUpdate | E::TouchEnd | E::TouchCancel => {
                self.handle_touch(ev);
                return glib::Propagation::Stop;
            }
            E::TouchpadPinch => {
                self.handle_pinch(ev);
                return glib::Propagation::Stop;
            }
            _ => {}
        }
        if ev.is_pointer_emulated() || matches!(source, Some(gdk::InputSource::Touchscreen)) {
            return glib::Propagation::Proceed;
        }
        if stylus {
            let mut st = self.st();
            st.last_pen = Some(Instant::now());
            st.pen_near = true;
        }
        if matches!(et, E::MotionNotify | E::ButtonPress) {
            self.set_mouse_pointer(!stylus);
        }
        match et {
            E::ButtonPress => {
                // Stift oder Maus setzt auf: laufenden Schwung sofort anhalten
                self.stop_kinetic();
                let button = ev.downcast_ref::<gdk::ButtonEvent>().map(|b| b.button()).unwrap_or(1);
                self.grab_focus();
                let pressure = ev.axis(gdk::AxisUse::Pressure).unwrap_or(0.0);
                if stylus && (button == 2 || button == 3) && pressure <= 0.0 {
                    // Stifttaste beim Schweben: merken, greift beim Aufsetzen
                    self.st().barrel = button;
                    return glib::Propagation::Stop;
                }
                if self.st().action.is_some() {
                    return glib::Propagation::Stop;
                }
                let mods = ev.modifier_state();
                let barrel = {
                    let st = self.st();
                    if mods.contains(gdk::ModifierType::BUTTON2_MASK) || st.barrel == 2 || (stylus && button == 2) {
                        2
                    } else if mods.contains(gdk::ModifierType::BUTTON3_MASK) || st.barrel == 3 || (stylus && button == 3) {
                        3
                    } else {
                        0
                    }
                };
                let mut tool = self.st().tools.tool;
                let mapped = {
                    let cfg = &self.st().cfg;
                    if tool_type == Some(gdk::DeviceToolType::Eraser) {
                        Some(cfg.eraser_end)
                    } else if stylus && barrel == 2 {
                        Some(cfg.button1)
                    } else if stylus && barrel == 3 {
                        Some(cfg.button2)
                    } else {
                        None
                    }
                };
                if let Some(a) = mapped {
                    use crate::settings::ButtonAction as B;
                    match a {
                        B::Eraser => tool = Tool::Eraser,
                        B::Select => {
                            tool = Tool::Select;
                            self.st().tools.select_mode = SelectMode::Lasso;
                        }
                        B::Hand => tool = Tool::Hand,
                        B::Radial => {
                            if let Some(s) = self.samples(ev, stylus).last() {
                                self.open_radial((s.x, s.y), true);
                            }
                            return glib::Propagation::Stop;
                        }
                        B::Nothing => {}
                    }
                } else if !stylus && button == 2 {
                    tool = Tool::Hand;
                } else if !stylus && button == 3 {
                    if let Some(s) = self.samples(ev, false).last() {
                        self.open_radial((s.x, s.y), true);
                    }
                    return glib::Propagation::Stop;
                }
                let s = self.samples(ev, stylus);
                if stylus && PenLog::enabled() {
                    // Schwebe-Punkte kurz vor dem Aufsetzen: meldet der Stift da schon Druck?
                    let t0 = ev.time();
                    let before = self
                        .imp()
                        .pen_hover
                        .take()
                        .into_iter()
                        .filter(|(t, _)| t0.wrapping_sub(*t) <= 60)
                        .map(|(t, p)| match p {
                            Some(p) => format!("-{}ms:{:.3}", t0.wrapping_sub(t), p),
                            None => format!("-{}ms:–", t0.wrapping_sub(t)),
                        })
                        .collect();
                    let mut log = PenLog { press_pressure: ev.axis(gdk::AxisUse::Pressure).unwrap_or(-1.0), before, ..Default::default() };
                    log.add_event(ev);
                    self.imp().penlog.replace(Some(log));
                }
                if let Some(s) = s.last() {
                    self.begin(tool, *s, stylus);
                }
                glib::Propagation::Stop
            }
            E::MotionNotify => {
                if self.st().action.is_none() {
                    if stylus && PenLog::enabled() {
                        let pp = gdk::AxisUse::Pressure.into_glib() as usize;
                        let mut h = self.imp().pen_hover.borrow_mut();
                        for tc in ev.history() {
                            let p = tc.flags().contains(gdk::AxisFlags::PRESSURE).then(|| tc.axes()[pp]);
                            h.push_back((tc.time(), p));
                        }
                        h.push_back((ev.time(), ev.axis(gdk::AxisUse::Pressure)));
                        while h.len() > 24 {
                            h.pop_front();
                        }
                    }
                    return glib::Propagation::Proceed;
                }
                if let Some(log) = self.imp().penlog.borrow_mut().as_mut() {
                    log.add_event(ev);
                }
                let s = self.samples(ev, stylus);
                self.motion(&s);
                glib::Propagation::Stop
            }
            E::ButtonRelease => {
                let button = ev.downcast_ref::<gdk::ButtonEvent>().map(|b| b.button()).unwrap_or(1);
                if stylus && (button == 2 || button == 3) {
                    self.st().barrel = 0;
                }
                if self.st().action.is_some() {
                    let s = self.samples(ev, stylus);
                    self.end(s.last().copied());
                }
                if let Some(mut log) = self.imp().penlog.take() {
                    log.add_event(ev);
                    log.report();
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

}

fn dot_cursor(c: Color, d: f64, highlighter: bool) -> Option<gdk::Cursor> {
    let size = (d + 4.0).ceil() as i32;
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let cr = cairo::Context::new(&surf).ok()?;
        let m = size as f64 / 2.0;
        let r = d / 2.0;
        if highlighter {
            cr.rectangle(m - r * 0.35, m - r, r * 0.7, 2.0 * r);
        } else {
            cr.arc(m, m, r, 0.0, std::f64::consts::TAU);
        }
        cr.set_source_rgba(c.r, c.g, c.b, if highlighter { 0.7 } else { 1.0 });
        cr.fill_preserve().ok()?;
        // dünner Kontrastrand, damit der Punkt auf jedem Hintergrund sichtbar ist
        let lum = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        if lum < 0.5 { cr.set_source_rgba(1.0, 1.0, 1.0, 0.9) } else { cr.set_source_rgba(0.0, 0.0, 0.0, 0.6) }
        cr.set_line_width(1.0);
        cr.stroke().ok()?;
    }
    let stride = surf.stride() as usize;
    let data = surf.take_data().ok()?;
    let tex = gdk::MemoryTexture::new(size, size, gdk::MemoryFormat::B8g8r8a8Premultiplied, &glib::Bytes::from_owned(data), stride);
    let fallback = gdk::Cursor::from_name("crosshair", None);
    Some(gdk::Cursor::from_texture(&tex, size / 2, size / 2, fallback.as_ref()))
}

/// Farbmatrix für "dunkles Papier": invertiert die Helligkeit, behält den Farbton.
pub(super) fn dark_matrix() -> (graphene::Matrix, graphene::Vec4) {
    let l = [0.2126f32, 0.7152, 0.0722];
    let k = render::DARK_K;
    let mut m = [0f32; 16];
    // graphene: Zeilenvektor * Matrix → m[i*4+j] = Einfluss von Eingang i auf Ausgang j
    for i in 0..3 {
        for j in 0..3 {
            m[i * 4 + j] = k * ((i == j) as i32 as f32 - 2.0 * l[i]);
        }
    }
    m[15] = 1.0;
    let off = render::DARK_OFF;
    (graphene::Matrix::from_float(m), graphene::Vec4::new(off, off, off, 0.0))
}

trait RectExt {
    fn intersect_or_empty(&self, o: &Rect) -> Rect;
}

impl RectExt for Rect {
    fn intersect_or_empty(&self, o: &Rect) -> Rect {
        let r = Rect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1));
        if r.x0 > r.x1 || r.y0 > r.y1 { Rect::new(0.0, 0.0, -1.0, -1.0) } else { r }
    }
}

impl Canvas {
    /// Versteckter Selbsttest (GRAPHIT_SELFTEST=1): simuliert Eingaben für alle Werkzeuge.
    pub fn selftest(&self) {
        let (w, h) = self.st().view;
        let count = |c: &Canvas| -> usize { c.st().doc.pages[0].borrow().layers.iter().map(|l| l.items.len()).sum() };
        let n0 = count(self);
        let t0 = Instant::now();
        let mut n = 0;
        for k in 0..3 {
            let y0 = h * 0.3 + k as f64 * 40.0;
            self.begin(Tool::Pen, Sample { x: w * 0.25, y: y0, pressure: Some(0.3), tilt: 0.0 }, true);
            for i in 0..600 {
                let t = i as f64 / 600.0;
                let s = Sample { x: w * 0.25 + t * w * 0.5, y: y0 + (t * 40.0).sin() * 12.0, pressure: Some(0.3 + 0.6 * (t * 9.0).sin().abs()), tilt: 0.0 };
                self.motion(&[s]);
                n += 1;
            }
            self.end(None);
        }
        let per = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        eprintln!("SELFTEST Stift: {n} Samples, {per:.3} ms/Sample, Elemente {} -> {}", n0, count(self));

        // Lasso um die Wellen, verschieben, skalieren, festlegen
        self.st().tools.select_mode = SelectMode::Lasso;
        let (x0, y0, x1, y1) = (w * 0.2, h * 0.25, w * 0.8, h * 0.3 + 120.0);
        self.begin(Tool::Select, Sample { x: x0, y: y0, pressure: None, tilt: 0.0 }, false);
        for (x, y) in [(x1, y0), (x1, y1), (x0, y1), (x0, y0 + 1.0)] {
            self.motion(&[Sample { x, y, pressure: None, tilt: 0.0 }]);
        }
        self.end(None);
        let nsel = self.st().sel.as_ref().map(|s| s.entries.len()).unwrap_or(0);
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("sel") {
            self.changed();
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("mark") {
            self.commit_selection();
            // "Material Selection" auf Seite 1 mit dem Marker überstreichen
            let (a, b) = {
                let mut st = self.st();
                let text = st.page_text(0).expect("PDF-Text");
                let m = text.find("material selection");
                let r = m.first().and_then(|l| l.first()).copied().expect("Treffer");
                let o = st.page_origin(0);
                let z = st.zoom;
                let scr = |x: f64, y: f64| ((o.x + x - st.cam.x) * z, (o.y + y - st.cam.y) * z);
                (scr(r.x0 + 2.0, r.center().y + 3.0), scr(r.x1 - 2.0, r.center().y - 2.0))
            };
            self.st().last_stroke_end = None;
            self.begin(Tool::Highlighter, Sample { x: a.0, y: a.1, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: (a.0 + b.0) / 2.0, y: a.1, pressure: None, tilt: 0.0 }]);
            std::thread::sleep(Duration::from_millis(120));
            self.motion(&[Sample { x: b.0, y: b.1, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let last = self.st().doc.pages[0].borrow().layers.last().unwrap().items.last().map(|i| match &**i {
                Item::Stroke(s) => format!("{:?} Breite {:.1} Punkte {:?}", s.tool, s.width, s.pts),
                _ => "kein Strich".into(),
            });
            eprintln!("SELFTEST Text-Marker: {last:?}");
            let n = self.search("material");
            eprintln!("SELFTEST Suche 'material': {n} Treffer, Position {:?}", self.search_pos());
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("pdfspace") {
            self.commit_selection();
            // Platz mitten im PDF-Text einfügen (Seite 1), danach speichern
            self.begin(Tool::Space, Sample { x: w * 0.5, y: h * 0.62, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: w * 0.5, y: h * 0.62 + 110.0, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let p = self.st().doc.pages[0].clone();
            eprintln!("SELFTEST PDF-Platz: Höhe {:.0}, Schnitte {:?}", p.borrow().height, p.borrow().bg.pdf_splits());
            let data = crate::xopp::SaveData::from_doc(&self.st().doc);
            let out = std::env::var("GRAPHIT_SAVE_TO").unwrap_or_default();
            if !out.is_empty() {
                eprintln!("SELFTEST gespeichert: {:?}", crate::xopp::save(&data, std::path::Path::new(&out)));
            }
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("pan") {
            // Fingerwisch nachstellen: 4 Touch-Updates pro Bild (wie ein 240-Hz-Digitizer),
            // gemessen werden Zeit pro Update und Bildabstände
            let start = Instant::now();
            let frames: std::rc::Rc<RefCell<Vec<f64>>> = Default::default();
            let upd: std::rc::Rc<RefCell<Vec<f64>>> = Default::default();
            let last = Cell::new(None::<Instant>);
            let (f2, u2) = (frames.clone(), upd.clone());
            self.add_tick_callback(move |c, _| {
                let now = Instant::now();
                if let Some(l) = last.replace(Some(now)) {
                    let ms = now.duration_since(l).as_secs_f64() * 1000.0;
                    if ms > 25.0 && std::env::var("GRAPHIT_PROFILE").is_ok() {
                        eprintln!("PROFILE >>> langes Bild: {ms:.1} ms");
                    }
                    f2.borrow_mut().push(ms);
                }
                let zoom_test = std::env::var("GRAPHIT_PAN_ZOOM").is_ok();
                let secs = start.elapsed().as_secs_f64();
                for _ in 0..4 {
                    let t0 = Instant::now();
                    if zoom_test {
                        // in der ersten Sekunde zoomen (Zwei-Finger-Zoom), dann ruhen lassen:
                        // gemessen wird das Nachrendern in neuer Auflösung
                        if secs < 1.0 {
                            let (z, w, h) = { let st = c.st(); (st.zoom, st.view.0, st.view.1) };
                            c.zoom_at(z * 1.004, w / 2.0, h / 2.0);
                        }
                    } else {
                        c.st().cam.y += 3.0;
                        c.changed();
                    }
                    u2.borrow_mut().push(t0.elapsed().as_secs_f64() * 1000.0);
                }
                if start.elapsed() < Duration::from_secs(4) {
                    return glib::ControlFlow::Continue;
                }
                let mut f = f2.borrow().clone();
                let u = u2.borrow();
                f.sort_by(|a, b| a.total_cmp(b));
                let avg_u = u.iter().sum::<f64>() / u.len() as f64;
                let max_u = u.iter().cloned().fold(0.0, f64::max);
                let long = f.iter().filter(|x| **x > 25.0).count();
                eprintln!(
                    "SELFTEST Pan: {} Bilder, Abstand Median {:.1} ms, 95% {:.1} ms, max {:.1} ms, >25 ms: {}; Update Ø {:.3} ms, max {:.2} ms",
                    f.len(), f[f.len() / 2], f[f.len() * 95 / 100], f[f.len() - 1], long, avg_u, max_u
                );
                glib::ControlFlow::Break
            });
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("img") {
            // Bild einfügen (GRAPHIT_IMAGE), einmal festlegen, eine Kopie bleibt ausgewählt
            let path = std::env::var("GRAPHIT_IMAGE").unwrap_or_default();
            let bytes = std::fs::read(&path).unwrap_or_default();
            let i = self.st().current_page();
            eprintln!("SELFTEST Bild: {:?}", self.insert_image(i, Pt::new(40.0, 60.0), &bytes));
            self.commit_selection();
            // zweites Bild (GRAPHIT_IMAGE2, sonst dasselbe) daneben, bleibt ausgewählt
            let bytes2 = std::env::var("GRAPHIT_IMAGE2").ok().and_then(|p| std::fs::read(p).ok()).unwrap_or(bytes);
            eprintln!("SELFTEST Bild: {:?}", self.insert_image(i, Pt::new(300.0, 60.0), &bytes2));
            self.commit_selection();
            self.queue_draw();
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("tap") {
            self.commit_selection();
            // Tipp auf leere Stelle → Radialmenü; Tipp auf Strich → Auswahl
            self.st().last_stroke_end = None;
            self.begin(Tool::Pen, Sample { x: w * 0.5, y: h * 0.85, pressure: Some(0.5), tilt: 0.0 }, true);
            self.end(None);
            eprintln!("SELFTEST Tipp leer: Radialmenü offen = {}", self.st().radial.is_some());
            self.st().radial = None;
            std::thread::sleep(Duration::from_millis(350));
            // Punkt direkt auf dem ersten Strich (Mitte seiner Punktliste)
            let y = {
                let st = self.st();
                let pg = st.doc.pages[0].borrow();
                let q = pg.layers.iter().flat_map(|l| l.items.iter()).find_map(|it| match &**it {
                    Item::Stroke(s) => Some(s.pts[0].lerp(s.pts[s.pts.len() - 1], 0.5)),
                    _ => None,
                });
                let o = st.page_origin(0);
                q.map(|q| (o.y + q.y - st.cam.y) * st.zoom).unwrap_or(0.0)
            };
            let x = {
                let st = self.st();
                let pg = st.doc.pages[0].borrow();
                let q = pg.layers.iter().flat_map(|l| l.items.iter()).find_map(|it| match &**it {
                    Item::Stroke(s) => Some(s.pts[0].lerp(s.pts[s.pts.len() - 1], 0.5)),
                    _ => None,
                });
                let o = st.page_origin(0);
                q.map(|q| (o.x + q.x - st.cam.x) * st.zoom).unwrap_or(0.0)
            };
            self.begin(Tool::Pen, Sample { x, y, pressure: Some(0.5), tilt: 0.0 }, true);
            self.end(None);
            eprintln!("SELFTEST Tipp auf Strich: Auswahl = {}", self.st().sel.is_some());
            self.commit_selection();
            // Tipp direkt nach einem Strich = Punkt
            self.begin(Tool::Pen, Sample { x: w * 0.4, y: h * 0.9, pressure: Some(0.5), tilt: 0.0 }, true);
            self.motion(&[Sample { x: w * 0.45, y: h * 0.9, pressure: Some(0.5), tilt: 0.0 }]);
            self.end(None);
            let n_before = count(self);
            self.begin(Tool::Pen, Sample { x: w * 0.47, y: h * 0.9, pressure: Some(0.5), tilt: 0.0 }, true);
            self.end(None);
            eprintln!("SELFTEST i-Punkt nach Strich: Elemente {} -> {}", n_before, count(self));
            // Tipp mit anderen Werkzeugen öffnet ebenfalls das Radialmenü
            for tool in [Tool::Eraser, Tool::Select, Tool::Shape, Tool::Space, Tool::Hand] {
                self.st().radial = None;
                let n0 = count(self);
                self.begin(tool, Sample { x: w * 0.55, y: h * 0.8, pressure: Some(0.5), tilt: 0.0 }, true);
                self.end(None);
                let open = self.st().radial.is_some();
                eprintln!("SELFTEST Tipp mit {tool:?}: Radialmenü = {open}, Elemente {} -> {}", n0, count(self));
            }
            self.st().radial = None;
            // Ziehen mit dem Radierer bleibt Radieren
            self.begin(Tool::Eraser, Sample { x: w * 0.55, y: h * 0.8, pressure: Some(0.5), tilt: 0.0 }, true);
            self.motion(&[Sample { x: w * 0.7, y: h * 0.8, pressure: Some(0.5), tilt: 0.0 }]);
            self.end(None);
            eprintln!("SELFTEST Radierer ziehen: Radialmenü = {}", self.st().radial.is_some());
            // Rückgängig im Radialmenü: Menü bleibt für weitere Schritte offen
            let c = (w * 0.5, h * 0.5);
            self.open_radial(c, false);
            let n0 = count(self);
            for _ in 0..2 {
                self.begin(Tool::Pen, Sample { x: c.0 - 105.0, y: c.1, pressure: Some(0.5), tilt: 0.0 }, true);
                self.end(None);
            }
            let open = self.st().radial.is_some();
            eprintln!("SELFTEST Radial 2× Rückgängig: offen = {open}, Elemente {} -> {}", n0, count(self));
            self.begin(Tool::Pen, Sample { x: c.0, y: c.1, pressure: Some(0.5), tilt: 0.0 }, true);
            self.end(None);
            let open = self.st().radial.is_some();
            eprintln!("SELFTEST Radial Mitte: offen = {open}");
            let cfg = self.st().cfg.clone();
            eprintln!("SELFTEST Tasten: {:?} / {:?}", cfg.button1, cfg.button2);
            return;
        }
        if std::env::var("GRAPHIT_SELFTEST").as_deref() == Ok("text") {
            self.commit_selection();
            self.begin(Tool::Text, Sample { x: w * 0.3, y: h * 0.62, pressure: None, tilt: 0.0 }, false);
            return;
        }
        eprintln!("SELFTEST Lasso: {nsel} Elemente ausgewählt, Seite hat noch {}", count(self));
        let r = self.selection_screen_rect();
        if let Some(r) = r {
            let c = r.center();
            self.begin(Tool::Select, Sample { x: c.x, y: c.y, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: c.x + 30.0, y: c.y + 40.0, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let r = self.selection_screen_rect().unwrap();
            self.begin(Tool::Select, Sample { x: r.x1 + 4.0, y: r.y1 + 4.0, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: r.x1 + 60.0, y: r.y1 + 40.0, pressure: None, tilt: 0.0 }]);
            self.end(None);
            eprintln!("SELFTEST Auswahl: xf = {:?}", self.st().sel.as_ref().map(|s| s.xf));
            // Drehen: Griff rechts um 90° nach unten um die Mitte führen
            let r = self.selection_screen_rect().unwrap();
            let c = r.center();
            let (hx, hy) = self.selection_rotate_handle().unwrap();
            let rad = (hx - c.x).hypot(hy - c.y);
            self.begin(Tool::Select, Sample { x: hx, y: hy, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: c.x + rad * 0.7, y: c.y + rad * 0.7, pressure: None, tilt: 0.0 }]);
            self.motion(&[Sample { x: c.x + 1.0, y: c.y + rad, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let ang = self.st().sel.as_ref().map(|s| s.xf.angle().to_degrees());
            eprintln!("SELFTEST Auswahl gedreht: Winkel = {ang:?}° (erwartet 90, eingerastet)");
        }
        self.commit_selection();
        eprintln!("SELFTEST nach Festlegen: {} Elemente, Undo-Schritte ok: {}", count(self), self.st().history.can_undo());

        // Formen
        for kind in ShapeKind::ALL {
            self.st().tools.shape = kind;
            self.begin(Tool::Shape, Sample { x: w * 0.3, y: h * 0.7, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: w * 0.45, y: h * 0.8, pressure: None, tilt: 0.0 }]);
            self.end(None);
        }
        eprintln!("SELFTEST Formen: {} Elemente", count(self));

        // Freihand-Kreis mit automatischer Formerkennung
        self.st().cfg.auto_shapes = true;
        self.begin(Tool::Pen, Sample { x: w * 0.7, y: h * 0.75, pressure: None, tilt: 0.0 }, false);
        for i in 0..=80 {
            let a = i as f64 / 80.0 * std::f64::consts::TAU;
            self.motion(&[Sample { x: w * 0.7 + 50.0 * a.sin(), y: h * 0.75 - 45.0 * a.cos(), pressure: None, tilt: 0.0 }]);
        }
        self.end(None);
        self.st().cfg.auto_shapes = false;
        let last_pts = self.st().doc.pages[0].borrow().layers.last().unwrap().items.last().map(|i| match &**i {
            Item::Stroke(s) => s.pts.len(),
            _ => 0,
        });
        eprintln!("SELFTEST Formerkennung: letzter Strich hat {:?} Punkte (Ellipse ≈ 37+)", last_pts);

        // Platz einfügen
        let before = count(self);
        self.begin(Tool::Space, Sample { x: w * 0.5, y: h * 0.5, pressure: None, tilt: 0.0 }, false);
        self.motion(&[Sample { x: w * 0.5, y: h * 0.5 + 60.0, pressure: None, tilt: 0.0 }]);
        self.end(None);
        eprintln!("SELFTEST Platz: Elemente {} -> {}, Seitenhöhe {:.0}", before, count(self), self.st().doc.pages[0].borrow().height);

        // Text
        self.begin(Tool::Text, Sample { x: w * 0.3, y: h * 0.9, pressure: None, tilt: 0.0 }, false);
        eprintln!("SELFTEST Text-Editor offen: {}", self.text_edit_info().is_some());
        self.cancel_text();

        // Radialmenü per Rechtsklick öffnen (bleibt für den Screenshot offen)
        let mut undo_steps = 0;
        while self.st().history.can_undo() && undo_steps < 50 {
            self.undo();
            undo_steps += 1;
        }
        eprintln!("SELFTEST Undo: {undo_steps} Schritte, Elemente wieder {} (Start {n0})", count(self));
        self.redo();
        self.redo();
        self.redo();

        // Im ausgewählten Bild radieren
        {
            let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, 200, 100).unwrap();
            {
                let cr = cairo::Context::new(&surf).unwrap();
                cr.set_source_rgb(0.9, 0.1, 0.1);
                cr.paint().unwrap();
            }
            let mut png = Vec::new();
            surf.write_to_png(&mut png).unwrap();
            let page = self.st().current_page();
            self.insert_image(page, Pt::new(100.0, 100.0), &png).unwrap();
            let image = |c: &Canvas| {
                c.st().sel.as_ref().and_then(|s| s.entries.iter().find_map(|e| match &*e.2 {
                    Item::Image(i) => Some(i.clone()),
                    _ => None,
                }))
            };
            let before = image(self).unwrap().data.clone();
            let r = self.selection_screen_rect().unwrap();
            let c = r.center();
            self.begin(Tool::Eraser, Sample { x: c.x, y: c.y, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: c.x + 20.0, y: c.y, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let img = image(self).unwrap();
            let raw = render::decode_image_raw(&img.data).unwrap();
            let (cw, ch) = (raw.width(), raw.height());
            let mut raw = raw;
            let stride = raw.stride() as usize;
            let d = raw.data().unwrap();
            let alpha = |x: i32, y: i32| d[y as usize * stride + x as usize * 4 + 3];
            eprintln!(
                "SELFTEST Bild radieren: geändert = {}, Alpha Mitte = {} (erwartet 0), Alpha Ecke = {} (erwartet 255)",
                !Arc::ptr_eq(&before, &img.data),
                alpha(cw / 2, ch / 2),
                alpha(1, 1)
            );
            drop(d);
            self.commit_selection();
            self.undo();
        }

        // Fixieren: danach nicht mehr auswählbar, Lösen macht es wieder auswählbar
        {
            let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, 50, 50).unwrap();
            let mut png = Vec::new();
            surf.write_to_png(&mut png).unwrap();
            let page = self.st().current_page();
            self.insert_image(page, Pt::new(300.0, 300.0), &png).unwrap();
            self.lock_selection();
            self.select_all();
            let after_lock = self.st().sel.as_ref().map(|s| s.entries.len()).unwrap_or(0);
            let freed = self.unlock_page();
            self.select_all();
            let after_unlock = self.st().sel.as_ref().map(|s| s.entries.len()).unwrap_or(0);
            eprintln!(
                "SELFTEST Fixieren: Strg+A wählt {after_lock}, nach Lösen ({freed} gelöst) {after_unlock} (erwartet eins mehr)"
            );
            self.commit_selection();
        }

        // Auswahl auf die nächste Seite ziehen, dann darüber Platz entfernen
        {
            let cp = self.st().current_page();
            self.insert_page(cp + 1, None);
            let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, 40, 40).unwrap();
            let mut png = Vec::new();
            surf.write_to_png(&mut png).unwrap();
            self.insert_image(cp, Pt::new(200.0, 200.0), &png).unwrap();
            let images = |c: &Canvas, i: usize| -> Vec<Rect> {
                c.st().doc.pages[i]
                    .borrow()
                    .layers
                    .iter()
                    .flat_map(|l| l.items.iter().filter_map(|it| matches!(&**it, Item::Image(_)).then(|| it.bbox().unwrap())).collect::<Vec<_>>())
                    .collect()
            };
            let (n0, n1) = (images(self, cp).len(), images(self, cp + 1).len());
            let r = self.selection_screen_rect().unwrap();
            let c = r.center();
            let target = {
                let st = self.st();
                let o = st.page_origin(cp + 1);
                ((o.x + 200.0 - st.cam.x) * st.zoom, (o.y + 300.0 - st.cam.y) * st.zoom)
            };
            self.begin(Tool::Select, Sample { x: c.x, y: c.y, pressure: None, tilt: 0.0 }, false);
            self.motion(&[Sample { x: target.0, y: target.1, pressure: None, tilt: 0.0 }]);
            self.end(None);
            let on_target = {
                let st = self.st();
                st.sel.as_ref().map(|s| std::rc::Rc::ptr_eq(&s.page, &st.doc.pages[cp + 1])).unwrap_or(false)
            };
            self.commit_selection();
            eprintln!(
                "SELFTEST Seitenwechsel: Auswahl auf Zielseite = {on_target}, Bilder Seite A {n0}→{}, Seite B {n1}→{}",
                images(self, cp).len(),
                images(self, cp + 1).len()
            );
            // Platz über dem Bild entfernen: es rutscht um 30 nach oben
            let y_img = images(self, cp + 1).last().unwrap().y0;
            let at = |y: f64| {
                let st = self.st();
                let o = st.page_origin(cp + 1);
                Sample { x: (o.x + 100.0 - st.cam.x) * st.zoom, y: (o.y + y - st.cam.y) * st.zoom, pressure: None, tilt: 0.0 }
            };
            self.begin(Tool::Space, at(y_img - 40.0), false);
            self.motion(&[at(y_img - 70.0)]);
            self.end(None);
            let y_after = images(self, cp + 1).last().unwrap().y0;
            eprintln!("SELFTEST Platz entfernen: Bild von {y_img:.1} nach {y_after:.1} (erwartet 30 höher)");
            self.undo();
            self.undo();
            eprintln!("SELFTEST nach 2× Rückgängig: Bilder Seite A {}, Seite B {}", images(self, cp).len(), images(self, cp + 1).len());
        }
        self.open_radial((w * 0.5, h * 0.45), false);
        if let Some(r) = &mut self.st().radial {
            r.hover = Some(edit::RadialItem::Tool(Tool::Shape));
        }
        self.queue_draw();
    }
}

#[cfg(test)]
mod fling_tests {
    use super::*;

    /// Gleichmäßiger Wisch nach oben: Geschwindigkeit ≈ −1000 px/s senkrecht.
    #[test]
    fn fling_velocity_from_regression() {
        let mut t = Touch::default();
        let now = Instant::now();
        for i in 0..10 {
            let dt = Duration::from_millis(90 - i * 10);
            let y = 500.0 - (i as f64) * 10.0; // 10 px pro 10 ms
            t.samples.push_back((now - dt, [100.0, y, 0.0]));
        }
        let (vx, vy) = t.fling_velocity().unwrap();
        assert!(vx.abs() < 1.0, "vx = {vx}");
        assert!((vy + 1000.0).abs() < 50.0, "vy = {vy}");
    }

    /// Zwei Finger gehen auseinander: Abstands-Trend ≈ +500 px/s für die Zoom-Vorhersage.
    #[test]
    fn spread_trend_for_pinch_prediction() {
        let mut t = Touch::default();
        let now = Instant::now();
        for i in 0..6 {
            let dt = Duration::from_millis(50 - i * 10);
            t.samples.push_back((now - dt, [300.0, 300.0, 200.0 + i as f64 * 5.0]));
        }
        let v = t.trend(50, 30).unwrap();
        assert!(v[0].abs() < 1.0 && v[1].abs() < 1.0);
        assert!((v[2] - 500.0).abs() < 25.0, "Abstand {}", v[2]);
    }

    fn samples(f: impl Fn(f64) -> [f64; 3]) -> Vec<(f64, [f64; 3])> {
        // 8 ms Abstand wie ein 120-Hz-Touchscreen, letzter Punkt bei t = 0
        (0..7).map(|i| -0.048 + i as f64 * 0.008).map(|t| (t, f(t))).collect()
    }

    /// Gleichmäßiges Schieben: Vorhersage = Geschwindigkeit × Zeit.
    #[test]
    fn touch_prediction_steady_pan() {
        let v = predict_touch(&samples(|t| [100.0, 500.0 + 1000.0 * t, 200.0]), 0.014).unwrap();
        assert!(v[0].abs() < 0.5, "x {}", v[0]);
        assert!((v[1] - 14.0).abs() < 1.0, "y {}", v[1]);
        assert!(v[2].abs() < 0.5, "Abstand {}", v[2]);
    }

    /// Finger bremst bis zum Stillstand bei t = 0: kaum noch Vorhersage, kein Überschießen.
    #[test]
    fn touch_prediction_stops_when_braking() {
        // y(t) = −a/2·t² (Geschwindigkeit fällt linear auf 0 bei t = 0)
        let v = predict_touch(&samples(|t| [0.0, -10000.0 * t * t, 200.0]), 0.014).unwrap();
        assert!(v[1].abs() < 2.0, "y {}", v[1]);
        // linear über 50 ms hätte deutlich mehr vorhergesagt
        let mut t = Touch::default();
        let now = Instant::now();
        for (dt, s) in samples(|t| [0.0, -10000.0 * t * t, 200.0]) {
            t.samples.push_back((now - Duration::from_secs_f64(-dt), s));
        }
        let lin = t.trend(50, 30).unwrap()[1] * 0.014;
        assert!(lin > 3.0, "linear {lin}");
    }

    /// Finger gehen auseinander und werden langsamer: Abstand nur bis zum Stillstand.
    #[test]
    fn touch_prediction_pinch_braking() {
        // Abstand wächst mit 800 px/s bei t = −48 ms und kommt bei t = +12 ms zum Stehen
        let s = |t: f64| 200.0 + 160.0 * t - 6667.0 * t * t;
        let v = predict_touch(&samples(|t| [300.0, 300.0, s(t)]), 0.030).unwrap();
        let exact = s(0.012) - s(0.0);
        assert!(v[2] > 0.0 && v[2] <= exact + 0.5, "Abstand {} (bis Stillstand {exact})", v[2]);
    }

    /// Ansicht springt weiter als der Finger: als Sprung markiert; normales Schieben nicht.
    #[test]
    fn gesture_log_flags_jumps() {
        let mut l = GestureLog::start(2.0);
        for i in 0..5 {
            l.frame(1, (100.0, 100.0 + i as f64 * 10.0), [0.0; 3], 2.0, if i == 0 { 0.0 } else { 10.0 }, false);
        }
        assert!(l.flags.is_empty(), "{:?}", l.flags);
        l.frame(1, (100.0, 150.0), [0.0; 3], 2.0, 90.0, false);
        assert_eq!(l.flags, vec!["sprung"]);
    }

    /// Finger vor dem Loslassen angehalten: kein Schwung.
    #[test]
    fn no_fling_after_pause() {
        let mut t = Touch::default();
        let now = Instant::now();
        for i in 0..5 {
            t.samples.push_back((now - Duration::from_millis(300 - i * 10), [0.0, i as f64 * 10.0, 0.0]));
        }
        assert!(t.fling_velocity().is_none());
    }
}

#[cfg(test)]
mod predict_tests {
    use super::*;

    fn track(f: impl Fn(f64) -> Pt) -> Vec<(f64, Pt)> {
        (0..=12).map(|i| {
            let t = i as f64 * 0.004;
            (t, f(t))
        }).collect()
    }

    /// Gleichförmig 200 pt/s nach rechts, 22 ms voraus → etwa 4,4 pt weiter, geradeaus.
    #[test]
    fn straight_line() {
        let h = track(|t| Pt::new(200.0 * t, 10.0));
        let p = predict_path(&h, 0.022, 100.0, 1.0);
        let end = *p.last().unwrap();
        let x0 = h.last().unwrap().1.x;
        assert!((end.x - x0 - 4.4).abs() < 0.5, "Vorsprung {:.2}", end.x - x0);
        assert!((end.y - 10.0).abs() < 0.1);
    }

    /// Stift bremst auf den Stillstand zu: Vorhersage darf nicht über den Haltepunkt hinaus.
    #[test]
    fn braking_does_not_overshoot() {
        // x(t) = 300·t − 3000·t² hält bei t = 0,05 an (x = 7,5)
        let h: Vec<(f64, Pt)> = (0..=10).map(|i| {
            let t = i as f64 * 0.004;
            (t, Pt::new(300.0 * t - 3000.0 * t * t, 0.0))
        }).collect();
        let p = predict_path(&h, 0.03, 100.0, 1.0);
        let end = p.last().map(|p| p.x).unwrap_or(h.last().unwrap().1.x);
        assert!(end <= 7.6, "überschießt bis {end:.2}");
    }

    /// Kreisbogen: Vorhersage biegt mit ab statt tangential geradeaus zu laufen.
    #[test]
    fn follows_curve() {
        let (r, w) = (20.0, 10.0); // 20 pt Radius, 10 rad/s
        let h = track(|t| Pt::new(r * (w * t).cos(), r * (w * t).sin()));
        let p = predict_path(&h, 0.03, 100.0, 1.0);
        let end = *p.last().unwrap();
        let dist_from_center = end.x.hypot(end.y);
        // tangential geradeaus läge der Punkt bei ~20,9 pt vom Mittelpunkt
        assert!((dist_from_center - r).abs() < 0.6, "Abstand {dist_from_center:.2}");
    }
}
