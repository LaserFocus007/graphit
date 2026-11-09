//! Alles, was das Dokument verändert: Stift, Radierer, Auswahl, Formen, Text,
//! Bilder, Platz einfügen, Seitenhöhe und das Radialmenü.

use std::f64::consts::{PI, TAU};

use base64::Engine as _;
use std::rc::Rc;

use super::*;
use crate::geom::{point_in_polygon, Xform};
use crate::shapes;

// ------------------------------------------------------------------------ Typen

/// Aktive Auswahl. Die ausgewählten Elemente sind solange aus ihren Ebenen
/// entfernt und werden separat (als eine Textur) gezeichnet.
/// Abstand des Auswahlrahmens um die Elemente und des Drehgriffs vom Rahmen (Pixel)
const SEL_PAD: f64 = 4.0;
const ROT_OFF: f64 = 26.0;

pub struct Selection {
    pub page: PageRef,
    /// Ebenen-Listen vor dem Herausnehmen
    orig: Vec<ItemList>,
    /// (Ebene, Index in orig oder usize::MAX für neu eingefügte, Element)
    pub(super) entries: Vec<(usize, usize, ItemRef)>,
    pub xf: Xform,
    base: Rect,
    modified: bool,
    deleted: bool,
    /// beim Festlegen an den letzten Rückgängig-Schritt anhängen (Wechsel der Seite)
    merge_undo: bool,
    tex: Option<(gdk::MemoryTexture, Rect, Xform, f64)>,
}

impl Selection {
    pub fn rect(&self) -> Rect {
        self.xf.apply_rect(&self.base)
    }
    /// Ecken des (evtl. gedrehten) Auswahlrahmens, um `pad` Seiteneinheiten erweitert.
    fn corners(&self, pad: f64) -> [Pt; 4] {
        self.xf.corners(&self.base.expand(pad / self.xf.scale()))
    }
    fn center(&self) -> Pt {
        self.xf.apply(self.base.center())
    }
    /// Drehgriff: rechts neben der Mitte der rechten Kante (`pad`, `off` in Seiteneinheiten).
    fn rotate_handle(&self, pad: f64, off: f64) -> (Pt, Pt) {
        let c = self.corners(pad);
        let m = Pt::new((c[1].x + c[2].x) / 2.0, (c[1].y + c[2].y) / 2.0);
        let ang = self.xf.angle();
        (m, Pt::new(m.x + off * ang.cos(), m.y + off * ang.sin()))
    }
    fn transformed_items(&self) -> Vec<Item> {
        self.entries.iter().map(|e| e.2.transformed(&self.xf)).collect()
    }
}

/// Ein Bild der Auswahl, in dem gerade radiert wird.
pub struct ImageErase {
    /// Index in `Selection::entries`
    idx: usize,
    /// Pixel wie in der Datei (ohne Papier-Anpassung); werden am Ende neu gespeichert
    raw: cairo::ImageSurface,
    changed: bool,
}

pub struct TextEdit {
    pub page: PageRef,
    pub pos: Pt,
    pub font: String,
    pub size: f64,
    pub color: Color,
    /// (Ebene, Index, Liste vorher) wenn ein bestehender Text bearbeitet wird
    orig: Option<(usize, usize, ItemList)>,
    pub initial: String,
}

pub struct RadialMenu {
    pub center: (f64, f64),
    pub hover: Option<RadialItem>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialItem {
    /// Mitte: Menü schließen
    Close,
    Undo,
    Redo,
    Paste,
    Claude,
    Color(usize),
    Tool(Tool),
}

/// Anfragen an das Fenster (z. B. Zwischenablage lesen).
pub enum Request {
    Paste,
    Claude,
    /// eingekreister Ausschnitt: Bild, Seite, Bereich in Seitenanteilen
    ClaudeRegion(Vec<u8>, usize, [f64; 4]),
}

const R_CENTER: f64 = 30.0;
const R_COLORS: f64 = 78.0;
const R_TOOLS: f64 = 132.0;
/// Äußerer Ring im Uhrzeigersinn ab oben: Rückgängig links, Wiederholen rechts.
const RADIAL_RING: [RadialItem; 10] = [
    RadialItem::Tool(Tool::Ask),
    RadialItem::Tool(Tool::Pen),
    RadialItem::Tool(Tool::Highlighter),
    RadialItem::Redo,
    RadialItem::Tool(Tool::Shape),
    RadialItem::Tool(Tool::Space),
    RadialItem::Claude,
    RadialItem::Paste,
    RadialItem::Undo,
    RadialItem::Tool(Tool::Hand),
];

/// Welcher Eintrag des Radialmenüs liegt unter dem Punkt?
pub fn radial_hit(c: (f64, f64), p: (f64, f64)) -> Option<RadialItem> {
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    let d = dx.hypot(dy);
    if d < R_CENTER {
        return Some(RadialItem::Close);
    }
    if d > R_TOOLS + 40.0 {
        return None;
    }
    let ang = dx.atan2(-dy).rem_euclid(TAU);
    if d < R_COLORS {
        return Some(RadialItem::Color((((ang + PI / 8.0) / (PI / 4.0)) as usize) % 8));
    }
    let step = TAU / RADIAL_RING.len() as f64;
    Some(RADIAL_RING[(((ang + step / 2.0) / step) as usize) % RADIAL_RING.len()])
}

/// One-Euro-Filter (Casiez et al.): glättet stark, solange der Stift langsam ist (gegen
/// Zittern), und kaum, wenn er schnell ist. Schnelle Schrift und Zickzack behalten so
/// ihre Spitzen, statt wie bei fester Glättung abgerundet und abgeschnitten zu werden.
fn one_euro(live: &mut Live, prev_raw: Pt, raw: Pt, last: Pt, dt: f64, smoothing: f64) -> Pt {
    let (p, fv) = one_euro_step(live.fv, prev_raw, raw, last, dt, smoothing);
    live.fv = fv;
    p
}

fn one_euro_step(fv: (f64, f64), prev_raw: Pt, raw: Pt, last: Pt, dt: f64, smoothing: f64) -> (Pt, (f64, f64)) {
    let alpha = |cutoff: f64| 1.0 / (1.0 + 1.0 / (std::f64::consts::TAU * cutoff * dt));
    // Geschwindigkeit des Stifts, selbst leicht geglättet
    let ad = alpha(8.0);
    let v = ((raw.x - prev_raw.x) / dt, (raw.y - prev_raw.y) / dt);
    let fv = (fv.0 + ad * (v.0 - fv.0), fv.1 + ad * (v.1 - fv.1));
    let speed = fv.0.hypot(fv.1);
    // Glättung 0..1 → Grenzfrequenz im Stillstand 6,6 … 0,6 Hz; ab ~300 pt/s (schnelle
    // Schrift) liegt sie über 100 Hz, der Filter ist dann praktisch aus
    let min_cutoff = 0.6 + 6.0 * (1.0 - smoothing);
    let a = alpha(min_cutoff + 0.4 * speed);
    (last.lerp(raw, a), fv)
}

#[cfg(test)]
mod one_euro_tests {
    use super::*;

    /// Schneller Zickzack (300 pt/s, 250 Hz): die Spitze darf kaum abgeschnitten werden.
    #[test]
    fn fast_zigzag_keeps_tips() {
        let dt = 0.004;
        let (mut fv, mut out, mut prev) = ((0.0, 0.0), Pt::new(0.0, 0.0), Pt::new(0.0, 0.0));
        let mut max_y: f64 = 0.0;
        for i in 1..=60 {
            // 30 Schritte hoch, 30 runter, je 1,2 pt pro 4 ms
            let y = if i <= 30 { i as f64 * 1.2 } else { (60 - i) as f64 * 1.2 };
            let raw = Pt::new(i as f64 * 0.5, y);
            let (p, f) = one_euro_step(fv, prev, raw, out, dt, 0.4);
            fv = f;
            out = p;
            prev = raw;
            max_y = max_y.max(out.y);
        }
        // Spitze bei 36 pt: höchstens 2 pt verloren
        assert!(max_y > 34.0, "Spitze {max_y:.1} pt statt 36");
    }

    /// Stillstand mit Zittern (±0,3 pt): wird deutlich beruhigt.
    #[test]
    fn slow_jitter_is_smoothed() {
        let dt = 0.004;
        let (mut fv, mut out, mut prev) = ((0.0, 0.0), Pt::new(0.0, 0.0), Pt::new(0.0, 0.0));
        let mut spread_out: f64 = 0.0;
        for i in 0..100 {
            let raw = Pt::new(if i % 2 == 0 { 0.3 } else { -0.3 }, 0.0);
            let (p, f) = one_euro_step(fv, prev, raw, out, dt, 0.4);
            fv = f;
            out = p;
            prev = raw;
            if i > 50 {
                spread_out = spread_out.max(out.x.abs());
            }
        }
        assert!(spread_out < 0.2, "Zittern {spread_out:.2} pt");
    }
}

fn seg_angle(i: usize) -> f64 {
    i as f64 * PI / 4.0
}

fn ring_angle(i: usize) -> f64 {
    i as f64 * TAU / RADIAL_RING.len() as f64
}

/// Kreisringsegment als GSK-Pfad (Winkel 0 = oben, im Uhrzeigersinn).
fn sector(c: (f64, f64), r0: f64, r1: f64, a0: f64, a1: f64) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    let pt = |r: f64, a: f64| ((c.0 + r * a.sin()) as f32, (c.1 - r * a.cos()) as f32);
    let n = 12;
    let (x, y) = pt(r1, a0);
    b.move_to(x, y);
    for k in 1..=n {
        let (x, y) = pt(r1, a0 + (a1 - a0) * k as f64 / n as f64);
        b.line_to(x, y);
    }
    for k in (0..=n).rev() {
        let (x, y) = pt(r0, a0 + (a1 - a0) * k as f64 / n as f64);
        b.line_to(x, y);
    }
    b.close();
    b.to_path()
}

fn circle_path(c: (f64, f64), r: f64) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    b.add_circle(&graphene::Point::new(c.0 as f32, c.1 as f32), r as f32);
    b.to_path()
}

fn rgba(c: Color) -> gdk::RGBA {
    c.to_gdk()
}

// ------------------------------------------------------------------------ Canvas

impl Canvas {
    pub fn connect_request(&self, f: impl Fn(&Canvas, Request) + 'static) {
        self.imp().request.replace(Some(Box::new(f)));
    }

    /// Das Fenster liefert hierüber den aktuellen Inhalt des Text-Editors.
    pub fn set_text_source(&self, f: impl Fn() -> String + 'static) {
        self.imp().text_source.replace(Some(Box::new(f)));
    }

    pub(super) fn request(&self, r: Request) {
        if let Some(f) = self.imp().request.borrow().as_ref() {
            f(self, r);
        }
    }

    // ---------------------------------------------------------------- Beginn

    pub(super) fn begin(&self, tool: Tool, s: Sample, stylus: bool) {
        // offenes Radialmenü: Tipp wählt aus oder schließt
        let radial = self.st().radial.take();
        if let Some(r) = radial {
            if let Some(item) = radial_hit(r.center, (s.x, s.y)) {
                self.apply_radial(item, r.center);
            }
            self.changed();
            return;
        }
        if self.st().readonly && tool != Tool::Hand {
            self.st().action = Some(Action::Pan { last: (s.x, s.y) });
            return;
        }
        // Griff unter einer Seite: Seite verlängern
        if tool != Tool::Hand {
            if let Some(i) = self.page_handle_hit(s.x, s.y) {
                self.commit_edits();
                let mut st = self.st();
                let page = st.doc.pages[i].clone();
                let h0 = page.borrow().height;
                let min_h = {
                    let p = page.borrow();
                    match (&st.doc.pdf_path, p.bg.is_pdf()) {
                        (Some(pdf), true) => {
                            render::with_pdf(pdf, |d| d.page(p.bg.pdf_index() as i32).map(|pg| pg.size().1))
                                .flatten()
                                .unwrap_or(100.0)
                                + p.bg.pdf_gap_total()
                        }
                        _ => 100.0,
                    }
                };
                let y0 = st.to_layout(s.x, s.y).y;
                st.action = Some(Action::PageHeight { page, h0, min_h, y0 });
                drop(st);
                self.changed();
                return;
            }
        }
        // Radierer über ausgewählten Bildern: in den Bildern radieren
        if tool == Tool::Eraser && self.begin_image_erase(s) {
            self.changed();
            return;
        }
        // bestehende Auswahl: anfassen zum Verschieben/Skalieren
        if matches!(tool, Tool::Select | Tool::Pen | Tool::Highlighter | Tool::Shape) && self.st().sel.is_some() {
            if self.begin_selection_drag(s) {
                self.changed();
                return;
            }
            self.commit_selection();
        } else if tool != Tool::Select {
            self.commit_selection();
        }
        if self.st().text.is_some() {
            // erster Tipp außerhalb beendet nur die Texteingabe
            self.commit_text();
            if tool == Tool::Text {
                self.changed();
                return;
            }
        }

        enum Post {
            None,
            HoldTimer,
            Erase(Pt),
        }
        let mut st = self.st();
        st.hover = Some((s.x, s.y));
        let post = match tool {
            Tool::Highlighter if st.page_at(s.x, s.y, 0.0).map(|(i, p)| self.try_text_mark(&mut st, i, p)).unwrap_or(false) => Post::None,
            Tool::Pen | Tool::Highlighter => {
                let Some((i, p)) = st.page_at(s.x, s.y, 0.0) else { return };
                let hl = tool == Tool::Highlighter;
                let ps = st.tools.pen_for(tool).clone();
                let pressure = stylus && st.cfg.pressure && !hl && s.pressure.is_some();
                let w0 = if pressure { st.width_for(ps.width, s.pressure, s.tilt) } else { ps.width };
                let mut attrs: Attrs = Vec::new();
                if let Some(style) = ps.style.attr() {
                    attrs.push(("style".into(), style.into()));
                }
                attrs.push(("capStyle".into(), if hl { "butt" } else { "round" }.into()));
                let mut live = Live {
                    page: st.doc.pages[i].clone(),
                    origin: st.page_origin(i),
                    tool: if hl { StrokeTool::Highlighter } else { StrokeTool::Pen },
                    color: ps.color,
                    width: ps.width,
                    pressure,
                    attrs,
                    pts: vec![p],
                    ws: vec![w0],
                    chunks: Vec::new(),
                    open: None,
                    open_from: 0,
                    scale: st.zoom * st.sf,
                    raw: p,
                    raw_w: w0,
                    still_since: Instant::now(),
                    still_at: p,
                    straight: false,
                    frozen: false,
                    started: Instant::now(),
                    start_screen: (s.x, s.y),
                    vel: (0.0, 0.0),
                    vel_t: Instant::now(),
                    fv: (0.0, 0.0),
                    ft: Instant::now(),
                    hist: std::collections::VecDeque::from([(0.0, p)]),
                    htime: 0.0,
                    pred: None,
                };
                live.update_open();
                st.action = Some(Action::Draw(live));
                if st.cfg.hold_to_straighten || (st.cfg.radial_longpress && stylus) { Post::HoldTimer } else { Post::None }
            }
            Tool::Eraser => {
                let Some((i, p)) = st.page_at(s.x, s.y, 20.0) else { return };
                let page = st.doc.pages[i].clone();
                let orig = page.borrow().layers.iter().map(|l| l.items.clone()).collect();
                let origin = st.page_origin(i);
                st.action = Some(Action::Erase { page, origin, orig, last: p });
                Post::Erase(p)
            }
            Tool::Ask => {
                let Some((i, p)) = st.page_at(s.x, s.y, 10.0) else { return };
                st.action = Some(Action::Lasso { page: st.doc.pages[i].clone(), origin: st.page_origin(i), pts: vec![p], rect: false });
                Post::None
            }
            Tool::Select => {
                let Some((i, p)) = st.page_at(s.x, s.y, 10.0) else { return };
                let rect = st.tools.select_mode == SelectMode::Rect;
                st.action = Some(Action::Lasso { page: st.doc.pages[i].clone(), origin: st.page_origin(i), pts: vec![p], rect });
                Post::None
            }
            Tool::Shape => {
                let Some((i, p)) = st.page_at(s.x, s.y, 0.0) else { return };
                let kind = st.tools.shape;
                st.action = Some(Action::Shape { page: st.doc.pages[i].clone(), origin: st.page_origin(i), kind, a: p, b: p });
                Post::None
            }
            Tool::Space => {
                let Some((i, p)) = st.page_at(s.x, s.y, 0.0) else { return };
                // Nach oben ziehen entfernt Platz, aber nur bis zum Inhalt darüber
                // (sonst würde der nachrückende Inhalt darüberliegendes überdecken)
                let page = st.doc.pages[i].clone();
                let min_y = page
                    .borrow()
                    .layers
                    .iter()
                    .flat_map(|l| l.items.iter().filter_map(|it| it.bbox()))
                    .filter(|b| b.y0 < p.y - 0.5 && b.y1 <= p.y + 0.5)
                    .fold(0.0f64, |m, b| m.max(b.y1));
                st.action = Some(Action::Space { page, origin: st.page_origin(i), y0: p.y, y: p.y, min_y });
                Post::None
            }
            Tool::Text => {
                let Some((i, p)) = st.page_at(s.x, s.y, 0.0) else { return };
                drop(st);
                self.start_text(i, p);
                return;
            }
            Tool::Hand => {
                st.action = Some(Action::Pan { last: (s.x, s.y) });
                Post::None
            }
        };
        // Kurzes Tippen soll auch mit diesen Werkzeugen das Radialmenü öffnen
        let tappable = matches!(tool, Tool::Eraser | Tool::Select | Tool::Shape | Tool::Space | Tool::Hand);
        st.tap = (tappable && st.action.is_some() && (st.cfg.tap_radial || st.cfg.tap_select))
            .then(|| (Instant::now(), (s.x, s.y), false));
        drop(st);
        match post {
            Post::HoldTimer => self.start_hold_timer(),
            Post::Erase(p) => self.erase_at(p, p),
            Post::None => {}
        }
        self.changed();
    }

    /// Radierer setzt auf einer Auswahl mit Bildern auf: Bildpixel radieren statt Striche.
    fn begin_image_erase(&self, s: Sample) -> bool {
        let mut guard = self.st();
        let st = &mut *guard;
        let Some(i) = st.sel.as_ref().and_then(|sel| st.doc.index_of(&sel.page)) else { return false };
        let origin = st.page_origin(i);
        let l = st.to_layout(s.x, s.y);
        let p = Pt::new(l.x - origin.x, l.y - origin.y);
        let zoom = st.zoom;
        let Some(sel) = &mut st.sel else { return false };
        let xf = sel.xf;
        if !sel.base.expand(6.0 / (zoom * xf.scale())).contains(xf.inverse().apply(p)) {
            return false;
        }
        let mut parts = Vec::new();
        for (idx, e) in sel.entries.iter_mut().enumerate() {
            let Item::Image(img) = &*e.2 else { continue };
            let Some(raw) = render::decode_image_raw(&img.data) else { continue };
            // eigene Kopie der angezeigten Pixel: das alte Bild bleibt für Rückgängig unverändert
            let Some(shown) = img.decoded.get_or_init(|| render::decode(&img.data)) else { continue };
            let Some(surf) = render::copy_surface(&shown.surf) else { continue };
            let dark = shown.dark.as_ref().and_then(render::copy_surface);
            let cache: ImageCache = Arc::new(std::sync::OnceLock::new());
            let _ = cache.set(Some(DecodedImage { surf, dark }));
            e.2 = Arc::new(Item::Image(ImageItem { decoded: cache, ..img.clone() }));
            parts.push(ImageErase { idx, raw, changed: false });
        }
        if parts.is_empty() {
            return false;
        }
        st.action = Some(Action::ImageErase { parts, last: p });
        drop(guard);
        self.image_erase_segment(p, p);
        true
    }

    /// Radiert die Strecke a→b (Seitenkoordinaten) aus den Bildern der Auswahl.
    fn image_erase_segment(&self, a: Pt, b: Pt) {
        {
            let mut st = self.st();
            let st = &mut *st;
            let r = st.tools.eraser_size;
            let (Some(Action::ImageErase { parts, .. }), Some(sel)) = (&mut st.action, &st.sel) else { return };
            let inv = sel.xf.inverse();
            let r = r / sel.xf.scale();
            for part in parts.iter_mut() {
                let Item::Image(img) = &*sel.entries[part.idx].2 else { continue };
                // in das ungedrehte Bildrechteck zurückrechnen
                let back = inv.then(&Xform::rotate_about(-img.angle().to_radians(), img.rect.center()));
                let (qa, qb) = (back.apply(a), back.apply(b));
                let rect = img.rect;
                if !Rect::from_pts(qa, qb).expand(r).intersects(&rect) {
                    continue;
                }
                let surfs = img.decoded.get().and_then(|d| d.as_ref()).map(|d| [Some(&d.surf), d.dark.as_ref()]).unwrap_or([None, None]);
                for surf in std::iter::once(Some(&part.raw)).chain(surfs).flatten() {
                    let Ok(cr) = cairo::Context::new(surf) else { continue };
                    cr.scale(surf.width() as f64 / rect.width(), surf.height() as f64 / rect.height());
                    cr.translate(-rect.x0, -rect.y0);
                    cr.set_operator(cairo::Operator::Clear);
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_width(2.0 * r);
                    cr.move_to(qa.x, qa.y);
                    cr.line_to(qb.x, qb.y);
                    let _ = cr.stroke();
                }
                part.changed = true;
            }
        }
        self.render_selection();
    }

    /// Radieren in Bildern beendet: veränderte Bilder als PNG neu speichern.
    fn finish_image_erase(&self, parts: Vec<ImageErase>) {
        {
            let mut st = self.st();
            let Some(sel) = &mut st.sel else { return };
            for part in parts.into_iter().filter(|p| p.changed) {
                let Item::Image(img) = &*sel.entries[part.idx].2 else { continue };
                let mut png = Vec::new();
                if part.raw.write_to_png(&mut png).is_err() {
                    continue;
                }
                let data = Arc::new(base64::engine::general_purpose::STANDARD.encode(&png));
                sel.entries[part.idx].2 = Arc::new(Item::Image(ImageItem { data, ..img.clone() }));
                sel.modified = true;
            }
        }
        self.render_selection();
    }

    /// Auswahl losgelassen: liegt ihre Mitte jetzt auf einer anderen Seite, wandert sie dorthin.
    fn finish_selection_move(&self) {
        let target = {
            let st = self.st();
            st.sel.as_ref().and_then(|sel| {
                let i = st.doc.index_of(&sel.page)?;
                let o = st.page_origin(i);
                let c = sel.rect().center();
                let (x, y) = ((o.x + c.x - st.cam.x) * st.zoom, (o.y + c.y - st.cam.y) * st.zoom);
                let (j, _) = st.page_at(x, y, 0.0).filter(|(j, _)| *j != i)?;
                let oj = st.page_origin(j);
                let shift = Xform::translate(o.x - oj.x, o.y - oj.y);
                Some((st.doc.pages[j].clone(), sel.transformed_items().iter().map(|it| it.transformed(&shift)).collect::<Vec<_>>()))
            })
        };
        let Some((page, items)) = target else {
            self.render_selection();
            return;
        };
        // von der alten Seite entfernen und auf der neuen als Auswahl einsetzen
        if let Some(sel) = &mut self.st().sel {
            sel.deleted = true;
        }
        self.commit_selection();
        self.select_new(page, items);
        // beides zusammen ist ein Rückgängig-Schritt
        if let Some(sel) = &mut self.st().sel {
            sel.merge_undo = true;
        }
    }

    /// Prüft, ob ein Druck die bestehende Auswahl trifft (Griff oder Fläche).
    fn begin_selection_drag(&self, s: Sample) -> bool {
        let mut st = self.st();
        let Some(sel) = &st.sel else { return false };
        let Some(i) = st.doc.index_of(&sel.page) else { return false };
        let origin = st.page_origin(i);
        let xf0 = sel.xf;
        let z = st.zoom;
        let l = st.to_layout(s.x, s.y);
        let p = Pt::new(l.x - origin.x, l.y - origin.y);
        let (_, rh) = sel.rotate_handle(SEL_PAD / z, ROT_OFF / z);
        if rh.dist(p) * z < 18.0 {
            let center = sel.center();
            st.action = Some(Action::SelRotate { center, a0: (p.y - center.y).atan2(p.x - center.x), xf0 });
            return true;
        }
        let corners = sel.corners(0.0);
        let inside = sel.base.expand(6.0 / (z * xf0.scale())).contains(xf0.inverse().apply(p));
        for (k, c) in corners.iter().enumerate() {
            if c.dist(p) * z < 16.0 {
                let anchor = corners[(k + 2) % 4];
                st.action = Some(Action::SelScale { anchor, start_d: anchor.dist(p).max(1e-3), xf0 });
                return true;
            }
        }
        if inside {
            st.action = Some(Action::SelMove { start: p, xf0 });
            return true;
        }
        false
    }

    // ---------------------------------------------------------------- Bewegung

    pub(super) fn motion(&self, samples: &[Sample]) {
        let Some(last) = samples.last() else { return };
        let mut st = self.st();
        st.hover = Some((last.x, last.y));
        let zoom = st.zoom;
        let smoothing = st.cfg.smoothing;
        let pred_ms = st.cfg.prediction_ms;
        let mut erase: Option<(Pt, Pt)> = None;
        let cam = st.cam;
        let mut new_ws = Vec::with_capacity(samples.len());
        if let Some(Action::Draw(live)) = &st.action {
            for s in samples {
                new_ws.push(if live.pressure { st.width_for(live.width, s.pressure, s.tilt) } else { live.width });
            }
        }
        let tap_max = st.cfg.tap_mm * 72.0 / 25.4 * zoom;
        if let Some((_, (x0, y0), far)) = &mut st.tap {
            *far |= ((last.x - *x0).powi(2) + (last.y - *y0).powi(2)).sqrt() > tap_max;
        }
        let to_page = |origin: Pt, s: &Sample| Pt::new(cam.x + s.x / zoom - origin.x, cam.y + s.y / zoom - origin.y);
        let mut sel_xf: Option<Xform> = None;
        let mut image_erase: Option<(Pt, Pt)> = None;
        let mut relayout = false;
        let sel_origin = st.sel.as_ref().and_then(|s| st.doc.index_of(&s.page)).map(|i| st.page_origin(i));
        match &mut st.action {
            Some(Action::Draw(live)) => {
                let min_d = 0.35 / zoom;
                let mut grew = false;
                let raw_before = live.raw;
                // Zeit pro Punkt: die Punkte eines Ereignisses teilen sich die Zeit seit dem letzten
                let now = Instant::now();
                let dt = (now.duration_since(live.ft).as_secs_f64() / samples.len().max(1) as f64).clamp(0.001, 0.05);
                live.ft = now;
                for (s, w) in samples.iter().zip(new_ws) {
                    let l = to_page(live.origin, s);
                    let prev_raw = live.raw;
                    live.raw = l;
                    live.htime += dt;
                    live.hist.push_back((live.htime, l));
                    while live.hist.front().map(|h| live.htime - h.0 > 0.08).unwrap_or(false) {
                        live.hist.pop_front();
                    }
                    live.raw_w = live.raw_w * 0.6 + w * 0.4;
                    if l.dist(live.still_at) > 2.0 / zoom {
                        live.still_at = l;
                        live.still_since = Instant::now();
                    }
                    if live.frozen {
                        continue;
                    }
                    if live.straight {
                        // begradigte Linie folgt dem Stift
                        let n = live.pts.len();
                        live.pts[n - 1] = shapes::snap_angle(live.pts[0], l);
                        grew = true;
                        continue;
                    }
                    let lp = *live.pts.last().unwrap();
                    let sm = if smoothing <= 0.001 { l } else { one_euro(live, prev_raw, l, lp, dt, smoothing) };
                    if sm.dist(lp) < min_d {
                        continue;
                    }
                    live.pts.push(sm);
                    live.ws.push(live.raw_w);
                    grew = true;
                }
                if grew {
                    if live.straight { live.rerender_all() } else { live.update_open() }
                }
                let dt = live.vel_t.elapsed().as_secs_f64();
                live.vel_t = Instant::now();
                if dt > 0.0005 {
                    let v = ((live.raw.x - raw_before.x) / dt, (live.raw.y - raw_before.y) / dt);
                    live.vel = (live.vel.0 * 0.5 + v.0 * 0.5, live.vel.1 * 0.5 + v.1 * 0.5);
                    live.update_prediction(zoom, pred_ms);
                }
            }
            Some(Action::Erase { origin, last: prev, .. }) => {
                let p = to_page(*origin, last);
                erase = Some((*prev, p));
                *prev = p;
            }
            Some(Action::Pan { last: lp }) => {
                let (lx, ly) = *lp;
                *lp = (last.x, last.y);
                st.cam.x -= (last.x - lx) / zoom;
                st.cam.y -= (last.y - ly) / zoom;
                drop(st);
                self.changed();
                return;
            }
            Some(Action::Lasso { origin, pts, .. }) => {
                for s in samples {
                    let p = to_page(*origin, s);
                    if pts.last().map(|q| q.dist(p) * zoom > 1.5).unwrap_or(true) {
                        pts.push(p);
                    }
                }
            }
            Some(Action::SelMove { start, xf0 }) => {
                if let Some(o) = sel_origin {
                    let p = to_page(o, last);
                    sel_xf = Some(xf0.then(&Xform::translate(p.x - start.x, p.y - start.y)));
                }
            }
            Some(Action::SelScale { anchor, start_d, xf0 }) => {
                if let Some(o) = sel_origin {
                    let p = to_page(o, last);
                    let k = (anchor.dist(p) / *start_d).clamp(0.05, 20.0);
                    sel_xf = Some(xf0.then(&Xform::scale_about(k, *anchor)));
                }
            }
            Some(Action::ImageErase { last: prev, .. }) => {
                if let Some(o) = sel_origin {
                    let p = to_page(o, last);
                    image_erase = Some((*prev, p));
                    *prev = p;
                }
            }
            Some(Action::SelRotate { center, a0, xf0 }) => {
                if let Some(o) = sel_origin {
                    let p = to_page(o, last);
                    let mut d = (p.y - center.y).atan2(p.x - center.x) - *a0;
                    // nahe an Vielfachen von 45° einrasten
                    let total = xf0.angle() + d;
                    let step = PI / 4.0;
                    let snapped = (total / step).round() * step;
                    if (snapped - total).abs() < 3f64.to_radians() {
                        d += snapped - total;
                    }
                    sel_xf = Some(xf0.then(&Xform::rotate_about(d, *center)));
                }
            }
            Some(Action::Shape { origin, kind, a, b, .. }) => {
                *b = shapes::constrain(*kind, *a, to_page(*origin, last));
            }
            Some(Action::Space { origin, y, min_y, .. }) => {
                *y = to_page(*origin, last).y.max(*min_y);
            }
            Some(Action::PageHeight { page, h0, min_h, y0 }) => {
                let ly = cam.y + last.y / zoom;
                let h = (*h0 + ly - *y0).max(*min_h);
                page.borrow_mut().height = h;
                relayout = true;
            }
            Some(Action::TextMark { .. }) => {
                Canvas::text_mark_motion(&mut st, last);
            }
            Some(Action::Radial { center, moved }) => {
                let c = *center;
                if (last.x - c.0).hypot(last.y - c.1) > R_CENTER {
                    *moved = true;
                }
                if let Some(r) = &mut st.radial {
                    r.hover = radial_hit(c, (last.x, last.y));
                }
            }
            None => {}
        }
        if let (Some(xf), Some(sel)) = (sel_xf, &mut st.sel) {
            sel.xf = xf;
        }
        if relayout {
            st.relayout();
        }
        drop(st);
        if let Some((a, b)) = erase {
            self.erase_at(a, b);
        }
        if let Some((a, b)) = image_erase {
            self.image_erase_segment(a, b);
        }
        self.queue_draw();
    }

    // ---------------------------------------------------------------- Ende

    pub(super) fn end(&self, s: Option<Sample>) {
        if let Some(s) = s {
            self.motion(&[s]);
        }
        self.stop_hold_timer();
        let action = self.st().action.take();
        let tap = {
            let mut st = self.st();
            let ms = st.cfg.tap_ms;
            st.tap.take().filter(|(t, _, far)| !far && t.elapsed() < Duration::from_millis(ms)).map(|(_, at, _)| at)
        };
        let action = match (tap, action) {
            (Some(at), Some(a)) => match self.tap_action(at, a) {
                Some(a) => Some(a),
                None => {
                    self.changed();
                    return;
                }
            },
            (_, a) => a,
        };
        match action {
            Some(Action::Draw(live)) => self.finish_stroke(live),
            Some(Action::Erase { page, orig, .. }) => {
                let mut st = self.st();
                let changes: Vec<Change> = {
                    let p = page.borrow();
                    orig.into_iter()
                        .enumerate()
                        .filter(|(i, old)| *i < p.layers.len() && !Arc::ptr_eq(old, &p.layers[*i].items))
                        .map(|(i, old)| Change::Items { page: page.clone(), layer: i, old, new: p.layers[i].items.clone() })
                        .collect()
                };
                st.history.push(changes);
            }
            Some(Action::Lasso { page, pts, .. }) if self.st().tools.tool == Tool::Ask => self.finish_ask(page, pts),
            Some(Action::Lasso { page, pts, rect, .. }) => self.finish_lasso(page, pts, rect),
            Some(Action::SelMove { .. }) => self.finish_selection_move(),
            Some(Action::SelScale { .. }) | Some(Action::SelRotate { .. }) => self.render_selection(),
            Some(Action::ImageErase { parts, .. }) => self.finish_image_erase(parts),
            Some(Action::Shape { page, kind, a, b, .. }) => {
                if a.dist(b) * self.st().zoom > 3.0 {
                    let (color, width, attrs) = self.pen_stroke_style();
                    let strokes = shapes::build(kind, a, b, width)
                        .into_iter()
                        .map(|pts| Item::Stroke(Stroke::new(StrokeTool::Pen, color, width, pts, None, attrs.clone())))
                        .collect();
                    self.add_items(&page, strokes);
                }
            }
            Some(Action::Space { page, y0, y, .. }) => self.insert_space(&page, y0, y - y0),
            Some(Action::TextMark { page, text, ia, ib, .. }) => self.finish_text_mark(page, text, ia, ib),
            Some(Action::PageHeight { page, h0, .. }) => {
                let mut st = self.st();
                let (w, h, bg) = {
                    let p = page.borrow();
                    (p.width, p.height, p.bg.clone())
                };
                if (h - h0).abs() > 0.5 {
                    page.borrow_mut().mark_ink(None);
                    page.borrow_mut().bg_ver = next_version();
                    st.history.push(vec![Change::Page { page: page.clone(), old: (w, h0, bg.clone()), new: (w, h, bg) }]);
                }
                st.relayout();
            }
            Some(Action::Radial { center, moved }) => {
                let hover = self.st().radial.as_ref().and_then(|r| r.hover);
                match (hover, moved) {
                    // losgelassen über einem Eintrag: auswählen (wie ein Marking-Menü)
                    (Some(item), true) => {
                        self.st().radial = None;
                        self.apply_radial(item, center);
                    }
                    (_, false) => {
                        // nur getippt: Menü bleibt zum Antippen offen
                        self.st().radial = Some(RadialMenu { center, hover: None });
                    }
                    _ => self.st().radial = None,
                }
            }
            _ => {}
        }
        self.changed();
    }

    fn pen_stroke_style(&self) -> (Color, f64, Attrs) {
        let st = self.st();
        let p = &st.tools.pen;
        let mut attrs: Attrs = vec![("capStyle".into(), "round".into())];
        if let Some(style) = p.style.attr() {
            attrs.push(("style".into(), style.into()));
        }
        if p.fill {
            attrs.push(("fill".into(), "64".into()));
        }
        (p.color, p.width, attrs)
    }

    /// Objekt unter `p` auswählen; true, wenn etwas getroffen wurde.
    fn select_at(&self, page: &PageRef, p: Pt) -> bool {
        let zoom = self.st().zoom;
        let hit = {
            let pg = page.borrow();
            pg.layers.iter().enumerate().rev().find_map(|(li, l)| {
                l.items.iter().enumerate().rev().find(|(_, it)| !it.locked() && it.hit(p, 5.0 / zoom)).map(|(i, _)| (li, i))
            })
        };
        match hit {
            Some(pick) => {
                self.make_selection(page.clone(), vec![pick]);
                true
            }
            None => false,
        }
    }

    /// Kurzer Tipp mit Radierer, Auswahl, Form, Platz oder Hand: Objekt wählen bzw.
    /// Radialmenü öffnen. Gibt die Aktion zurück, wenn sie normal enden soll.
    fn tap_action(&self, at: (f64, f64), action: Action) -> Option<Action> {
        let (try_select, open) = {
            let cfg = &self.st().cfg;
            (cfg.tap_select, cfg.tap_radial)
        };
        match &action {
            // ein Tipp, der wirklich etwas radiert hat, bleibt ein Radieren
            Action::Erase { page, orig, .. } => {
                let p = page.borrow();
                if orig.iter().zip(&p.layers).any(|(o, l)| !Arc::ptr_eq(o, &l.items)) {
                    drop(p);
                    return Some(action);
                }
            }
            Action::Lasso { page, pts, .. } => {
                if try_select && self.select_at(page, pts[0]) {
                    return None;
                }
            }
            Action::Shape { .. } | Action::Space { .. } | Action::Pan { .. } => {}
            _ => return Some(action),
        }
        if !open {
            return Some(action);
        }
        self.st().radial = Some(RadialMenu { center: at, hover: None });
        None
    }

    fn finish_stroke(&self, mut live: Live) {
        // Kurzes Tippen (wie der Strichfilter in Xournal++): kein Strich, sondern
        // Objekt auswählen bzw. Radialmenü öffnen. Ein Tipp kurz nach einem Strich
        // bleibt ein Punkt (i-Punkt, Satzzeichen).
        let (is_tap, try_select, open) = {
            let st = self.st();
            let cfg = &st.cfg;
            let len: f64 = live.pts.windows(2).map(|w| w[0].dist(w[1])).sum::<f64>().max(live.raw.dist(live.pts[0]));
            let quick = live.started.elapsed() < Duration::from_millis(cfg.tap_ms) && len < cfg.tap_mm * 72.0 / 25.4;
            let after_stroke =
                st.last_stroke_end.map(|t| t.elapsed() < Duration::from_millis(cfg.tap_successive_ms)).unwrap_or(false);
            (quick && !after_stroke && (cfg.tap_radial || cfg.tap_select), cfg.tap_select, cfg.tap_radial)
        };
        self.st().last_stroke_end = Some(Instant::now());
        if is_tap {
            if try_select && self.select_at(&live.page, live.pts[0]) {
                return;
            }
            if open {
                self.st().radial = Some(RadialMenu { center: live.start_screen, hover: None });
                return;
            }
        }
        if !live.straight && !live.frozen {
            let last = *live.pts.last().unwrap();
            if live.raw.dist(last) > 0.05 {
                live.pts.push(live.raw);
                live.ws.push(live.raw_w);
                live.update_open();
            }
            // automatische Formerkennung (optional)
            let min = self.st().cfg.shape_min_mm * 72.0 / 25.4;
            let big = Rect::bounding(&live.pts).map(|b| b.width().max(b.height()) >= min).unwrap_or(false);
            if self.st().cfg.auto_shapes && live.tool == StrokeTool::Pen && big {
                if let Some(shape) = shapes::recognize(&live.pts, false) {
                    let w = live.ws.iter().sum::<f64>() / live.ws.len() as f64;
                    live.ws = vec![w; shape.len()];
                    live.pts = shape;
                    live.pressure = false;
                    live.width = w;
                    live.rerender_all();
                }
            }
        }
        let stroke = live.stroke_of(&live.pts, &live.ws, false);
        let mut st = self.st();
        let page = live.page.clone();
        let layer = page.borrow().current_layer();
        let mut items: Vec<ItemRef> = (*page.borrow().layers[layer].items).clone();
        let rect = stroke.bbox.translate(live.origin.x, live.origin.y);
        items.push(Arc::new(Item::Stroke(stroke)));
        st.push_items_change(&page, layer, Arc::new(items));
        let ver = page.borrow().ink_ver;
        let id = page.borrow().id;
        let alpha = live.color.a;
        let tex: Vec<_> = live.all_textures().cloned().collect();
        st.pending.push(Pending { page: id, ver, rect, alpha, tex });
        drop(st);
        self.maybe_append_page(&page);
    }

    /// Fügt Elemente der aktuellen Ebene einer Seite hinzu (ein Undo-Schritt).
    pub(super) fn add_items(&self, page: &PageRef, new: Vec<Item>) {
        let mut st = self.st();
        let layer = page.borrow().current_layer();
        let mut items: Vec<ItemRef> = (*page.borrow().layers[layer].items).clone();
        items.extend(new.into_iter().map(Arc::new));
        st.push_items_change(page, layer, Arc::new(items));
    }

    // ---------------------------------------------------------------- Radierer

    fn erase_at(&self, a: Pt, b: Pt) {
        let st = self.st();
        let Some(Action::Erase { page, .. }) = &st.action else { return };
        let page = page.clone();
        let whole = st.tools.eraser_mode == EraserMode::Stroke;
        // Strich-Radierer hat keinen Radius: gelöscht wird, was die Spitze kreuzt
        let r = if whole { 1.5 / st.zoom } else { st.tools.eraser_size };
        drop(st);
        let steps = ((a.dist(b) / (r * 0.5)).ceil() as usize).max(1);
        let pts: Vec<Pt> = (0..=steps).map(|k| a.lerp(b, k as f64 / steps as f64)).collect();
        let area = Rect::bounding(&pts).unwrap().expand(r);
        let mut p = page.borrow_mut();
        for li in 0..p.layers.len() {
            let items = p.layers[li].items.clone();
            let mut out: Vec<ItemRef> = Vec::with_capacity(items.len());
            let mut changed = false;
            for it in items.iter() {
                let Item::Stroke(s) = &**it else {
                    out.push(it.clone());
                    continue;
                };
                if !s.bbox.intersects(&area) || it.locked() {
                    out.push(it.clone());
                    continue;
                }
                if whole {
                    if pts.iter().any(|q| s.hit(*q, r)) {
                        changed = true;
                    } else {
                        out.push(it.clone());
                    }
                    continue;
                }
                let mut parts: Vec<Stroke> = vec![s.clone()];
                let mut touched = false;
                for q in &pts {
                    let mut next = Vec::new();
                    for part in parts {
                        match part.erase_circle(*q, r) {
                            Some(rest) => {
                                touched = true;
                                next.extend(rest);
                            }
                            None => next.push(part),
                        }
                    }
                    parts = next;
                }
                if touched {
                    changed = true;
                    out.extend(parts.into_iter().map(|s| Arc::new(Item::Stroke(s))));
                } else {
                    out.push(it.clone());
                }
            }
            if changed {
                p.set_items(li, Arc::new(out));
            }
        }
    }

    // ---------------------------------------------------------------- Halten

    pub(super) fn start_hold_timer(&self) {
        self.stop_hold_timer();
        let weak = self.downgrade();
        let id = glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(c) = weak.upgrade() else { return glib::ControlFlow::Break };
            let mut st = c.st();
            let zoom = st.zoom;
            let cfg = st.cfg.clone();
            let Some(Action::Draw(live)) = &mut st.action else {
                c.imp().hold_timer.take();
                return glib::ControlFlow::Break;
            };
            let len: f64 = live.pts.windows(2).map(|w| w[0].dist(w[1])).sum();
            // langer Druck auf eine Stelle: Radialmenü statt Punkt
            if cfg.radial_longpress && len * zoom < 4.0 && live.started.elapsed() >= Duration::from_millis(cfg.radial_delay_ms) {
                let center = live.start_screen;
                st.action = Some(Action::Radial { center, moved: false });
                st.radial = Some(RadialMenu { center, hover: None });
                drop(st);
                c.imp().hold_timer.take();
                c.changed();
                return glib::ControlFlow::Break;
            }
            if !cfg.hold_to_straighten || live.straight || live.still_since.elapsed() < Duration::from_millis(cfg.hold_delay_ms) {
                return if cfg.hold_to_straighten || len * zoom < 4.0 { glib::ControlFlow::Continue } else { glib::ControlFlow::Break };
            }
            if len * zoom < 30.0 {
                return glib::ControlFlow::Continue;
            }
            // Halten: Form erkennen, sonst gerade Linie, die dem Stift weiter folgt
            let w = live.ws.iter().sum::<f64>() / live.ws.len() as f64;
            match shapes::recognize(&live.pts, false).filter(|s| s.len() > 2) {
                Some(shape) => {
                    live.pts = shape;
                    live.frozen = true;
                }
                None => {
                    live.pts = vec![live.pts[0], shapes::snap_angle(live.pts[0], live.raw)];
                }
            }
            live.ws = vec![w; live.pts.len()];
            live.pressure = false;
            live.width = w;
            live.straight = true;
            live.pred = None;
            live.rerender_all();
            drop(st);
            c.queue_draw();
            c.imp().hold_timer.take();
            glib::ControlFlow::Break
        });
        self.imp().hold_timer.replace(Some(id));
    }

    pub(super) fn stop_hold_timer(&self) {
        if let Some(id) = self.imp().hold_timer.take() {
            id.remove();
        }
    }

    // ---------------------------------------------------------------- Auswahl

    fn finish_lasso(&self, page: PageRef, pts: Vec<Pt>, rect: bool) {
        let zoom = self.st().zoom;
        let len: f64 = pts.windows(2).map(|w| w[0].dist(w[1])).sum();
        let p = page.borrow();
        let mut picks = Vec::new();
        if len * zoom < 6.0 {
            // Tippen: oberstes Element unter dem Punkt
            let q = pts[0];
            'outer: for (li, l) in p.layers.iter().enumerate().rev() {
                for (i, it) in l.items.iter().enumerate().rev() {
                    if !it.locked() && it.hit(q, 4.0 / zoom) {
                        picks.push((li, i));
                        break 'outer;
                    }
                }
            }
        } else {
            let poly: Vec<Pt> = if rect {
                let r = Rect::from_pts(pts[0], *pts.last().unwrap());
                vec![Pt::new(r.x0, r.y0), Pt::new(r.x1, r.y0), Pt::new(r.x1, r.y1), Pt::new(r.x0, r.y1)]
            } else {
                pts
            };
            for (li, l) in p.layers.iter().enumerate() {
                for (i, it) in l.items.iter().enumerate() {
                    if it.locked() {
                        continue;
                    }
                    let inside = match &**it {
                        Item::Stroke(s) => {
                            let n = s.pts.iter().filter(|q| point_in_polygon(**q, &poly)).count();
                            n as f64 >= s.pts.len() as f64 * 0.7
                        }
                        other => other.bbox().map(|b| point_in_polygon(b.center(), &poly)).unwrap_or(false),
                    };
                    if inside {
                        picks.push((li, i));
                    }
                }
            }
        }
        drop(p);
        if !picks.is_empty() {
            self.make_selection(page, picks);
        }
    }

    fn make_selection(&self, page: PageRef, picks: Vec<(usize, usize)>) {
        {
            let mut st = self.st();
            let orig: Vec<ItemList> = page.borrow().layers.iter().map(|l| l.items.clone()).collect();
            let entries: Vec<(usize, usize, ItemRef)> = picks.iter().map(|(l, i)| (*l, *i, orig[*l][*i].clone())).collect();
            for (li, list) in orig.iter().enumerate() {
                if entries.iter().any(|e| e.0 == li) {
                    let keep: Vec<ItemRef> =
                        list.iter().enumerate().filter(|(i, _)| !picks.contains(&(li, *i))).map(|(_, it)| it.clone()).collect();
                    page.borrow_mut().set_items(li, Arc::new(keep));
                }
            }
            let base = entries.iter().filter_map(|e| e.2.bbox()).reduce(|a, b| a.union(&b)).unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
            st.sel = Some(Selection { page, orig, entries, xf: Xform::IDENTITY, base, modified: false, deleted: false, merge_undo: false, tex: None });
            if st.tools.tool != Tool::Select && st.tools.tool != Tool::Pen && st.tools.tool != Tool::Highlighter {
                st.tools.tool = Tool::Select;
            }
        }
        self.render_selection();
    }

    /// Neue (eingefügte) Elemente als Auswahl auf eine Seite setzen.
    fn select_new(&self, page: PageRef, items: Vec<Item>) {
        self.commit_edits();
        {
            let mut st = self.st();
            let orig: Vec<ItemList> = page.borrow().layers.iter().map(|l| l.items.clone()).collect();
            let layer = page.borrow().current_layer();
            let entries: Vec<(usize, usize, ItemRef)> = items.into_iter().map(|it| (layer, usize::MAX, Arc::new(it))).collect();
            let base = entries.iter().filter_map(|e| e.2.bbox()).reduce(|a, b| a.union(&b)).unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
            st.sel = Some(Selection { page, orig, entries, xf: Xform::IDENTITY, base, modified: true, deleted: false, merge_undo: false, tex: None });
            st.tools.tool = Tool::Select;
        }
        self.render_selection();
        self.changed();
    }

    pub fn render_selection(&self) {
        let mut st = self.st();
        let scale = st.zoom * st.sf;
        let dark = st.dark_paper;
        let Some(sel) = &st.sel else { return };
        let items = sel.transformed_items();
        let bb = items.iter().filter_map(|i| i.bbox()).reduce(|a, b| a.union(&b));
        let tex = bb.and_then(|bb| {
            texture_for(
                |cr| {
                    for it in &items {
                        render::draw_item_in(cr, it, None, dark);
                    }
                },
                bb.expand(1.0),
                scale,
                Pt::new(0.0, 0.0),
            )
        });
        let xf = sel.xf;
        if let Some(sel) = &mut st.sel {
            sel.tex = tex.map(|(t, r)| (t, r, xf, scale));
        }
    }

    /// Legt die Auswahl zurück ins Dokument (mit Undo-Schritt, falls verändert).
    pub fn commit_selection(&self) {
        let Some(sel) = self.st().sel.take() else { return };
        let mut changes = Vec::new();
        let changed = sel.modified || sel.deleted || !sel.xf.is_identity();
        let layer_ids: Vec<usize> = {
            let mut v: Vec<usize> = sel.entries.iter().map(|e| e.0).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        for li in layer_ids {
            let old = sel.orig[li].clone();
            if !changed {
                sel.page.borrow_mut().set_items(li, old);
                continue;
            }
            let mut new: Vec<ItemRef> = Vec::with_capacity(old.len() + sel.entries.len());
            for (i, it) in old.iter().enumerate() {
                match sel.entries.iter().find(|e| e.0 == li && e.1 == i) {
                    Some(e) if !sel.deleted => new.push(Arc::new(e.2.transformed(&sel.xf))),
                    Some(_) => {}
                    None => new.push(it.clone()),
                }
            }
            if !sel.deleted {
                for e in sel.entries.iter().filter(|e| e.0 == li && e.1 == usize::MAX) {
                    new.push(Arc::new(e.2.transformed(&sel.xf)));
                }
            }
            let new = Arc::new(new);
            sel.page.borrow_mut().set_items(li, new.clone());
            changes.push(Change::Items { page: sel.page.clone(), layer: li, old, new });
        }
        let mut st = self.st();
        if sel.merge_undo {
            for c in changes {
                st.history.append_to_last(c);
            }
        } else {
            st.history.push(changes);
        }
        drop(st);
        self.changed();
    }

    /// Auswahl samt Seitenhintergrund als PNG (zum Mitschicken an Claude).
    pub fn selection_png(&self) -> Option<Vec<u8>> {
        let st = self.st();
        let sel = st.sel.as_ref()?;
        let r = sel.rect().expand(10.0);
        let scale = 2.5;
        let (w, h) = ((r.width() * scale).ceil() as i32, (r.height() * scale).ceil() as i32);
        if w < 1 || h < 1 || w > 8000 || h > 8000 {
            return None;
        }
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
        {
            let cr = cairo::Context::new(&surf).ok()?;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().ok()?;
            cr.scale(scale, scale);
            cr.translate(-r.x0, -r.y0);
            let page = sel.page.borrow();
            render::draw_page(&cr, &page.snapshot(st.doc.page_pdf(&page)), true);
            for it in sel.transformed_items() {
                render::draw_item_in(&cr, &it, None, false);
            }
        }
        let mut png = Vec::new();
        surf.write_to_png(&mut png).ok()?;
        Some(png)
    }

    pub fn has_selection(&self) -> bool {
        self.st().sel.is_some()
    }

    pub fn delete_selection(&self) {
        if let Some(sel) = &mut self.st().sel {
            sel.deleted = true;
        } else {
            return;
        }
        self.commit_selection();
    }

    pub fn copy_selection(&self) -> bool {
        let mut st = self.st();
        let Some(sel) = &st.sel else { return false };
        st.clip = sel.transformed_items();
        true
    }

    pub fn cut_selection(&self) {
        if self.copy_selection() {
            self.delete_selection();
        }
    }

    /// Fügt die interne Zwischenablage ein: an der Stiftposition oder versetzt.
    pub fn paste(&self) -> bool {
        let (page, items) = {
            let st = self.st();
            if st.clip.is_empty() {
                return false;
            }
            let Some(bb) = st.clip.iter().filter_map(|i| i.bbox()).reduce(|a, b| a.union(&b)) else { return false };
            let target = st.hover.and_then(|(x, y)| st.page_at(x, y, 0.0));
            let (i, dx, dy) = match target {
                Some((i, p)) => (i, p.x - bb.x0, p.y - bb.y0),
                None => (st.current_page(), 12.0, 12.0),
            };
            let xf = Xform::translate(dx, dy);
            (st.doc.pages[i].clone(), st.clip.iter().map(|it| it.transformed(&xf)).collect::<Vec<_>>())
        };
        self.select_new(page, items);
        true
    }

    pub fn duplicate_selection(&self) {
        let (page, items) = {
            let st = self.st();
            let Some(sel) = &st.sel else { return };
            let xf = Xform::translate(12.0, 12.0);
            (sel.page.clone(), sel.transformed_items().iter().map(|it| it.transformed(&xf)).collect::<Vec<_>>())
        };
        self.select_new(page, items);
    }

    pub fn select_all(&self) {
        self.commit_edits();
        let page = {
            let st = self.st();
            st.doc.pages[st.current_page()].clone()
        };
        let picks: Vec<(usize, usize)> = page
            .borrow()
            .layers
            .iter()
            .enumerate()
            .flat_map(|(li, l)| l.items.iter().enumerate().filter(|(_, it)| !it.locked()).map(move |(i, _)| (li, i)))
            .collect();
        if !picks.is_empty() {
            self.make_selection(page, picks);
            self.st().tools.tool = Tool::Select;
            self.changed();
        }
    }

    /// Auswahl fixieren („einbrennen“): danach nicht mehr auswählbar oder radierbar.
    pub fn lock_selection(&self) {
        {
            let mut st = self.st();
            let Some(sel) = &mut st.sel else { return };
            for e in &mut sel.entries {
                e.2 = Arc::new(e.2.with_locked(true));
            }
            sel.modified = true;
        }
        self.commit_selection();
    }

    /// Fixierungen auf der aktuellen Seite lösen; liefert die Anzahl gelöster Elemente.
    pub fn unlock_page(&self) -> usize {
        self.commit_edits();
        let page = {
            let st = self.st();
            st.doc.pages[st.current_page()].clone()
        };
        let mut n = 0;
        let mut changes = Vec::new();
        let layers = page.borrow().layers.len();
        for li in 0..layers {
            let old = page.borrow().layers[li].items.clone();
            if !old.iter().any(|it| it.locked()) {
                continue;
            }
            let new: ItemList = Arc::new(
                old.iter()
                    .map(|it| {
                        if it.locked() {
                            n += 1;
                            Arc::new(it.with_locked(false))
                        } else {
                            it.clone()
                        }
                    })
                    .collect(),
            );
            page.borrow_mut().set_items(li, new.clone());
            changes.push(Change::Items { page: page.clone(), layer: li, old, new });
        }
        if !changes.is_empty() {
            self.st().history.push(changes);
            self.changed();
        }
        n
    }

    pub fn recolor_selection(&self, c: Color) -> bool {
        {
            let mut st = self.st();
            let Some(sel) = &mut st.sel else { return false };
            for e in &mut sel.entries {
                e.2 = Arc::new(e.2.recolored(c));
            }
            sel.modified = true;
        }
        self.render_selection();
        self.changed();
        true
    }

    pub fn nudge_selection(&self, dx: f64, dy: f64) -> bool {
        {
            let mut st = self.st();
            let Some(sel) = &mut st.sel else { return false };
            sel.xf = sel.xf.then(&Xform::translate(dx, dy));
        }
        self.render_selection();
        self.changed();
        true
    }

    /// Drehgriff der Auswahl in Widget-Koordinaten (für den Selbsttest).
    pub(super) fn selection_rotate_handle(&self) -> Option<(f64, f64)> {
        let st = self.st();
        let sel = st.sel.as_ref()?;
        let o = st.page_origin(st.doc.index_of(&sel.page)?);
        let z = st.zoom;
        let (_, h) = sel.rotate_handle(SEL_PAD / z, ROT_OFF / z);
        Some(((o.x + h.x - st.cam.x) * z, (o.y + h.y - st.cam.y) * z))
    }

    /// Auswahl-Rechteck in Widget-Koordinaten (für die schwebende Auswahlleiste).
    pub fn selection_screen_rect(&self) -> Option<Rect> {
        let st = self.st();
        let sel = st.sel.as_ref()?;
        if st.action.is_some() {
            return None;
        }
        let o = st.page_origin(st.doc.index_of(&sel.page)?);
        let r = sel.rect().translate(o.x, o.y);
        let z = st.zoom;
        Some(Rect::new((r.x0 - st.cam.x) * z, (r.y0 - st.cam.y) * z, (r.x1 - st.cam.x) * z, (r.y1 - st.cam.y) * z))
    }

    /// Schließt alle offenen Bearbeitungen (vor Speichern, Undo, Werkzeugwechsel ...).
    pub fn commit_edits(&self) {
        self.commit_text();
        self.commit_selection();
    }

    // ---------------------------------------------------------------- Text

    fn start_text(&self, page_idx: usize, p: Pt) {
        let mut st = self.st();
        let page = st.doc.pages[page_idx].clone();
        // bestehenden Text antippen = bearbeiten
        let hit = {
            let pg = page.borrow();
            pg.layers.iter().enumerate().rev().find_map(|(li, l)| {
                l.items.iter().enumerate().rev().find_map(|(i, it)| match &**it {
                    Item::Text(t) if !it.locked() && t.bbox().expand(2.0).contains(p) => Some((li, i, t.clone())),
                    _ => None,
                })
            })
        };
        let te = match hit {
            Some((li, i, t)) => {
                let old = page.borrow().layers[li].items.clone();
                let mut rest: Vec<ItemRef> = (*old).clone();
                rest.remove(i);
                page.borrow_mut().set_items(li, Arc::new(rest));
                TextEdit {
                    page,
                    pos: Pt::new(t.x, t.y),
                    font: t.font.clone(),
                    size: t.size,
                    color: t.color,
                    orig: Some((li, i, old)),
                    initial: t.text.clone(),
                }
            }
            None => TextEdit {
                page,
                pos: p,
                font: st.tools.text_font.clone(),
                size: st.tools.text_size,
                color: st.tools.pen.color,
                orig: None,
                initial: String::new(),
            },
        };
        st.text = Some(te);
        drop(st);
        self.changed();
    }

    /// Beendet die Texteingabe und übernimmt den Text aus dem Editor des Fensters.
    pub fn commit_text(&self) {
        if self.st().text.is_none() {
            return;
        }
        let text = self.imp().text_source.borrow().as_ref().map(|f| f()).unwrap_or_default();
        let Some(te) = self.st().text.take() else { return };
        let page = te.page.clone();
        let layer = te.orig.as_ref().map(|o| o.0).unwrap_or_else(|| page.borrow().current_layer());
        let current = page.borrow().layers[layer].items.clone();
        let old = te.orig.as_ref().map(|o| o.2.clone()).unwrap_or_else(|| current.clone());
        let mut new: Vec<ItemRef> = (*current).clone();
        let trimmed = text.trim_end().to_string();
        if !trimmed.is_empty() {
            let item = Arc::new(Item::Text(TextItem::new(te.pos.x, te.pos.y, trimmed, te.font, te.size, te.color, Vec::new())));
            match &te.orig {
                Some((_, i, _)) => new.insert((*i).min(new.len()), item),
                None => new.push(item),
            }
        }
        let unchanged = te.orig.is_some() && text.trim_end() == te.initial;
        let mut st = self.st();
        if unchanged {
            page.borrow_mut().set_items(layer, old);
        } else if new.len() != old.len() || te.orig.is_some() {
            let new = Arc::new(new);
            page.borrow_mut().set_items(layer, new.clone());
            st.history.push(vec![Change::Items { page: page.clone(), layer, old, new }]);
        }
        drop(st);
        self.changed();
    }

    pub fn cancel_text(&self) {
        let Some(te) = self.st().text.take() else { return };
        if let Some((li, _, old)) = te.orig {
            te.page.borrow_mut().set_items(li, old);
        }
        self.changed();
    }

    /// Position, Schriftgröße (px) und Inhalt des Text-Editors, falls aktiv.
    pub fn text_edit_info(&self) -> Option<(f64, f64, f64, Color, String, String)> {
        let st = self.st();
        let te = st.text.as_ref()?;
        let o = st.page_origin(st.doc.index_of(&te.page)?);
        let z = st.zoom;
        Some((
            (o.x + te.pos.x - st.cam.x) * z,
            (o.y + te.pos.y - st.cam.y) * z,
            te.size * z,
            te.color,
            te.font.clone(),
            te.initial.clone(),
        ))
    }

    // ---------------------------------------------------------------- Bilder

    /// Fügt ein Bild (beliebiges Format) ein und wählt es zum Verschieben aus.
    pub fn insert_image(&self, page_idx: usize, pos: Pt, bytes: &[u8]) -> Result<(), String> {
        // 96 dpi → pt
        self.insert_image_scaled(page_idx, pos, bytes, 0.75)
    }

    /// Wie `insert_image`, mit eigener Größe: `pt_per_px` Punkte je Bildpunkt.
    pub fn insert_image_scaled(&self, page_idx: usize, pos: Pt, bytes: &[u8], pt_per_px: f64) -> Result<(), String> {
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(bytes).map_err(|e| e.to_string())?;
        loader.close().map_err(|e| e.to_string())?;
        let pb = loader.pixbuf().ok_or(crate::i18n::tl("Bild kann nicht gelesen werden"))?;
        let png = pb.save_to_bufferv("png", &[]).map_err(|e| e.to_string())?;
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let page = self.st().doc.pages.get(page_idx).cloned().ok_or(crate::i18n::tl("Seite fehlt"))?;
        let (pw, ph) = {
            let p = page.borrow();
            (p.width, p.height)
        };
        // höchstens 70 % der Seitenbreite
        let mut w = pb.width() as f64 * pt_per_px;
        let mut h = pb.height() as f64 * pt_per_px;
        let k = (pw * 0.7 / w).min(ph * 0.7 / h).min(1.0);
        w *= k;
        h *= k;
        let x = pos.x.min(pw - w).max(0.0);
        let y = pos.y.min(ph - h).max(0.0);
        let item = Item::Image(ImageItem::new(Rect::new(x, y, x + w, y + h), Arc::new(data), Vec::new()));
        self.select_new(page, vec![item]);
        Ok(())
    }

    /// Text aus der Systemzwischenablage als Textobjekt einfügen.
    pub fn paste_text(&self, text: &str) {
        let (i, p) = self.paste_target();
        let (page, item) = {
            let st = self.st();
            let t = TextItem::new(p.x, p.y, text.trim_end().to_string(), st.tools.text_font.clone(), st.tools.text_size, st.tools.pen.color, Vec::new());
            (st.doc.pages[i].clone(), Item::Text(t))
        };
        self.select_new(page, vec![item]);
    }

    /// Einfügeposition für Bilder aus der Zwischenablage.
    pub fn paste_target(&self) -> (usize, Pt) {
        let st = self.st();
        st.hover.and_then(|(x, y)| st.page_at(x, y, 0.0)).unwrap_or_else(|| {
            // sonst: Mitte des sichtbaren Teils der aktuellen Seite
            let i = st.current_page();
            let r = st.page_layout_rect(i);
            let v = st.view_layout().intersect_or_empty(&r);
            let c = if v.width() > 0.0 { v.center() } else { r.center() };
            (i, Pt::new((c.x - r.x0 - 60.0).max(10.0), (c.y - r.y0 - 40.0).max(10.0)))
        })
    }

    // ---------------------------------------------------------------- Platz

    fn insert_space(&self, page: &PageRef, y0: f64, dy: f64) {
        if dy.abs() < 1.0 {
            return;
        }
        let mut st = self.st();
        let mut changes = Vec::new();
        let mut bottom: f64 = 0.0;
        let nlayers = page.borrow().layers.len();
        for li in 0..nlayers {
            let old = page.borrow().layers[li].items.clone();
            let xf = Xform::translate(0.0, dy);
            let mut moved = false;
            let new: Vec<ItemRef> = old
                .iter()
                .map(|it| match it.bbox() {
                    Some(b) if b.y0 >= y0 - 0.5 => {
                        moved = true;
                        let n = Arc::new(it.transformed(&xf));
                        bottom = bottom.max(n.bbox().map(|b| b.y1).unwrap_or(0.0));
                        n
                    }
                    _ => it.clone(),
                })
                .collect();
            if moved {
                let new = Arc::new(new);
                page.borrow_mut().set_items(li, new.clone());
                changes.push(Change::Items { page: page.clone(), layer: li, old, new });
            }
        }
        // PDF-Hintergrund an derselben Stelle aufschneiden, damit er mitwandert
        let (w, h, bg) = {
            let p = page.borrow();
            (p.width, p.height, p.bg.clone())
        };
        let (mut nh, mut nbg) = (h, bg.clone());
        if bg.is_pdf() && st.doc.pdf_path.is_some() {
            let (splits, real) = bg.split_pdf_at(y0, dy);
            nbg = bg.with_pdf_splits(&splits);
            nh = (h + real).max(10.0);
        }
        // Inhalt rutscht über den Rand: Seite verlängern
        if bottom > nh {
            nh = bottom + 20.0;
        }
        if (nh - h).abs() > 0.01 || nbg.attrs != bg.attrs {
            let mut p = page.borrow_mut();
            p.height = nh;
            p.bg = nbg.clone();
            p.bg_ver = next_version();
            p.mark_ink(None);
            changes.push(Change::Page { page: page.clone(), old: (w, h, bg), new: (w, nh, nbg) });
        }
        st.history.push(changes);
        st.relayout();
    }

    /// Trifft der Punkt den Ziehgriff unter einer Seite?
    fn page_handle_hit(&self, x: f64, y: f64) -> Option<usize> {
        let st = self.st();
        let l = st.to_layout(x, y);
        (0..st.doc.pages.len()).find(|&i| {
            let r = st.page_layout_rect(i);
            let dx = (l.x - (r.x0 + r.x1) / 2.0).abs() * st.zoom;
            let dy = (l.y - r.y1) * st.zoom;
            dx < 40.0 && (-2.0..=16.0).contains(&dy)
        })
    }

    // ---------------------------------------------------------------- Radialmenü

    pub fn open_radial(&self, center: (f64, f64), held: bool) {
        self.commit_text();
        {
            let mut st = self.st();
            st.radial = Some(RadialMenu { center, hover: None });
            if held {
                st.action = Some(Action::Radial { center, moved: false });
            }
        }
        self.changed();
    }

    fn apply_radial(&self, item: RadialItem, center: (f64, f64)) {
        match item {
            RadialItem::Close => {}
            // Menü bleibt offen, damit mehrere Schritte nacheinander gehen
            RadialItem::Undo | RadialItem::Redo => {
                if item == RadialItem::Undo { self.undo() } else { self.redo() }
                self.st().radial = Some(RadialMenu { center, hover: None });
            }
            RadialItem::Claude => self.request(Request::Claude),
            RadialItem::Paste => {
                // an der Stelle einfügen, wo das Menü geöffnet wurde
                self.st().hover = Some(center);
                self.request(Request::Paste);
            }
            RadialItem::Color(i) => {
                let c = PALETTE[RADIAL_COLORS[i]];
                if !self.recolor_selection(c) {
                    let mut st = self.st();
                    if !st.tools.tool.uses_pen() {
                        st.tools.tool = Tool::Pen;
                    }
                    st.tools.set_color(c);
                }
            }
            RadialItem::Tool(t) => self.set_tool(t),
        }
        self.changed();
    }

    // ---------------------------------------------------------------- Overlays

    pub(super) fn draw_overlays(&self, st: &mut State, snap: &gtk::Snapshot, dark: bool) {
        let accent = self
            .color_lookup("accent_color")
            .unwrap_or(gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
        let z = st.zoom;
        let cam = st.cam;
        let scr = |o: Pt, p: Pt| (((o.x + p.x - cam.x) * z) as f32, ((o.y + p.y - cam.y) * z) as f32);

        // Ziehgriffe unter den Seiten
        let vis = st.view_layout();
        let handle_hover = st.hover.and_then(|(x, y)| {
            let l = st.to_layout(x, y);
            (0..st.doc.pages.len()).find(|&i| {
                let r = st.page_layout_rect(i);
                ((l.x - (r.x0 + r.x1) / 2.0).abs() * z) < 40.0 && ((l.y - r.y1) * z) > -2.0 && ((l.y - r.y1) * z) < 16.0
            })
        });
        for i in 0..st.doc.pages.len() {
            let r = st.page_layout_rect(i);
            if !r.intersects(&vis) {
                continue;
            }
            let cx = ((r.x0 + r.x1) / 2.0 - cam.x) * z;
            let y = (r.y1 - cam.y) * z + 6.0;
            let hov = handle_hover == Some(i) || matches!(&st.action, Some(Action::PageHeight { page, .. }) if Rc::ptr_eq(page, &st.doc.pages[i]));
            let rect = graphene::Rect::new(cx as f32 - 22.0, y as f32, 44.0, 5.0);
            snap.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 2.5));
            let c = if hov { accent } else if dark { gdk::RGBA::new(1.0, 1.0, 1.0, 0.28) } else { gdk::RGBA::new(0.0, 0.0, 0.0, 0.22) };
            snap.append_color(&c, &rect);
            snap.pop();
        }

        // Auswahl
        if let Some(sel) = &st.sel {
            if let Some(i) = st.doc.index_of(&sel.page) {
                let o = st.page_origin(i);
                if let Some((tex, r0, x0, _)) = &sel.tex {
                    // Textur relativ zur Transformation beim Rendern verschieben/drehen/skalieren
                    let rel = x0.inverse().then(&sel.xf);
                    snap.save();
                    snap.translate(&graphene::Point::new(((rel.tx + o.x - cam.x) * z) as f32, ((rel.ty + o.y - cam.y) * z) as f32));
                    snap.scale(z as f32, z as f32);
                    snap.rotate(rel.angle().to_degrees() as f32);
                    snap.scale(rel.scale() as f32, rel.scale() as f32);
                    // im Dunkelmodus schon umgefärbt gerendert (wie die Kacheln)
                    let b = graphene::Rect::new(r0.x0 as f32, r0.y0 as f32, (r0.x1 - r0.x0) as f32, (r0.y1 - r0.y0) as f32);
                    snap.append_texture(tex, &b);
                    snap.restore();
                }
                let quad = sel.corners(SEL_PAD / z).map(|p| scr(o, p));
                let frame = gsk::PathBuilder::new();
                frame.move_to(quad[0].0, quad[0].1);
                for q in &quad[1..] {
                    frame.line_to(q.0, q.1);
                }
                frame.close();
                let frame = frame.to_path();
                let mut fill = accent;
                fill.set_alpha(0.06);
                snap.append_fill(&frame, gsk::FillRule::Winding, &fill);
                let stroke = gsk::Stroke::new(1.5);
                stroke.set_dash(&[6.0, 4.0]);
                snap.append_stroke(&frame, &stroke, &accent);
                // Drehgriff mit Verbindungslinie
                let (m, h) = sel.rotate_handle(SEL_PAD / z, ROT_OFF / z);
                let (m, h) = (scr(o, m), scr(o, h));
                let line = gsk::PathBuilder::new();
                line.move_to(m.0, m.1);
                line.line_to(h.0, h.1);
                snap.append_stroke(&line.to_path(), &gsk::Stroke::new(1.5), &accent);
                let p = circle_path((h.0 as f64, h.1 as f64), 7.0);
                snap.append_fill(&p, gsk::FillRule::Winding, &accent);
                snap.append_stroke(&p, &gsk::Stroke::new(2.0), &gdk::RGBA::WHITE);
                for (hx, hy) in quad {
                    let p = circle_path((hx as f64, hy as f64), 6.0);
                    snap.append_fill(&p, gsk::FillRule::Winding, &gdk::RGBA::WHITE);
                    snap.append_stroke(&p, &gsk::Stroke::new(2.0), &accent);
                }
            }
        }

        match &st.action {
            Some(Action::Lasso { origin, pts, rect, .. }) if pts.len() > 1 => {
                let b = gsk::PathBuilder::new();
                if *rect {
                    let r = Rect::from_pts(pts[0], *pts.last().unwrap());
                    let (x0, y0) = scr(*origin, Pt::new(r.x0, r.y0));
                    let (x1, y1) = scr(*origin, Pt::new(r.x1, r.y1));
                    b.add_rect(&graphene::Rect::new(x0, y0, x1 - x0, y1 - y0));
                } else {
                    let (x, y) = scr(*origin, pts[0]);
                    b.move_to(x, y);
                    for p in &pts[1..] {
                        let (x, y) = scr(*origin, *p);
                        b.line_to(x, y);
                    }
                    b.close();
                }
                let path = b.to_path();
                let mut fill = accent;
                fill.set_alpha(0.08);
                snap.append_fill(&path, gsk::FillRule::Winding, &fill);
                let stroke = gsk::Stroke::new(1.5);
                stroke.set_dash(&[5.0, 4.0]);
                snap.append_stroke(&path, &stroke, &accent);
            }
            Some(Action::Shape { origin, kind, a, b, .. }) => {
                let p = &st.tools.pen;
                let c = p.color.to_gdk();
                if st.dark_paper {
                    let (m, off) = super::dark_matrix();
                    snap.push_color_matrix(&m, &off);
                }
                let stroke = gsk::Stroke::new((p.width * z).max(1.0) as f32);
                stroke.set_line_cap(gsk::LineCap::Round);
                stroke.set_line_join(gsk::LineJoin::Round);
                for pts in shapes::build(*kind, *a, *b, p.width) {
                    let pb = gsk::PathBuilder::new();
                    let (x, y) = scr(*origin, pts[0]);
                    pb.move_to(x, y);
                    for q in &pts[1..] {
                        let (x, y) = scr(*origin, *q);
                        pb.line_to(x, y);
                    }
                    snap.append_stroke(&pb.to_path(), &stroke, &c);
                }
                if st.dark_paper {
                    snap.pop();
                }
            }
            Some(Action::Space { page, origin, y0, y, .. }) => {
                let w = page.borrow().width;
                let (x0, ya) = scr(*origin, Pt::new(0.0, *y0));
                let (x1, yb) = scr(*origin, Pt::new(w, *y));
                // nach oben = Platz entfernen: rot
                let accent = if y < y0 { gdk::RGBA::new(0.88, 0.2, 0.2, 1.0) } else { accent };
                let mut band = accent;
                band.set_alpha(0.12);
                snap.append_color(&band, &graphene::Rect::new(x0, ya.min(yb), x1 - x0, (yb - ya).abs()));
                for yy in [ya, yb] {
                    let pb = gsk::PathBuilder::new();
                    pb.move_to(x0, yy);
                    pb.line_to(x1, yy);
                    let stroke = gsk::Stroke::new(1.5);
                    stroke.set_dash(&[6.0, 4.0]);
                    snap.append_stroke(&pb.to_path(), &stroke, &accent);
                }
            }
            _ => {}
        }

        self.draw_search_and_mark(st, snap);
        self.draw_guide(st, snap);

        if let Some(r) = &st.radial {
            self.draw_radial(snap, r, &st.tools, dark, accent);
        }
    }

    fn color_lookup(&self, name: &str) -> Option<gdk::RGBA> {
        #[allow(deprecated)]
        self.style_context().lookup_color(name)
    }

    fn draw_radial(&self, snap: &gtk::Snapshot, r: &RadialMenu, tools: &ToolState, dark: bool, accent: gdk::RGBA) {
        let c = r.center;
        let (bg, bg2, fg) = if dark {
            (gdk::RGBA::new(0.17, 0.17, 0.19, 0.97), gdk::RGBA::new(0.22, 0.22, 0.25, 1.0), gdk::RGBA::new(0.95, 0.95, 0.95, 1.0))
        } else {
            (gdk::RGBA::new(0.99, 0.99, 0.99, 0.97), gdk::RGBA::new(0.93, 0.93, 0.94, 1.0), gdk::RGBA::new(0.15, 0.15, 0.17, 1.0))
        };
        // Schatten + Grundscheibe
        let outer = graphene::Rect::new((c.0 - R_TOOLS) as f32, (c.1 - R_TOOLS) as f32, (2.0 * R_TOOLS) as f32, (2.0 * R_TOOLS) as f32);
        snap.append_outset_shadow(&gsk::RoundedRect::from_rect(outer, R_TOOLS as f32), &gdk::RGBA::new(0.0, 0.0, 0.0, 0.3), 0.0, 3.0, 0.0, 14.0);
        snap.append_fill(&circle_path(c, R_TOOLS), gsk::FillRule::Winding, &bg);
        snap.append_fill(&circle_path(c, R_COLORS), gsk::FillRule::Winding, &bg2);
        // Hover-Segment
        let mut hl = accent;
        hl.set_alpha(0.25);
        match r.hover {
            Some(RadialItem::Color(i)) => {
                let a = seg_angle(i);
                snap.append_fill(&sector(c, R_CENTER, R_COLORS, a - PI / 8.0, a + PI / 8.0), gsk::FillRule::Winding, &hl);
            }
            Some(RadialItem::Close) => snap.append_fill(&circle_path(c, R_CENTER), gsk::FillRule::Winding, &hl),
            Some(item) => {
                if let Some(i) = RADIAL_RING.iter().position(|x| *x == item) {
                    let (a, half) = (ring_angle(i), PI / RADIAL_RING.len() as f64);
                    snap.append_fill(&sector(c, R_COLORS, R_TOOLS, a - half, a + half), gsk::FillRule::Winding, &hl);
                }
            }
            None => {}
        }
        // Werkzeuge
        let theme = gtk::IconTheme::for_display(&self.display());
        let sf = self.scale_factor();
        let icon = |name: &str, x: f64, y: f64, size: f64, color: gdk::RGBA| {
            let p = theme.lookup_icon(name, &[], size as i32, sf, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
            snap.save();
            snap.translate(&graphene::Point::new((x - size / 2.0) as f32, (y - size / 2.0) as f32));
            p.snapshot_symbolic(snap.upcast_ref::<gdk::Snapshot>(), size, size, &[color]);
            snap.restore();
        };
        let rm = (R_COLORS + R_TOOLS) / 2.0;
        for (i, item) in RADIAL_RING.iter().enumerate() {
            let a = ring_angle(i);
            let (name, col) = match item {
                RadialItem::Tool(t) => (t.icon(), if *t == tools.tool { accent } else { fg }),
                RadialItem::Undo => ("edit-undo-symbolic", fg),
                RadialItem::Redo => ("edit-redo-symbolic", fg),
                RadialItem::Paste => ("edit-paste-symbolic", fg),
                RadialItem::Claude => ("fk-spark-symbolic", gdk::RGBA::new(0.85, 0.47, 0.34, 1.0)),
                _ => continue,
            };
            icon(name, c.0 + rm * a.sin(), c.1 - rm * a.cos(), 24.0, col);
        }
        // Farben
        let rc = (R_CENTER + R_COLORS) / 2.0;
        let cur = if tools.tool == Tool::Highlighter { tools.highlighter.color } else { tools.pen.color };
        for (i, col) in RADIAL_COLORS.iter().map(|&k| &PALETTE[k]).enumerate() {
            let a = seg_angle(i);
            let p = (c.0 + rc * a.sin(), c.1 - rc * a.cos());
            let same = (col.r - cur.r).abs() < 0.01 && (col.g - cur.g).abs() < 0.01 && (col.b - cur.b).abs() < 0.01;
            if same {
                snap.append_stroke(&circle_path(p, 13.0), &gsk::Stroke::new(2.0), &accent);
            }
            snap.append_fill(&circle_path(p, 9.5), gsk::FillRule::Winding, &rgba(*col));
        }
        // Mitte: schließen
        icon("window-close-symbolic", c.0, c.1, 16.0, fg);
    }
}
