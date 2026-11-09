//! Dokumentmodell. Elemente sind unveränderlich und werden per `Arc` geteilt:
//! Render-Threads, Undo-Stack und Speicher-Thread halten einfach Referenzen,
//! ohne dass irgendwer das Dokument sperren muss.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::geom::{dist_point_segment, Color, Pt, Rect, Xform};
use crate::xml::Element;

static VERSION: AtomicU64 = AtomicU64::new(1);
static PAGE_ID: AtomicU64 = AtomicU64::new(1);

/// Global monoton steigende Versionsnummer (auch nach Undo nie wiederverwendet).
pub fn next_version() -> u64 {
    VERSION.fetch_add(1, Ordering::Relaxed)
}

pub type Attrs = Vec<(String, String)>;

pub fn get_attr<'a>(attrs: &'a Attrs, k: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

pub fn set_attr(attrs: &mut Attrs, k: &str, v: &str) {
    match attrs.iter_mut().find(|(n, _)| n == k) {
        Some(e) => e.1 = v.to_string(),
        None => attrs.push((k.to_string(), v.to_string())),
    }
}

// ------------------------------------------------------------------------ Elemente

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeTool {
    Pen,
    Highlighter,
    Eraser,
}

impl StrokeTool {
    pub fn as_str(&self) -> &'static str {
        match self {
            StrokeTool::Pen => "pen",
            StrokeTool::Highlighter => "highlighter",
            StrokeTool::Eraser => "eraser",
        }
    }
    pub fn parse(s: &str) -> StrokeTool {
        match s {
            "highlighter" => StrokeTool::Highlighter,
            "eraser" => StrokeTool::Eraser,
            _ => StrokeTool::Pen,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stroke {
    pub tool: StrokeTool,
    pub color: Color,
    pub width: f64,
    pub pts: Vec<Pt>,
    /// Breite je Punkt (Druck), gleiche Länge wie `pts`.
    pub widths: Option<Vec<f64>>,
    pub attrs: Attrs,
    pub bbox: Rect,
}

impl Stroke {
    pub fn new(tool: StrokeTool, color: Color, width: f64, pts: Vec<Pt>, widths: Option<Vec<f64>>, attrs: Attrs) -> Stroke {
        let maxw = widths.as_ref().map(|w| w.iter().cloned().fold(0.0, f64::max)).unwrap_or(width);
        let bbox = Rect::bounding(&pts).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)).expand(maxw / 2.0 + 0.5);
        Stroke { tool, color, width, pts, widths, attrs, bbox }
    }

    pub fn attr(&self, k: &str) -> Option<&str> {
        get_attr(&self.attrs, k)
    }

    /// Füll-Deckkraft (0..1), falls die Form gefüllt ist.
    pub fn fill(&self) -> Option<f64> {
        let f: i32 = self.attr("fill")?.parse().ok()?;
        (f > 0).then(|| f as f64 / 255.0)
    }

    pub fn width_at(&self, i: usize) -> f64 {
        self.widths.as_ref().map(|w| w[i]).unwrap_or(self.width)
    }

    pub fn hit(&self, p: Pt, r: f64) -> bool {
        if !self.bbox.expand(r).contains(p) {
            return false;
        }
        if self.pts.len() == 1 {
            return p.dist(self.pts[0]) <= r + self.width_at(0) / 2.0;
        }
        self.pts.windows(2).enumerate().any(|(i, s)| dist_point_segment(p, s[0], s[1]) <= r + self.width_at(i) / 2.0)
    }

    /// Radiert einen Kreis aus dem Strich. `None` = unberührt, sonst die Reststücke.
    pub fn erase_circle(&self, c: Pt, r: f64) -> Option<Vec<Stroke>> {
        if !self.hit(c, r) {
            return None;
        }
        // lange Segmente unterteilen, damit auch Segmentmitten radiert werden
        let step = (r * 0.5).max(0.3);
        let mut pts = Vec::with_capacity(self.pts.len());
        let mut ws = Vec::with_capacity(self.pts.len());
        for i in 0..self.pts.len() {
            if i > 0 {
                let (a, b) = (self.pts[i - 1], self.pts[i]);
                let n = (a.dist(b) / step) as usize;
                for k in 1..n {
                    let t = k as f64 / n as f64;
                    pts.push(a.lerp(b, t));
                    ws.push(self.width_at(i - 1) + (self.width_at(i) - self.width_at(i - 1)) * t);
                }
            }
            pts.push(self.pts[i]);
            ws.push(self.width_at(i));
        }
        let mut parts = Vec::new();
        let (mut cp, mut cw) = (Vec::new(), Vec::new());
        for (p, w) in pts.into_iter().zip(ws) {
            if p.dist(c) <= r + w * 0.25 {
                if cp.len() >= 2 {
                    parts.push((std::mem::take(&mut cp), std::mem::take(&mut cw)));
                }
                cp.clear();
                cw.clear();
            } else {
                cp.push(p);
                cw.push(w);
            }
        }
        if cp.len() >= 2 {
            parts.push((cp, cw));
        }
        let pressure = self.widths.is_some();
        Some(
            parts
                .into_iter()
                .map(|(p, w)| Stroke::new(self.tool, self.color, self.width, p, pressure.then_some(w), self.attrs.clone()))
                .collect(),
        )
    }
}

#[derive(Debug)]
pub struct TextItem {
    pub x: f64,
    pub y: f64,
    pub text: String,
    pub font: String,
    pub size: f64,
    pub color: Color,
    pub attrs: Attrs,
    extent: OnceLock<(f64, f64)>,
}

impl Clone for TextItem {
    fn clone(&self) -> Self {
        TextItem::new(self.x, self.y, self.text.clone(), self.font.clone(), self.size, self.color, self.attrs.clone())
    }
}

impl TextItem {
    pub fn new(x: f64, y: f64, text: String, font: String, size: f64, color: Color, attrs: Attrs) -> Self {
        TextItem { x, y, text, font, size, color, attrs, extent: OnceLock::new() }
    }
    pub fn extent(&self) -> (f64, f64) {
        *self.extent.get_or_init(|| crate::render::measure_text(self))
    }
    pub fn bbox(&self) -> Rect {
        let (w, h) = self.extent();
        Rect::new(self.x, self.y, self.x + w, self.y + h)
    }
}

/// Dekodierte Bild-Surface. Wird nach dem Dekodieren nie mehr verändert und ist
/// daher gefahrlos zwischen Threads teilbar (cairo zählt Referenzen atomar).
pub struct DecodedImage {
    pub surf: cairo::ImageSurface,
    /// Fotos/bunte Bilder: vorab gegenkorrigiert, damit der Dunkelmodus-Filter sie
    /// unverändert zeigt (None bei Screenshots mit hellem/dunklem Hintergrund)
    pub dark: Option<cairo::ImageSurface>,
}
unsafe impl Send for DecodedImage {}
unsafe impl Sync for DecodedImage {}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DecodedImage")
    }
}

pub type ImageCache = Arc<OnceLock<Option<DecodedImage>>>;

#[derive(Clone, Debug)]
pub struct ImageItem {
    pub rect: Rect,
    /// Base64-kodiertes PNG wie in der .xopp-Datei.
    pub data: Arc<String>,
    pub attrs: Attrs,
    pub decoded: ImageCache,
}

/// Drehwinkel eines Bildes in Grad im Uhrzeigersinn um seine Mitte. Eigenes Attribut
/// von Graphit: `left/top/right/bottom` bleiben das ungedrehte Rechteck, Xournal++
/// zeigt das Bild daher ungedreht an derselben Stelle.
pub const IMAGE_ROTATION: &str = "graphit-rotation";

/// Attribut für fixierte Elemente (eigenes Attribut von Graphit, Xournal++ ignoriert es).
pub const LOCKED: &str = "graphit-locked";

impl ImageItem {
    pub fn new(rect: Rect, data: Arc<String>, attrs: Attrs) -> Self {
        ImageItem { rect, data, attrs, decoded: Arc::new(OnceLock::new()) }
    }
    pub fn angle(&self) -> f64 {
        get_attr(&self.attrs, IMAGE_ROTATION).and_then(|v| v.parse::<f64>().ok()).filter(|a| a.is_finite()).unwrap_or(0.0)
    }
    /// Umschließendes Rechteck des (evtl. gedrehten) Bildes.
    pub fn bbox(&self) -> Rect {
        match self.angle() {
            0.0 => self.rect,
            a => Xform::rotate_about(a.to_radians(), self.rect.center()).apply_rect(&self.rect),
        }
    }
}

/// Unbekanntes Element (z. B. LaTeX-`teximage`), wird unverändert erhalten.
#[derive(Clone, Debug)]
pub struct RawItem {
    pub elem: Element,
    pub bbox: Option<Rect>,
    pub decoded: ImageCache,
}

#[derive(Clone, Debug)]
pub enum Item {
    Stroke(Stroke),
    Text(TextItem),
    Image(ImageItem),
    Raw(RawItem),
}

pub type ItemRef = Arc<Item>;
pub type ItemList = Arc<Vec<ItemRef>>;

impl Item {
    pub fn bbox(&self) -> Option<Rect> {
        match self {
            Item::Stroke(s) => Some(s.bbox),
            Item::Text(t) => Some(t.bbox()),
            Item::Image(i) => Some(i.bbox()),
            Item::Raw(r) => r.bbox,
        }
    }

    pub fn transformed(&self, x: &Xform) -> Item {
        let k = x.scale();
        match self {
            Item::Stroke(s) => Item::Stroke(Stroke::new(
                s.tool,
                s.color,
                s.width * k,
                s.pts.iter().map(|p| x.apply(*p)).collect(),
                s.widths.as_ref().map(|w| w.iter().map(|v| v * k).collect()),
                s.attrs.clone(),
            )),
            Item::Text(t) => {
                // Text bleibt aufrecht, nur seine Mitte wandert mit
                let r = x.apply_rect_upright(&t.bbox());
                Item::Text(TextItem::new(r.x0, r.y0, t.text.clone(), t.font.clone(), t.size * k, t.color, t.attrs.clone()))
            }
            Item::Image(i) => {
                // Mitte wandert mit, der Winkel wird nur als Attribut vermerkt
                let mut a = (i.angle() + x.angle().to_degrees()).rem_euclid(360.0);
                if a.min(360.0 - a) < 1e-4 {
                    a = 0.0;
                }
                let mut attrs = i.attrs.clone();
                attrs.retain(|(k, _)| k != IMAGE_ROTATION);
                if a != 0.0 {
                    attrs.push((IMAGE_ROTATION.into(), fmt_num(a)));
                }
                Item::Image(ImageItem { rect: x.apply_rect_upright(&i.rect), attrs, ..i.clone() })
            }
            Item::Raw(r) => {
                let Some(b) = r.bbox else { return self.clone() };
                let nb = x.apply_rect_upright(&b);
                let mut elem = r.elem.clone();
                for (k, v) in [("left", nb.x0), ("top", nb.y0), ("right", nb.x1), ("bottom", nb.y1)] {
                    set_attr(&mut elem.attrs, k, &fmt_num(v));
                }
                Item::Raw(RawItem { elem, bbox: Some(nb), decoded: r.decoded.clone() })
            }
        }
    }

    fn attrs(&self) -> &Attrs {
        match self {
            Item::Stroke(s) => &s.attrs,
            Item::Text(t) => &t.attrs,
            Item::Image(i) => &i.attrs,
            Item::Raw(r) => &r.elem.attrs,
        }
    }

    /// Fixiert („eingebrannt“): lässt sich nicht mehr auswählen oder wegradieren.
    pub fn locked(&self) -> bool {
        get_attr(self.attrs(), LOCKED) == Some("1")
    }

    pub fn with_locked(&self, on: bool) -> Item {
        let mut it = self.clone();
        let attrs = match &mut it {
            Item::Stroke(s) => &mut s.attrs,
            Item::Text(t) => &mut t.attrs,
            Item::Image(i) => &mut i.attrs,
            Item::Raw(r) => &mut r.elem.attrs,
        };
        attrs.retain(|(k, _)| k != LOCKED);
        if on {
            attrs.push((LOCKED.into(), "1".into()));
        }
        it
    }

    pub fn recolored(&self, c: Color) -> Item {
        match self {
            Item::Stroke(s) => {
                let c = if s.tool == StrokeTool::Highlighter { c.with_alpha(s.color.a) } else { c };
                Item::Stroke(Stroke { color: c, ..s.clone() })
            }
            Item::Text(t) => Item::Text(TextItem::new(t.x, t.y, t.text.clone(), t.font.clone(), t.size, c, t.attrs.clone())),
            _ => self.clone(),
        }
    }

    /// Trefferprüfung für "Objekt auswählen" / Text antippen.
    pub fn hit(&self, p: Pt, r: f64) -> bool {
        match self {
            Item::Stroke(s) => s.hit(p, r),
            _ => self.bbox().map(|b| b.expand(r).contains(p)).unwrap_or(false),
        }
    }
}

pub fn fmt_num(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

/// Umschließendes Rechteck aller Elemente, die nur in einer der beiden Listen sind.
pub fn diff_bbox(old: &ItemList, new: &ItemList) -> Option<Rect> {
    use std::collections::HashSet;
    let key = |i: &ItemRef| Arc::as_ptr(i) as usize;
    let a: HashSet<usize> = old.iter().map(key).collect();
    let b: HashSet<usize> = new.iter().map(key).collect();
    let mut r: Option<Rect> = None;
    for it in old.iter().filter(|i| !b.contains(&key(i))).chain(new.iter().filter(|i| !a.contains(&key(i)))) {
        match it.bbox() {
            Some(bb) => r = Some(Rect::union_opt(r, bb)),
            None => return None,
        }
    }
    // keine Änderung → leeres Rechteck außerhalb jeder Seite
    Some(r.unwrap_or(Rect::new(-1e9, -1e9, -1e9, -1e9)))
}

// ------------------------------------------------------------------------ Seiten

#[derive(Clone, Debug)]
pub struct Background {
    /// Original-Attribute (`type`, `style`, `color`, `pageno`, ...).
    pub attrs: Attrs,
}

impl Background {
    pub fn solid(style: &str) -> Background {
        Background {
            attrs: vec![
                ("type".into(), "solid".into()),
                ("color".into(), "#ffffffff".into()),
                ("style".into(), style.into()),
            ],
        }
    }
    pub fn pdf(pageno: usize) -> Background {
        Background { attrs: vec![("type".into(), "pdf".into()), ("pageno".into(), pageno.to_string())] }
    }
    pub fn kind(&self) -> &str {
        get_attr(&self.attrs, "type").unwrap_or("solid")
    }
    pub fn is_pdf(&self) -> bool {
        self.kind() == "pdf"
    }
    pub fn style(&self) -> &str {
        get_attr(&self.attrs, "style").unwrap_or("plain")
    }
    pub fn color(&self) -> Color {
        get_attr(&self.attrs, "color").and_then(Color::from_hex).unwrap_or(Color::WHITE)
    }
    /// Eingefügte Lücken im PDF: (y im Original-PDF, Höhe der Lücke), sortiert.
    /// Gespeichert als Graphit-Attribut `graphit-splits="y:h;y:h"`; Xournal++
    /// ignoriert es und zeigt das PDF dann ungeteilt.
    pub fn pdf_splits(&self) -> Vec<(f64, f64)> {
        // "federkiel-splits": Dateien aus der Zeit vor der Umbenennung
        let mut v: Vec<(f64, f64)> = get_attr(&self.attrs, "graphit-splits")
            .or_else(|| get_attr(&self.attrs, "federkiel-splits"))
            .unwrap_or("")
            .split(';')
            .filter_map(|p| {
                let (a, b) = p.split_once(':')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            })
            .filter(|(_, h): &(f64, f64)| *h > 0.01)
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    pub fn with_pdf_splits(&self, splits: &[(f64, f64)]) -> Background {
        let mut b = self.clone();
        b.attrs.retain(|(k, _)| k != "graphit-splits" && k != "federkiel-splits");
        let v: Vec<String> = splits.iter().filter(|s| s.1 > 0.01).map(|(y, h)| format!("{}:{}", fmt_num(*y), fmt_num(*h))).collect();
        if !v.is_empty() {
            b.attrs.push(("graphit-splits".into(), v.join(";")));
        }
        b
    }

    /// PDF-y → angezeigte y (unter jedem Schnitt um dessen Lücke nach unten).
    pub fn pdf_to_display(&self, y: f64) -> f64 {
        y + self.pdf_splits().iter().filter(|s| s.0 <= y).map(|s| s.1).sum::<f64>()
    }

    /// Angezeigte y → PDF-y (in einer Lücke: deren Anfang).
    pub fn display_to_pdf(&self, y: f64) -> f64 {
        let mut cum = 0.0;
        for (sy, g) in self.pdf_splits() {
            if y < sy + cum {
                break;
            }
            if y < sy + cum + g {
                return sy;
            }
            cum += g;
        }
        y - cum
    }

    /// Summe aller eingefügten Lücken.
    pub fn pdf_gap_total(&self) -> f64 {
        self.pdf_splits().iter().map(|s| s.1).sum()
    }

    /// Fügt an der Seitenposition `y` (angezeigte Koordinate) eine Lücke `dy` ein
    /// (negativ = Lücke verkleinern). Liefert die neuen Schnitte und die tatsächliche
    /// Höhenänderung.
    pub fn split_pdf_at(&self, y: f64, dy: f64) -> (Vec<(f64, f64)>, f64) {
        let mut splits = self.pdf_splits();
        let mut cum = 0.0;
        for i in 0..splits.len() {
            let (sy, g) = splits[i];
            if y < sy + cum {
                break;
            }
            if y <= sy + cum + g + 0.5 {
                // in (oder direkt unter) einer bestehenden Lücke: diese anpassen
                let ng = (g + dy).max(0.0);
                let real = ng - g;
                splits[i].1 = ng;
                splits.retain(|s| s.1 > 0.01);
                return (splits, real);
            }
            cum += g;
        }
        if dy <= 0.0 {
            return (splits, 0.0); // PDF selbst kann nicht schrumpfen
        }
        let py = y - cum;
        splits.push((py, dy));
        splits.sort_by(|a, b| a.0.total_cmp(&b.0));
        (splits, dy)
    }

    pub fn pdf_index(&self) -> usize {
        get_attr(&self.attrs, "pageno").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).saturating_sub(1)
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub items: ItemList,
    pub attrs: Attrs,
}

impl Layer {
    pub fn new() -> Layer {
        Layer { items: Arc::new(Vec::new()), attrs: Vec::new() }
    }
    pub fn visible(&self) -> bool {
        get_attr(&self.attrs, "visible") != Some("false")
    }
}

#[derive(Debug)]
pub struct Page {
    pub id: u64,
    pub width: f64,
    pub height: f64,
    pub bg: Background,
    pub layers: Vec<Layer>,
    /// Version des Inhalts (Striche etc.) - steuert die Kachel-Caches.
    pub ink_ver: u64,
    /// Version des Hintergrunds.
    pub bg_ver: u64,
    /// Änderungsprotokoll (Version, Bereich) für gezielte Kachel-Invalidierung.
    pub damage: Vec<(u64, Option<Rect>)>,
    pub damage_floor: u64,
}

pub type PageRef = Rc<RefCell<Page>>;

impl Page {
    pub fn new(width: f64, height: f64, bg: Background) -> Page {
        let ink_ver = next_version();
        Page {
            id: PAGE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            bg,
            layers: vec![Layer::new()],
            ink_ver,
            bg_ver: next_version(),
            damage: Vec::new(),
            damage_floor: ink_ver,
        }
    }

    pub fn into_ref(self) -> PageRef {
        Rc::new(RefCell::new(self))
    }

    /// Kopie mit neuer Identität (für "Seite duplizieren").
    pub fn duplicate(&self) -> Page {
        let mut p = Page::new(self.width, self.height, self.bg.clone());
        p.layers = self.layers.clone();
        p
    }

    pub fn current_layer(&self) -> usize {
        self.layers.len() - 1
    }

    pub fn set_items(&mut self, layer: usize, items: ItemList) {
        let dmg = diff_bbox(&self.layers[layer].items, &items);
        self.layers[layer].items = items;
        self.mark_ink(dmg);
    }

    /// Inhalt hat sich geändert. `dmg` = betroffener Bereich (None = alles).
    pub fn mark_ink(&mut self, dmg: Option<Rect>) {
        self.ink_ver = next_version();
        self.damage.push((self.ink_ver, dmg));
        if self.damage.len() > 512 {
            let cut = self.damage.len() - 256;
            self.damage_floor = self.damage[cut - 1].0;
            self.damage.drain(..cut);
        }
    }

    /// Ist eine Kachel mit Version `ver` über dem Bereich `r` noch aktuell?
    /// So werden nach einer Änderung nur die betroffenen Kacheln neu gerastert.
    pub fn tile_valid(&self, ver: u64, r: &Rect) -> bool {
        if ver >= self.ink_ver {
            return true;
        }
        if ver < self.damage_floor {
            return false;
        }
        self.damage.iter().filter(|(v, _)| *v > ver).all(|(_, d)| matches!(d, Some(d) if !d.intersects(r)))
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width, self.height)
    }

    pub fn snapshot(&self, pdf: Option<PathBuf>) -> Arc<PageData> {
        Arc::new(PageData {
            width: self.width,
            height: self.height,
            bg: self.bg.clone(),
            layers: self.layers.iter().filter(|l| l.visible()).map(|l| l.items.clone()).collect(),
            pdf,
        })
    }
}

/// Unveränderliche Momentaufnahme einer Seite für Render-Threads.
#[derive(Debug)]
pub struct PageData {
    pub width: f64,
    pub height: f64,
    pub bg: Background,
    pub layers: Vec<ItemList>,
    pub pdf: Option<PathBuf>,
}

// ------------------------------------------------------------------------ Dokument

pub struct Document {
    pub pages: Vec<PageRef>,
    pub path: Option<PathBuf>,
    pub pdf_path: Option<PathBuf>,
    pub root_attrs: Attrs,
    pub title: String,
}

impl Document {
    pub fn new_blank(style: &str) -> Document {
        Document {
            pages: vec![Page::new(595.276, 841.89, Background::solid(style)).into_ref()],
            path: None,
            pdf_path: None,
            root_attrs: Vec::new(),
            title: "Xournal++ document - see https://xournalpp.github.io/".into(),
        }
    }

    pub fn from_pdf(pdf: PathBuf, sizes: &[(f64, f64)]) -> Document {
        let mut d = Document::new_blank("plain");
        d.pages = sizes.iter().enumerate().map(|(i, (w, h))| Page::new(*w, *h, Background::pdf(i + 1)).into_ref()).collect();
        d.pdf_path = Some(pdf);
        d
    }

    pub fn page_pdf(&self, p: &Page) -> Option<PathBuf> {
        if p.bg.is_pdf() { self.pdf_path.clone() } else { None }
    }

    pub fn index_of(&self, page: &PageRef) -> Option<usize> {
        self.pages.iter().position(|p| Rc::ptr_eq(p, page))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_splits_mapping() {
        let bg = Background::pdf(1);
        // 50 pt Lücke bei y=300
        let (s, real) = bg.split_pdf_at(300.0, 50.0);
        assert_eq!(s, vec![(300.0, 50.0)]);
        assert_eq!(real, 50.0);
        let bg = bg.with_pdf_splits(&s);
        // weiter unten (angezeigt 500 → PDF 450) eine zweite Lücke
        let (s, _) = bg.split_pdf_at(500.0, 20.0);
        assert_eq!(s, vec![(300.0, 50.0), (450.0, 20.0)]);
        let bg = bg.with_pdf_splits(&s);
        // in der ersten Lücke weiter aufziehen → dieselbe Lücke wächst
        let (s, _) = bg.split_pdf_at(320.0, 10.0);
        assert_eq!(s, vec![(300.0, 60.0), (450.0, 20.0)]);
        // Lücke wieder schließen
        let bg = bg.with_pdf_splits(&s);
        let (s, real) = bg.split_pdf_at(330.0, -100.0);
        assert_eq!(s, vec![(450.0, 20.0)]);
        assert_eq!(real, -60.0);
        // Attribut-Roundtrip
        assert_eq!(bg.with_pdf_splits(&s).pdf_splits(), s);
    }

    #[test]
    fn image_rotation_is_an_attribute() {
        let img = Item::Image(ImageItem::new(Rect::new(0.0, 0.0, 40.0, 20.0), Arc::new("x".into()), Vec::new()));
        let c = Pt::new(100.0, 100.0);
        let r = img.transformed(&Xform::rotate_about(std::f64::consts::FRAC_PI_2, c));
        let Item::Image(i) = &r else { panic!() };
        assert_eq!(i.angle(), 90.0);
        // Rechteck bleibt ungedreht (Größe gleich), nur die Mitte wandert
        assert_eq!((i.rect.width(), i.rect.height()), (40.0, 20.0));
        assert!(i.rect.center().dist(Pt::new(190.0, 20.0)) < 1e-9, "{:?}", i.rect.center());
        let b = r.bbox().unwrap();
        assert!((b.width() - 20.0).abs() < 1e-9 && (b.height() - 40.0).abs() < 1e-9);
        // zurückdrehen entfernt das Attribut
        let back = r.transformed(&Xform::rotate_about(-std::f64::consts::FRAC_PI_2, c));
        let Item::Image(i) = &back else { panic!() };
        assert!(i.attrs.is_empty() && i.angle() == 0.0);
        assert!(i.rect.center().dist(Pt::new(20.0, 10.0)) < 1e-9);
    }
}
