//! Claude schreibt in die Notiz: Es legt eine Liste von Änderungen als JSON ab, Graphit
//! löst die Orte auf und setzt alles in einem Undo-Schritt auf die Seiten. Das Ergebnis
//! geht als Datei und Bild zurück, damit Claude es prüfen und korrigieren kann.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use gtk::{gdk_pixbuf, gio};
use serde_json::{json, Value};

use crate::docview::DocView;
use crate::geom::{Color, Pt, Rect};
use crate::latex;
use crate::model::{ImageItem, Item, PageData, PageRef, Stroke, StrokeTool, TextItem};
use crate::render;
use crate::tools::PALETTE;

pub const REQUEST: &str = "einfuegen.json";
pub const RESULT: &str = "ergebnis.json";
const RESULT_IMAGE: &str = "ergebnis.png";
const GUIDE: &str = "einfuegen-anleitung.md";

const TEXT_SIZE: f64 = 11.0;
const FONT: &str = "Sans";
/// Abstand neben einer Fundstelle
const GAP: f64 = 6.0;

const GUIDE_TEXT: &str = r#"# In die Notiz schreiben

Schreibe die Datei CACHE/einfuegen.json. Graphit übernimmt sie sofort und löscht sie. Der Nutzer kann alles mit einem Rückgängig entfernen.

{"aenderungen":[ ... ], "geometrie":[5], "rueckgaengig":false}

Jede Änderung hat einen Inhalt, einen Ort und optional "seite" (ab 1, Vorgabe aktuelle Seite; "neu" = neue leere Seite direkt danach) und "farbe" (schwarz, blau, rot, gruen, orange, hellblau, hellgruen, gelb, lila, grau).

Inhalt, genau einer:
- "text": Text, Markdown oder Formeln ($…$). Reine Zahlen und Wörter werden bearbeitbare Textobjekte, Formeln und Tabellen gesetzte Bilder. "groesse" in Punkt (Vorgabe 11).
- "bild": Pfad zu einer PNG-Datei, etwa einem Diagramm.
- "linien": [[[x,y],[x,y],…],…] als Linienzüge, "pfeil":true für eine Spitze am Ende, "breite" in Punkt. Für Skizzen, Hilfslinien, Freikörperbilder.
- "markieren": "wörtlicher Text", legt den Marker darüber.

Ort, für text und bild, höchstens einer:
- "bereich":[x0,y0,x1,y1]: passt den Inhalt hinein, verkleinert ihn nötigenfalls. "ausrichtung": links, mitte (Vorgabe), rechts.
- "punkt":[x,y]: obere linke Ecke.
- "neben":"wörtlicher Text", "richtung": rechts (Vorgabe), links, oben, unten.
- "frei":true: sucht eine freie Stelle ohne Überdeckung, mit "nahe":"wörtlicher Text" in deren Umgebung. Vorgabe, wenn kein Ort genannt ist.

Alle Koordinaten sind Anteile der Seite von 0 bis 1, (0,0) ist oben links.

Lage auf der Seite: CACHE/geometrie-<Seite>.json hat "woerter" [[text,x0,y0,x1,y1],…] und "linien" [[x0,y0,x1,y1],…] (gedruckte waagrechte und senkrechte Striche, also auch Tabellengitter). Die aktuelle Seite liegt immer bereit, andere holst du mit "geometrie":[Seite]. Die Datei kann groß sein: Filtere sie per Bash, statt sie ganz zu lesen. Zellen ergeben sich aus den Gitterlinien. Handschrift steht in CACHE/striche.json.

Trage jede Zelle, Lücke oder Beschriftung als eigene Änderung an ihrer Stelle ein, statt einen eigenen Block daneben zu setzen.

Ergebnis: Warte mit
timeout 30 sh -c 'until [ -f CACHE/ergebnis.json ]; do sleep 0.3; done'; cat CACHE/ergebnis.json
Dort stehen "eingefuegt", "fehler" und "orte" je Änderung. CACHE/ergebnis.png zeigt die erste geänderte Seite: Sieh sie an, wenn viele Einträge eng sitzen. Ist etwas falsch, entferne alles mit {"rueckgaengig":true} und schreibe es neu. Das geht nur, solange der Nutzer nichts geändert hat.
"#;

pub fn write_guide(cache: &Path) {
    let _ = std::fs::write(cache.join(GUIDE), GUIDE_TEXT.replace("CACHE", &cache.display().to_string()));
}

// ------------------------------------------------------------------------ Anweisungen

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Current,
    New,
    Number(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Right,
    Left,
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Debug, PartialEq)]
enum Spot {
    Area([f64; 4], Align),
    Point([f64; 2]),
    Beside(String, Side),
    Free(Option<String>),
}

#[derive(Clone, Debug, PartialEq)]
enum Content {
    Text { text: String, size: f64 },
    Image(PathBuf),
    Lines { lines: Vec<Vec<[f64; 2]>>, arrow: bool, width: f64 },
    Highlight(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Change {
    target: Target,
    color: Color,
    spot: Spot,
    content: Content,
}

#[derive(Debug, Default, PartialEq)]
struct Request {
    undo: bool,
    geometry: Vec<usize>,
    changes: Vec<Result<Change, String>>,
}

fn color_named(name: &str) -> Option<Color> {
    let i = match name.to_lowercase().as_str() {
        "schwarz" => 0,
        "blau" => 1,
        "rot" => 2,
        "gruen" | "grün" => 3,
        "orange" => 4,
        "hellblau" => 5,
        "hellgruen" | "hellgrün" => 6,
        "gelb" => 8,
        "lila" => 9,
        "grau" => 14,
        _ => return None,
    };
    Some(PALETTE[i])
}

fn four(v: &Value) -> Option<[f64; 4]> {
    let a = v.as_array().filter(|a| a.len() == 4)?;
    let (x0, y0, x1, y1) = (a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?, a[3].as_f64()?);
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

fn two(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array().filter(|a| a.len() == 2)?;
    Some([a[0].as_f64()?, a[1].as_f64()?])
}

fn parse_change(v: &Value) -> Result<Change, String> {
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let target = match &v["seite"] {
        Value::String(s) if s == "neu" => Target::New,
        Value::Number(n) => Target::Number(n.as_u64().filter(|n| *n >= 1).ok_or("seite beginnt bei 1")? as usize - 1),
        Value::Null => Target::Current,
        other => return Err(format!("unbekannte seite {other}")),
    };
    let color = match text("farbe") {
        Some(c) => color_named(&c).ok_or(format!("unbekannte farbe {c}"))?,
        None => PALETTE[0],
    };
    let align = match text("ausrichtung").as_deref() {
        Some("links") => Align::Start,
        Some("rechts") => Align::End,
        _ => Align::Center,
    };
    let spot = if let Some(a) = four(&v["bereich"]) {
        Spot::Area(a, align)
    } else if let Some(p) = two(&v["punkt"]) {
        Spot::Point(p)
    } else if let Some(t) = text("neben") {
        let side = match text("richtung").as_deref() {
            Some("links") => Side::Left,
            Some("oben") => Side::Above,
            Some("unten") => Side::Below,
            _ => Side::Right,
        };
        Spot::Beside(t, side)
    } else {
        Spot::Free(text("nahe"))
    };
    // Text nicht kürzen: Einrückung und Zeilenenden gehören zum Inhalt
    let content = if let Some(t) = v["text"].as_str().filter(|t| !t.trim().is_empty()) {
        Content::Text { text: t.trim_end().to_string(), size: v["groesse"].as_f64().filter(|s| *s > 1.0).unwrap_or(TEXT_SIZE) }
    } else if let Some(p) = text("bild") {
        Content::Image(PathBuf::from(p))
    } else if let Some(t) = text("markieren") {
        Content::Highlight(t)
    } else if let Some(l) = v["linien"].as_array() {
        let lines: Vec<Vec<[f64; 2]>> = l.iter().filter_map(|q| Some(q.as_array()?.iter().filter_map(two).collect::<Vec<_>>())).filter(|q| q.len() >= 2).collect();
        if lines.is_empty() {
            return Err("linien ohne gültigen Linienzug".into());
        }
        Content::Lines { lines, arrow: v["pfeil"].as_bool().unwrap_or(false), width: v["breite"].as_f64().filter(|w| *w > 0.0).unwrap_or(1.2) }
    } else {
        return Err("kein Inhalt (text, bild, linien oder markieren)".into());
    };
    Ok(Change { target, color, spot, content })
}

/// `None`: Datei ist noch nicht vollständig geschrieben oder kein gültiges JSON.
fn parse_request(text: &str) -> Option<Request> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.is_object().then(|| Request {
        undo: v["rueckgaengig"].as_bool().unwrap_or(false),
        geometry: v["geometrie"].as_array().map(|a| a.iter().filter_map(|n| n.as_u64()).filter(|n| *n >= 1).map(|n| n as usize - 1).collect()).unwrap_or_default(),
        changes: v["aenderungen"].as_array().map(|a| a.iter().map(parse_change).collect()).unwrap_or_default(),
    })
}

// ------------------------------------------------------------------------ Geometrie

/// Belegte Fläche einer Seite, um freie Stellen zu finden.
struct Grid {
    cols: usize,
    rows: usize,
    used: Vec<bool>,
}

const CELL: f64 = 2.0;
const PAGE_MARGIN: f64 = 20.0;

impl Grid {
    fn empty(w: f64, h: f64) -> Grid {
        let (cols, rows) = ((w / CELL).ceil() as usize + 1, (h / CELL).ceil() as usize + 1);
        Grid { cols, rows, used: vec![false; cols * rows] }
    }

    fn of_page(page: &PageData) -> Grid {
        let mut g = Grid::empty(page.width, page.height);
        // Papiermuster zählt nicht als belegt, ein PDF schon
        if let Some((w, h, stride, data)) = raster(page, 1.0 / CELL, page.bg.is_pdf()) {
            for y in 0..h.min(g.rows) {
                for x in 0..w.min(g.cols) {
                    let p = &data[y * stride + x * 4..y * stride + x * 4 + 3];
                    g.used[y * g.cols + x] = luma(p) < 240.0;
                }
            }
        }
        g
    }

    fn mark(&mut self, r: Rect) {
        let (cols, rows) = (self.cols, self.rows);
        let span = |a: f64, b: f64, n: usize| ((a / CELL).floor().max(0.0) as usize).min(n)..((b / CELL).ceil().max(0.0) as usize).min(n);
        for y in span(r.y0, r.y1, rows) {
            for x in span(r.x0, r.x1, cols) {
                self.used[y * cols + x] = true;
            }
        }
    }

    /// Obere linke Ecke der besten freien Stelle: nah an `near`, sonst möglichst weit oben.
    fn free_spot(&self, w: f64, h: f64, near: Option<Pt>) -> Option<Pt> {
        let (nw, nh) = (((w + 2.0 * GAP) / CELL).ceil() as usize, ((h + 2.0 * GAP) / CELL).ceil() as usize);
        let margin = (PAGE_MARGIN / CELL) as usize;
        if self.cols < nw + 2 * margin || self.rows < nh + 2 * margin {
            return None;
        }
        let mut sum = vec![0u32; (self.cols + 1) * (self.rows + 1)];
        let at = |x: usize, y: usize| y * (self.cols + 1) + x;
        for y in 0..self.rows {
            for x in 0..self.cols {
                sum[at(x + 1, y + 1)] = self.used[y * self.cols + x] as u32 + sum[at(x, y + 1)] + sum[at(x + 1, y)] - sum[at(x, y)];
            }
        }
        let mut best: Option<(f64, Pt)> = None;
        for y in (margin..=self.rows - nh - margin).step_by(2) {
            for x in (margin..=self.cols - nw - margin).step_by(2) {
                if sum[at(x + nw, y + nh)] + sum[at(x, y)] != sum[at(x + nw, y)] + sum[at(x, y + nh)] {
                    continue;
                }
                let p = Pt::new(x as f64 * CELL + GAP, y as f64 * CELL + GAP);
                let cost = match near {
                    Some(n) => (p.x + w / 2.0 - n.x).hypot(p.y + h / 2.0 - n.y),
                    None => p.y * 10_000.0 + p.x,
                };
                if best.is_none_or(|(c, _)| cost < c) {
                    best = Some((cost, p));
                }
            }
        }
        best.map(|b| b.1)
    }
}

fn luma(bgr: &[u8]) -> f64 {
    0.114 * bgr[0] as f64 + 0.587 * bgr[1] as f64 + 0.299 * bgr[2] as f64
}

/// Seite als Pixel auf Weiß: Breite, Höhe, Zeilenlänge, Daten.
fn raster(page: &PageData, scale: f64, with_background: bool) -> Option<(usize, usize, usize, Vec<u8>)> {
    let mut s = surface(page, scale, with_background)?;
    s.flush();
    let (w, h, stride) = (s.width() as usize, s.height() as usize, s.stride() as usize);
    let data = s.data().ok()?.to_vec();
    Some((w, h, stride, data))
}

fn surface(page: &PageData, scale: f64, with_background: bool) -> Option<cairo::ImageSurface> {
    let s = cairo::ImageSurface::create(cairo::Format::Rgb24, (page.width * scale).ceil() as i32, (page.height * scale).ceil() as i32).ok()?;
    {
        let cr = cairo::Context::new(&s).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(scale, scale);
        if with_background {
            render::draw_page(&cr, page, false);
        } else {
            render::draw_items(&cr, &page.layers, None);
        }
    }
    Some(s)
}

/// Lange gerade Striche im Bild, als [x0,y0,x1,y1] in Pixeln. Dicke Linien zählen einmal.
fn straight_lines(w: usize, h: usize, dark: impl Fn(usize, usize) -> bool, min_len: usize) -> Vec<[usize; 4]> {
    let mut found: Vec<[usize; 4]> = Vec::new();
    // waagrecht: pro Zeile die langen Läufe
    let mut scan = |n: usize, m: usize, at: &dyn Fn(usize, usize) -> bool, horizontal: bool| {
        let start = found.len();
        for a in 0..n {
            let mut run = 0;
            for b in 0..=m {
                if b < m && at(a, b) {
                    run += 1;
                    continue;
                }
                if run >= min_len {
                    let (b0, b1) = (b - run, b - 1);
                    // Fortsetzung derselben dicken Linie aus der vorigen Zeile
                    let same = found[start..].iter_mut().find(|l| {
                        let (la, l0, l1) = if horizontal { (l[1], l[0], l[2]) } else { (l[0], l[1], l[3]) };
                        a <= la + 3 && l0.abs_diff(b0) <= 3 && l1.abs_diff(b1) <= 3
                    });
                    match same {
                        Some(l) if horizontal => {
                            l[1] = (l[1] + a) / 2;
                            l[3] = l[1];
                        }
                        Some(l) => {
                            l[0] = (l[0] + a) / 2;
                            l[2] = l[0];
                        }
                        None if horizontal => found.push([b0, a, b1, a]),
                        None => found.push([a, b0, a, b1]),
                    }
                }
                run = 0;
            }
        }
    };
    scan(h, w, &|y, x| dark(x, y), true);
    scan(w, h, &|x, y| dark(x, y), false);
    found
}

/// Ausschnitt einer Seite als PNG, gut lesbar auch für kleine Formeln.
pub fn region_png(page: &PageData, area: Rect) -> Option<Vec<u8>> {
    const SCALE: f64 = 3.0;
    let s = surface(page, SCALE, true)?;
    let clip = |v: f64, max: f64| (v * SCALE).clamp(0.0, max * SCALE);
    let (x0, y0, x1, y1) = (clip(area.x0, page.width), clip(area.y0, page.height), clip(area.x1, page.width), clip(area.y1, page.height));
    let out = cairo::ImageSurface::create(cairo::Format::Rgb24, ((x1 - x0).ceil() as i32).max(1), ((y1 - y0).ceil() as i32).max(1)).ok()?;
    {
        let cr = cairo::Context::new(&out).ok()?;
        cr.set_source_surface(&s, -x0, -y0).ok()?;
        cr.paint().ok()?;
    }
    let mut png = Vec::new();
    out.write_to_png(&mut png).ok()?;
    Some(png)
}

/// Engster Rahmen um den Inhalt in `r` (Pixel). Zerschneidet er ein Zeichen, wächst er
/// bis zur nächsten Lücke, damit ein Term ganz drinliegt.
fn tight_box(w: usize, h: usize, dark: impl Fn(usize, usize) -> bool, r: [usize; 4], grow: usize) -> Option<[usize; 4]> {
    let (rx0, ry0, rx1, ry1) = (r[0].min(w - 1), r[1].min(h - 1), r[2].min(w - 1), r[3].min(h - 1));
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in ry0..=ry1 {
        for x in rx0..=rx1 {
            if dark(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let column = |x: usize, y0: usize, y1: usize| (y0..=y1).any(|y| dark(x, y));
    let row = |y: usize, x0: usize, x1: usize| (x0..=x1).any(|x| dark(x, y));
    for _ in 0..grow {
        let mut moved = false;
        if x0 > 0 && column(x0 - 1, y0, y1) {
            x0 -= 1;
            moved = true;
        }
        if x1 + 1 < w && column(x1 + 1, y0, y1) {
            x1 += 1;
            moved = true;
        }
        if y0 > 0 && row(y0 - 1, x0, x1) {
            y0 -= 1;
            moved = true;
        }
        if y1 + 1 < h && row(y1 + 1, x0, x1) {
            y1 += 1;
            moved = true;
        }
        if !moved {
            break;
        }
    }
    Some([x0, y0, x1, y1])
}

/// Wo auf einer Seite Inhalt steht, in Zellen mit Summentabelle für schnelle Flächenabfragen.
pub struct InkMap {
    cols: usize,
    rows: usize,
    sum: Vec<u32>,
}

/// Zellgröße in Punkten: grob genug, dass ein Wort als volle Fläche zählt.
const INK_CELL: f64 = 3.0;
/// Rand um allen Inhalt in Zellen (6 pt): Karten halten Abstand und schneiden nichts an.
const INK_MARGIN: usize = 2;

impl InkMap {
    fn from_cells(cols: usize, rows: usize, full: impl Fn(usize, usize) -> bool) -> InkMap {
        let mut sum = vec![0u32; (cols + 1) * (rows + 1)];
        for y in 0..rows {
            for x in 0..cols {
                let i = (y + 1) * (cols + 1) + x + 1;
                sum[i] = full(x, y) as u32 + sum[i - 1] + sum[i - cols - 1] - sum[i - cols - 2];
            }
        }
        InkMap { cols, rows, sum }
    }

    /// Breite der Seite in Punkten.
    pub fn width(&self) -> f64 {
        self.cols as f64 * INK_CELL
    }

    /// Fläche mit Inhalt innerhalb von `r`, in Punkten².
    pub fn covered(&self, r: &Rect) -> f64 {
        let cell = |v: f64, max: usize| ((v / INK_CELL).max(0.0) as usize).min(max);
        let (x0, y0) = (cell(r.x0, self.cols), cell(r.y0, self.rows));
        let (x1, y1) = (cell(r.x1.ceil() + INK_CELL - 1.0, self.cols), cell(r.y1.ceil() + INK_CELL - 1.0, self.rows));
        let at = |x: usize, y: usize| self.sum[y * (self.cols + 1) + x] as f64;
        (at(x1, y1) + at(x0, y0) - at(x0, y1) - at(x1, y0)) * INK_CELL * INK_CELL
    }
}

/// Alles Sichtbare einer Seite: PDF, Bilder, Text und Striche, samt einem Rand darum.
pub fn ink_map(page: &PageData) -> Option<InkMap> {
    let (w, h, stride, data) = raster(page, 1.0, true)?;
    let step = INK_CELL as usize;
    let (cols, rows) = (w.div_ceil(step), h.div_ceil(step));
    // auch hellgrauer Text zählt; getönte Kästen (um 242) nicht
    let dark = |x: usize, y: usize| luma(&data[y * stride + x * 4..y * stride + x * 4 + 3]) < 225.0;
    let full: Vec<bool> = (0..rows * cols)
        .map(|i| {
            let (cx, cy) = (i % cols, i / cols);
            (cy * step..((cy + 1) * step).min(h)).any(|y| (cx * step..((cx + 1) * step).min(w)).any(|x| dark(x, y)))
        })
        .collect();
    let grown = grow(&full, cols, rows, INK_MARGIN);
    Some(InkMap::from_cells(cols, rows, |cx, cy| grown[cy * cols + cx]))
}

/// Belegte Zellen um `r` Zellen in jede Richtung erweitern.
fn grow(cells: &[bool], cols: usize, rows: usize, r: usize) -> Vec<bool> {
    let across: Vec<bool> = (0..rows * cols)
        .map(|i| {
            let (x, y) = (i % cols, i / cols);
            (x.saturating_sub(r)..(x + r + 1).min(cols)).any(|xx| cells[y * cols + xx])
        })
        .collect();
    (0..rows * cols)
        .map(|i| {
            let (x, y) = (i % cols, i / cols);
            (y.saturating_sub(r)..(y + r + 1).min(rows)).any(|yy| across[yy * cols + x])
        })
        .collect()
}

/// Grobe Bereiche an den Inhalt der Seite anlegen. Ohne Inhalt darin bleibt der Bereich.
pub fn snap_to_content(page: &PageData, areas: &[Rect]) -> Vec<Rect> {
    const SCALE: f64 = 3.0;
    let Some((w, h, stride, data)) = raster(page, SCALE, true) else { return areas.to_vec() };
    let dark = |x: usize, y: usize| luma(&data[y * stride + x * 4..y * stride + x * 4 + 3]) < 170.0;
    areas
        .iter()
        .map(|a| {
            let px = |v: f64| (v * SCALE).max(0.0) as usize;
            match tight_box(w, h, dark, [px(a.x0), px(a.y0), px(a.x1), px(a.y1)], (15.0 * SCALE) as usize) {
                Some(b) => Rect::new(b[0] as f64 / SCALE, b[1] as f64 / SCALE, (b[2] + 1) as f64 / SCALE, (b[3] + 1) as f64 / SCALE),
                None => *a,
            }
        })
        .collect()
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Geometrie einer Seite ablegen. Läuft auch im Hintergrund-Thread.
pub fn write_geometry(cache: &Path, index: usize, page: &PageData, words: &[(String, Rect)]) {
    const SCALE: f64 = 2.0;
    let (pw, ph) = (page.width.max(1.0), page.height.max(1.0));
    let mut lines = Vec::new();
    if let Some((w, h, stride, data)) = raster(page, SCALE, true) {
        let dark = |x: usize, y: usize| luma(&data[y * stride + x * 4..y * stride + x * 4 + 3]) < 170.0;
        lines = straight_lines(w, h, dark, (25.0 * SCALE) as usize);
        lines.truncate(400);
    }
    let lines: Vec<Value> = lines
        .iter()
        .map(|l| json!([round(l[0] as f64 / SCALE / pw), round(l[1] as f64 / SCALE / ph), round(l[2] as f64 / SCALE / pw), round(l[3] as f64 / SCALE / ph)]))
        .collect();
    let words: Vec<Value> = words.iter().map(|(t, r)| json!([t, round(r.x0 / pw), round(r.y0 / ph), round(r.x1 / pw), round(r.y1 / ph)])).collect();
    let out = json!({"seite": index + 1, "breite_pt": page.width, "hoehe_pt": page.height, "woerter": words, "linien": lines});
    let _ = std::fs::write(cache.join(format!("geometrie-{}.json", index + 1)), out.to_string());
}

/// Zeilen so umbrechen, dass sie in `max_w` passen. Einzelne Wörter bleiben ganz.
fn wrap(text: &str, max_w: f64, width_of: &impl Fn(&str) -> f64) -> String {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split(' ') {
            let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && width_of(&next) > max_w {
                out.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = next;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// Obere linke Ecke, damit (w, h) in `area` nach `align` sitzt.
fn align_in(area: Rect, w: f64, h: f64, align: Align) -> Pt {
    let x = match align {
        Align::Start => area.x0,
        Align::Center => area.x0 + (area.width() - w) / 2.0,
        Align::End => area.x1 - w,
    };
    Pt::new(x, area.y0 + (area.height() - h) / 2.0)
}

fn beside(t: Rect, w: f64, h: f64, side: Side) -> Pt {
    match side {
        Side::Right => Pt::new(t.x1 + GAP, t.center().y - h / 2.0),
        Side::Left => Pt::new(t.x0 - GAP - w, t.center().y - h / 2.0),
        Side::Above => Pt::new(t.x0, t.y0 - GAP - h),
        Side::Below => Pt::new(t.x0, t.y1 + GAP),
    }
}

// ------------------------------------------------------------------------ Anwenden

/// Inhalt mit fester Größe, bevor sein Ort feststeht.
enum Piece {
    Text(String, f64),
    Picture(Vec<u8>, f64, f64),
}

fn picture_item(png: &[u8], rect: Rect) -> Item {
    let data = base64::engine::general_purpose::STANDARD.encode(png);
    Item::Image(ImageItem::new(rect, Arc::new(data), Vec::new()))
}

fn text_item(text: &str, x: f64, y: f64, size: f64, color: Color) -> TextItem {
    TextItem::new(x, y, text.to_string(), FONT.to_string(), size, color, Vec::new())
}

fn hex(c: Color) -> String {
    format!("{:02X}{:02X}{:02X}", (c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8)
}

/// Formeln und Blöcke setzen; läuft im Hintergrund.
fn typeset(jobs: Vec<(usize, String, f64, Color)>) -> HashMap<usize, (Vec<u8>, f64, f64)> {
    let mut done = HashMap::new();
    let mut lines: HashMap<String, Vec<(usize, f64, String, bool)>> = HashMap::new();
    for (i, text, size, color) in jobs {
        match latex::one_line_tex(&text) {
            Some((tex, display)) => lines.entry(hex(color)).or_default().push((i, size, tex, display)),
            None => {
                if let Some(png) = latex::block_png(&text) {
                    if let Some(pb) = gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(png.clone())).ok() {
                        let k = size / TEXT_SIZE / latex::NOTE_RASTER;
                        done.insert(i, (png, pb.width() as f64 * k, pb.height() as f64 * k));
                    }
                }
            }
        }
    }
    for (ink, list) in lines {
        let items: Vec<(String, bool)> = list.iter().map(|l| (l.2.clone(), l.3)).collect();
        for (l, set) in list.iter().zip(latex::set_lines(&items, &ink)) {
            if let Some(s) = set {
                let k = l.1 / 10.0;
                done.insert(l.0, (s.png, s.w * k, s.h * k));
            }
        }
    }
    done
}

struct Sheet {
    page: PageRef,
    index: Option<usize>,
    size: (f64, f64),
    items: Vec<Item>,
    placed: Vec<Rect>,
    grid: Option<Grid>,
}

pub struct Outcome {
    pub placed: usize,
    pub undone: bool,
    pub errors: Vec<String>,
}

/// Liegt eine vollständige Anweisung vor, ausführen. `last`: Serials der eigenen Schritte, der neueste zuletzt.
pub async fn apply(view: &Rc<DocView>, cache: &Path, current: usize, last: &std::cell::RefCell<Vec<u64>>, final_try: bool) -> Option<Outcome> {
    let file = cache.join(REQUEST);
    let text = std::fs::read_to_string(&file).ok()?;
    let Some(request) = parse_request(&text) else {
        if final_try {
            let _ = std::fs::remove_file(&file);
            let errors = vec!["einfuegen.json ist kein gültiges JSON".to_string()];
            let _ = std::fs::write(cache.join(RESULT), json!({"eingefuegt": 0, "fehler": errors}).to_string());
            return Some(Outcome { placed: 0, undone: false, errors });
        }
        return None;
    };
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(cache.join(RESULT));
    let c = &view.canvas;
    c.commit_edits();
    let mut errors: Vec<String> = Vec::new();
    let mut undone = false;
    if request.undo {
        if last.borrow().last() == Some(&c.history_serial()) {
            c.undo();
            last.borrow_mut().pop();
            undone = true;
        } else {
            errors.push("rueckgaengig nicht möglich: Der Nutzer hat seitdem etwas geändert".into());
        }
    }
    for i in &request.geometry {
        match c.page_data(*i) {
            Some(data) => write_geometry(cache, *i, &data, &c.page_words(*i)),
            None => errors.push(format!("geometrie: Seite {} gibt es nicht", i + 1)),
        }
    }
    let jobs: Vec<(usize, String, f64, Color)> = request
        .changes
        .iter()
        .enumerate()
        .filter_map(|(i, ch)| match ch {
            Ok(Change { content: Content::Text { text, size }, color, .. }) if latex::needs_typesetting(text) => Some((i, text.clone(), *size, *color)),
            _ => None,
        })
        .collect();
    let mut typeset_done = if jobs.is_empty() { HashMap::new() } else { gio::spawn_blocking(move || typeset(jobs)).await.unwrap_or_default() };

    let current = current.min(c.page_count().saturating_sub(1));
    let needs_blank = request.changes.iter().flatten().any(|ch| ch.target == Target::New);
    let blank = needs_blank.then(|| c.blank_page_after(current));
    let mut sheets: HashMap<Option<usize>, Sheet> = HashMap::new();
    let mut orte: Vec<Value> = Vec::new();
    let mut placed = 0;
    let mut touched_first: Option<Option<usize>> = None;

    for (n, change) in request.changes.into_iter().enumerate() {
        let label = n + 1;
        let change = match change {
            Ok(ch) => ch,
            Err(e) => {
                errors.push(format!("Änderung {label}: {e}"));
                continue;
            }
        };
        let key = match change.target {
            Target::Current => Some(current),
            Target::Number(i) if i < c.page_count() => Some(i),
            Target::Number(i) => {
                errors.push(format!("Änderung {label}: Seite {} gibt es nicht", i + 1));
                continue;
            }
            Target::New => None,
        };
        let sheet = sheets.entry(key).or_insert_with(|| {
            let page = match key {
                Some(i) => c.page_ref(i).expect("Seite geprüft"),
                None => blank.clone().expect("leere Seite angelegt"),
            };
            let size = (page.borrow().width, page.borrow().height);
            Sheet { page, index: key, size, items: Vec::new(), placed: Vec::new(), grid: None }
        });
        let (pw, ph) = sheet.size;
        let area_of = |a: [f64; 4]| Rect::new(a[0] * pw, a[1] * ph, a[2] * pw, a[3] * ph);
        let find = |q: &str| sheet.index.and_then(|i| c.find_on_page(q, i));

        // Inhalte ohne Ort
        match &change.content {
            Content::Lines { lines, arrow, width } => {
                let mut bb: Option<Rect> = None;
                for l in lines {
                    let pts: Vec<Pt> = l.iter().map(|p| Pt::new(p[0] * pw, p[1] * ph)).collect();
                    if *arrow {
                        let (a, b) = (pts[pts.len() - 2], pts[pts.len() - 1]);
                        let d = (b.x - a.x).atan2(b.y - a.y);
                        let head = |t: f64| Pt::new(b.x - 7.0 * (d + t).sin(), b.y - 7.0 * (d + t).cos());
                        let s = Stroke::new(StrokeTool::Pen, change.color, *width, vec![head(0.45), b, head(-0.45)], None, Vec::new());
                        sheet.items.push(Item::Stroke(s));
                    }
                    let s = Stroke::new(StrokeTool::Pen, change.color, *width, pts, None, Vec::new());
                    bb = Some(bb.map_or(s.bbox, |r| r.union(&s.bbox)));
                    sheet.items.push(Item::Stroke(s));
                }
                if let Some(r) = bb {
                    sheet.placed.push(r);
                }
                orte.push(json!({"aenderung": label}));
                placed += 1;
                touched_first.get_or_insert(key);
                continue;
            }
            Content::Highlight(q) => {
                let Some(rects) = find(q) else {
                    errors.push(format!("Änderung {label}: Text nicht gefunden: {q}"));
                    continue;
                };
                let tint = if change.color == PALETTE[0] { PALETTE[8] } else { change.color }.with_alpha(0.5);
                for r in rects {
                    let y = r.center().y;
                    let s = Stroke::new(StrokeTool::Highlighter, tint, r.height().max(4.0), vec![Pt::new(r.x0, y), Pt::new(r.x1, y)], None, Vec::new());
                    sheet.items.push(Item::Stroke(s));
                }
                orte.push(json!({"aenderung": label}));
                placed += 1;
                touched_first.get_or_insert(key);
                continue;
            }
            _ => {}
        }

        let width_room = match &change.spot {
            Spot::Area(a, _) => (a[2] - a[0]) * pw,
            Spot::Point(p) => (pw - PAGE_MARGIN - p[0] * pw).max(40.0),
            _ => pw * 0.6,
        };
        let mut piece = match &change.content {
            Content::Text { text, size } if latex::needs_typesetting(text) => match typeset_done.remove(&n) {
                Some((png, w, h)) => Piece::Picture(png, w, h),
                None => {
                    errors.push(format!("Änderung {label}: Formel ließ sich nicht setzen"));
                    continue;
                }
            },
            Content::Text { text, size } => {
                let width_of = |t: &str| text_item(t, 0.0, 0.0, *size, change.color).extent().0;
                Piece::Text(wrap(text, width_room, &width_of), *size)
            }
            Content::Image(path) => match std::fs::read(path).ok().and_then(|b| {
                let pb = gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(b)).ok()?;
                Some((pb.save_to_bufferv("png", &[]).ok()?, pb.width() as f64 * 0.5, pb.height() as f64 * 0.5))
            }) {
                Some((png, w, h)) => Piece::Picture(png, w, h),
                None => {
                    errors.push(format!("Änderung {label}: Bild nicht lesbar: {}", path.display()));
                    continue;
                }
            },
            _ => unreachable!("oben behandelt"),
        };
        let measure = |p: &Piece| match p {
            Piece::Text(t, s) => text_item(t, 0.0, 0.0, *s, change.color).extent(),
            Piece::Picture(_, w, h) => (*w, *h),
        };
        let scale_to = |p: &mut Piece, k: f64| match p {
            Piece::Text(_, s) => *s *= k,
            Piece::Picture(_, w, h) => {
                *w *= k;
                *h *= k;
            }
        };
        // nie breiter als die Seite
        let (w, h) = measure(&piece);
        let k = ((pw - 2.0 * GAP) / w).min((ph - 2.0 * GAP) / h).min(1.0);
        if k < 1.0 {
            scale_to(&mut piece, k);
        }
        let top_left = match &change.spot {
            Spot::Area(a, align) => {
                let area = area_of(*a);
                let (w, h) = measure(&piece);
                // etwas Luft zum Zellenrand
                let k = ((area.width() - 3.0).max(1.0) / w).min((area.height() - 2.0).max(1.0) / h).min(1.0);
                if k < 1.0 {
                    scale_to(&mut piece, k);
                }
                let (w, h) = measure(&piece);
                Some(align_in(area, w, h, *align))
            }
            Spot::Point(p) => Some(Pt::new(p[0] * pw, p[1] * ph)),
            Spot::Beside(q, side) => match find(q) {
                Some(rects) => {
                    let t = rects.iter().skip(1).fold(rects[0], |a, b| a.union(b));
                    let (w, h) = measure(&piece);
                    Some(beside(t, w, h, *side))
                }
                None => {
                    errors.push(format!("Änderung {label}: Text nicht gefunden: {q}"));
                    continue;
                }
            },
            Spot::Free(near) => {
                let near_pt = near.as_deref().and_then(|q| match find(q) {
                    Some(r) => Some(r[0].center()),
                    None => {
                        errors.push(format!("Änderung {label}: Text nicht gefunden: {q}, stattdessen irgendwo frei"));
                        None
                    }
                });
                if sheet.grid.is_none() {
                    let mut g = match sheet.index.and_then(|i| c.page_data(i)) {
                        Some(d) => Grid::of_page(&d),
                        None => Grid::empty(pw, ph),
                    };
                    for r in &sheet.placed {
                        g.mark(*r);
                    }
                    sheet.grid = Some(g);
                }
                let grid = sheet.grid.as_ref().expect("gerade angelegt");
                let mut spot = None;
                for k in [1.0, 0.85, 0.7] {
                    if k < 1.0 {
                        scale_to(&mut piece, if k == 0.85 { 0.85 } else { 0.7 / 0.85 });
                    }
                    let (w, h) = measure(&piece);
                    spot = grid.free_spot(w, h, near_pt);
                    if spot.is_some() {
                        break;
                    }
                }
                if spot.is_none() {
                    errors.push(format!("Änderung {label}: kein freier Platz auf der Seite"));
                    continue;
                }
                spot
            }
        };
        let (w, h) = measure(&piece);
        let p = top_left.expect("Ort bestimmt");
        // sichtbar auf der Seite halten
        let p = Pt::new(p.x.min(pw - w).max(0.0), p.y.min(ph - h).max(0.0));
        let rect = Rect::new(p.x, p.y, p.x + w, p.y + h);
        let item = match piece {
            Piece::Text(t, s) => Item::Text(text_item(&t, p.x, p.y, s, change.color)),
            Piece::Picture(png, ..) => picture_item(&png, rect),
        };
        sheet.items.push(item);
        sheet.placed.push(rect);
        if let Some(g) = &mut sheet.grid {
            g.mark(rect);
        }
        orte.push(json!({"aenderung": label, "bereich": [round(rect.x0 / pw), round(rect.y0 / ph), round(rect.x1 / pw), round(rect.y1 / ph)]}));
        placed += 1;
        touched_first.get_or_insert(key);
    }

    let batch: Vec<(PageRef, Vec<Item>)> = sheets.into_values().filter(|s| !s.items.is_empty()).map(|s| (s.page, s.items)).collect();
    let mut seite = Value::Null;
    if !batch.is_empty() {
        let blank_used = needs_blank && touched_first.is_some();
        let shown = touched_first.flatten().map(|i| if blank_used && i > current { i + 1 } else { i }).unwrap_or(current + 1);
        let serial = c.place_batch(blank.filter(|_| blank_used).map(|p| (current, p)), batch, shown);
        last.borrow_mut().push(serial);
        seite = json!(shown + 1);
        if let Some(data) = c.page_data(shown) {
            let target = cache.join(RESULT_IMAGE);
            let _ = gio::spawn_blocking(move || {
                let scale = 1100.0 / data.width.max(1.0);
                if let Some(s) = surface(&data, scale, true) {
                    if let Ok(mut f) = std::fs::File::create(&target) {
                        let _ = s.write_to_png(&mut f);
                    }
                }
            })
            .await;
        }
    }
    // zuletzt: Auf diese Datei wartet Claude, das Bild muss dann schon da sein
    let out = json!({"eingefuegt": placed, "rueckgaengig": undone, "fehler": errors, "orte": orte, "seite": seite});
    let _ = std::fs::write(cache.join(RESULT), out.to_string());
    Some(Outcome { placed, undone, errors })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(v: Value) -> Change {
        parse_change(&v).unwrap()
    }

    #[test]
    fn reads_changes() {
        let c = one(json!({"seite": 4, "bereich": [0.5, 0.2, 0.3, 0.4], "text": "1→2", "ausrichtung": "links", "farbe": "blau"}));
        assert_eq!(c.target, Target::Number(3));
        assert_eq!(c.spot, Spot::Area([0.3, 0.2, 0.5, 0.4], Align::Start));
        assert_eq!(c.color, PALETTE[1]);
        assert_eq!(one(json!({"text": "x"})).spot, Spot::Free(None));
        assert_eq!(one(json!({"seite": "neu", "text": "x", "nahe": "A"})).spot, Spot::Free(Some("A".into())));
        assert_eq!(one(json!({"text": "x", "neben": "A", "richtung": "unten"})).spot, Spot::Beside("A".into(), Side::Below));
        assert!(matches!(one(json!({"linien": [[[0, 0], [1, 1]], [[0, 0]]], "pfeil": true})).content, Content::Lines { lines, arrow: true, .. } if lines.len() == 1));
        assert!(parse_change(&json!({"bereich": [0, 0, 1, 1]})).is_err());
        assert!(parse_change(&json!({"text": "x", "seite": 0})).is_err());
        assert!(parse_change(&json!({"text": "x", "farbe": "pink"})).is_err());
    }

    #[test]
    fn half_written_file_waits() {
        assert_eq!(parse_request(r#"{"aenderungen":[{"text":"#), None);
        let r = parse_request(r#"{"rueckgaengig":true,"geometrie":[5]}"#).unwrap();
        assert!(r.undo);
        assert_eq!(r.geometry, [4]);
    }

    #[test]
    fn content_gets_a_margin() {
        let mut cells = vec![false; 25];
        cells[12] = true;
        let g = grow(&cells, 5, 5, 1);
        assert_eq!(g.iter().filter(|c| **c).count(), 9);
        assert!(g[6] && g[18] && !g[0]);
    }

    #[test]
    fn ink_map_counts_areas() {
        let m = InkMap::from_cells(10, 10, |x, y| x < 2 && y < 3);
        assert_eq!(m.covered(&Rect::new(0.0, 0.0, 30.0, 30.0)), 6.0 * 9.0);
        assert_eq!(m.covered(&Rect::new(6.0, 0.0, 30.0, 30.0)), 0.0);
        assert_eq!(m.covered(&Rect::new(-50.0, -50.0, 3.0, 3.0)), 9.0);
    }

    #[test]
    fn free_spot_avoids_ink() {
        let mut g = Grid::empty(200.0, 200.0);
        g.mark(Rect::new(0.0, 0.0, 200.0, 100.0));
        let p = g.free_spot(50.0, 20.0, None).unwrap();
        assert!(p.y >= 100.0 && p.y + 20.0 <= 200.0, "{p:?}");
        // nah am Wunschort, nicht einfach oben
        let q = g.free_spot(50.0, 20.0, Some(Pt::new(150.0, 170.0))).unwrap();
        assert!(q.x > 90.0 && q.y > 130.0, "{q:?}");
        assert!(g.free_spot(50.0, 150.0, None).is_none());
    }

    #[test]
    fn finds_grid_lines() {
        // 2 px dicke Waagrechte, eine Senkrechte, ein kurzer Strich wie ein Buchstabe
        let dark = |x: usize, y: usize| ((20..=21).contains(&y) && (10..90).contains(&x)) || (x == 50 && (5..60).contains(&y)) || (y == 80 && (5..12).contains(&x));
        let l = straight_lines(100, 100, dark, 40);
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!((l[0][0], l[0][2]), (10, 89));
        assert!(l[0][1] == 20 && l[0][3] == 20, "{l:?}");
        assert_eq!(l[1], [50, 5, 50, 59]);
    }

    #[test]
    fn snaps_to_a_whole_term() {
        // zwei "Zeichen": x 10..=20 und x 30..=40; der Bereich trifft das erste nur halb
        let dark = |x: usize, y: usize| (10..=40).contains(&y) && ((10..=20).contains(&x) || (30..=40).contains(&x));
        assert_eq!(tight_box(60, 60, dark, [15, 20, 18, 25], 30), Some([10, 10, 20, 40]));
        // die Lücke trennt: Bereich über beide bleibt beide
        assert_eq!(tight_box(60, 60, dark, [12, 12, 35, 20], 30), Some([10, 10, 40, 40]));
        assert_eq!(tight_box(60, 60, dark, [45, 45, 50, 50], 30), None);
    }

    #[test]
    fn wraps_words() {
        let by_len = |t: &str| t.chars().count() as f64;
        assert_eq!(wrap("aa bb cc", 5.0, &by_len), "aa bb\ncc");
        assert_eq!(wrap("langeswort x", 5.0, &by_len), "langeswort\nx");
        assert_eq!(wrap("a\n\nb", 5.0, &by_len), "a\n\nb");
    }

    #[test]
    fn places_beside_and_in_area() {
        let t = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(beside(t, 5.0, 4.0, Side::Right), Pt::new(26.0, 13.0));
        assert_eq!(beside(t, 5.0, 4.0, Side::Below), Pt::new(10.0, 26.0));
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(align_in(a, 4.0, 2.0, Align::Center), Pt::new(3.0, 4.0));
        assert_eq!(align_in(a, 4.0, 2.0, Align::End), Pt::new(6.0, 4.0));
    }
}
