//! Cairo-Rasterisierung. Wird von den Render-Threads (Kacheln), der Live-Vorschau
//! des aktiven Strichs und dem PDF-Export gemeinsam genutzt. Koordinaten in pt.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use cairo::{Context, Format, ImageSurface, LineCap, LineJoin};
use gtk::pango;
use gtk::gdk_pixbuf::prelude::*;

use crate::geom::{Color, Pt, Rect};
use crate::model::*;

// ------------------------------------------------------------------------ Striche

/// Strichdarstellung "Klassisch" (Segmente wie Xournal++) statt glatter Fläche.
pub static CLASSIC_INK: AtomicBool = AtomicBool::new(false);

fn cap(s: &Stroke) -> LineCap {
    match s.attr("capStyle") {
        Some("butt") => LineCap::Butt,
        Some("square") => LineCap::Square,
        _ => LineCap::Round,
    }
}

fn dash(s: &Stroke) -> Option<Vec<f64>> {
    let w = s.width.max(0.5);
    match s.attr("style")? {
        "dash" => Some(vec![6.0 * w, 3.0 * w]),
        "dashdot" => Some(vec![6.0 * w, 3.0 * w, 0.5 * w, 3.0 * w]),
        "dot" => Some(vec![0.5 * w, 3.0 * w]),
        st if st.starts_with("cust:") => {
            let v: Vec<f64> = st[5..].split_whitespace().filter_map(|x| x.parse().ok()).collect();
            (!v.is_empty()).then_some(v)
        }
        _ => None,
    }
}

/// Glatter Pfad: quadratische Kurven durch die Segment-Mittelpunkte.
pub fn smooth_path(cr: &Context, pts: &[Pt]) {
    cr.move_to(pts[0].x, pts[0].y);
    if pts.len() < 3 {
        for p in &pts[1..] {
            cr.line_to(p.x, p.y);
        }
        return;
    }
    // Echte Ecken (Knick über 60°, z. B. Rechtecke, Dreiecke) bleiben spitz, sonst
    // würde die Glättung sie über die Kantenmitten abschneiden.
    let mut cur = pts[0];
    for i in 1..pts.len() - 1 {
        let (a, c, b) = (pts[i - 1], pts[i], pts[i + 1]);
        let m = c.lerp(b, 0.5);
        let (l1, l2) = (a.dist(c), c.dist(b));
        let cos = if l1 > 1e-9 && l2 > 1e-9 { ((c.x - a.x) * (b.x - c.x) + (c.y - a.y) * (b.y - c.y)) / (l1 * l2) } else { 1.0 };
        if cos < 0.5 {
            cr.line_to(c.x, c.y);
            cr.line_to(m.x, m.y);
        } else {
            cr.curve_to(
                cur.x + 2.0 / 3.0 * (c.x - cur.x),
                cur.y + 2.0 / 3.0 * (c.y - cur.y),
                m.x + 2.0 / 3.0 * (c.x - m.x),
                m.y + 2.0 / 3.0 * (c.y - m.y),
                m.x,
                m.y,
            );
        }
        cur = m;
    }
    let last = pts[pts.len() - 1];
    cr.line_to(last.x, last.y);
}

pub fn draw_stroke(cr: &Context, s: &Stroke, over: Option<Color>) {
    let c = over.unwrap_or(s.color);
    let pts = &s.pts;
    cr.set_line_cap(cap(s));
    cr.set_line_join(LineJoin::Round);
    if pts.len() == 1 {
        cr.arc(pts[0].x, pts[0].y, s.width_at(0) / 2.0, 0.0, 2.0 * PI);
        cr.set_source_rgba(c.r, c.g, c.b, c.a);
        let _ = cr.fill();
        return;
    }
    let dash = dash(s);
    if let Some(d) = &dash {
        cr.set_dash(d, 0.0);
    }
    if s.widths.is_none() || s.tool == StrokeTool::Highlighter {
        smooth_path(cr, pts);
        if let Some(f) = s.fill() {
            cr.set_source_rgba(c.r, c.g, c.b, f);
            let _ = cr.fill_preserve();
        }
        cr.set_source_rgba(c.r, c.g, c.b, c.a);
        cr.set_line_width(s.width);
        let _ = cr.stroke();
    } else {
        let ws = s.widths.as_ref().unwrap();
        if dash.is_none() && !CLASSIC_INK.load(Ordering::Relaxed) {
            // Druckstrich als eine zusammenhängende Fläche: Kreise an den Punkten +
            // tangentiale Vierecke dazwischen. Breitenwechsel sind so stufenlos, und
            // Transparenz stimmt ohne Gruppe (eine Füllung, keine Überlappung).
            cr.set_fill_rule(cairo::FillRule::Winding);
            pressure_outline(cr, pts, ws);
            cr.set_source_rgba(c.r, c.g, c.b, c.a);
            let _ = cr.fill();
        } else {
            let group = c.a < 1.0;
            if group {
                cr.push_group();
                cr.set_source_rgb(c.r, c.g, c.b);
            } else {
                cr.set_source_rgba(c.r, c.g, c.b, c.a);
            }
            let n = pts.len();
            let mut i = 0;
            while i < n - 1 {
                let w0 = ws[i];
                cr.move_to(pts[i].x, pts[i].y);
                let mut j = i + 1;
                cr.line_to(pts[j].x, pts[j].y);
                while j < n - 1 && (ws[j] - w0).abs() <= 0.03 * w0 + 0.01 {
                    j += 1;
                    cr.line_to(pts[j].x, pts[j].y);
                }
                cr.set_line_width(w0.max(0.05));
                let _ = cr.stroke();
                i = j;
            }
            if group {
                let _ = cr.pop_group_to_source();
                let _ = cr.paint_with_alpha(c.a);
            }
        }
    }
    if dash.is_some() {
        cr.set_dash(&[], 0.0);
    }
}

/// Umriss eines Strichs mit variabler Breite (alle Teilpfade gegen den Uhrzeigersinn,
/// damit die Nonzero-Füllung sie sauber vereinigt).
pub fn pressure_outline(cr: &Context, pts: &[Pt], ws: &[f64]) {
    let n = pts.len();
    for i in 0..n {
        let r = (ws[i] / 2.0).max(0.03);
        cr.new_sub_path();
        cr.arc(pts[i].x, pts[i].y, r, 0.0, 2.0 * PI);
        cr.close_path();
    }
    for i in 0..n.saturating_sub(1) {
        let (a, b) = (pts[i], pts[i + 1]);
        let (ra, rb) = ((ws[i] / 2.0).max(0.03), (ws[i + 1] / 2.0).max(0.03));
        let d = a.dist(b);
        if d <= (ra - rb).abs() + 1e-6 {
            continue; // ein Kreis enthält den anderen
        }
        let (ux, uy) = ((b.x - a.x) / d, (b.y - a.y) / d);
        let (nx, ny) = (-uy, ux);
        // exakte äußere Tangenten zweier Kreise
        let sn = (ra - rb) / d;
        let cs = (1.0 - sn * sn).max(0.0).sqrt();
        let v1 = (ux * sn + nx * cs, uy * sn + ny * cs);
        let v2 = (ux * sn - nx * cs, uy * sn - ny * cs);
        let q = [
            Pt::new(a.x + v1.0 * ra, a.y + v1.1 * ra),
            Pt::new(b.x + v1.0 * rb, b.y + v1.1 * rb),
            Pt::new(b.x + v2.0 * rb, b.y + v2.1 * rb),
            Pt::new(a.x + v2.0 * ra, a.y + v2.1 * ra),
        ];
        // Orientierung angleichen (positive Fläche wie die Kreisbögen)
        let area: f64 = (0..4).map(|k| q[k].x * q[(k + 1) % 4].y - q[(k + 1) % 4].x * q[k].y).sum();
        cr.new_sub_path();
        if area >= 0.0 {
            cr.move_to(q[0].x, q[0].y);
            for p in &q[1..] {
                cr.line_to(p.x, p.y);
            }
        } else {
            cr.move_to(q[3].x, q[3].y);
            for p in q[..3].iter().rev() {
                cr.line_to(p.x, p.y);
            }
        }
        cr.close_path();
    }
}

// ------------------------------------------------------------------------ Text

thread_local! {
    static MEASURE: Context = Context::new(ImageSurface::create(Format::ARgb32, 1, 1).unwrap()).unwrap();
}

pub fn text_layout(cr: &Context, font: &str, size: f64, text: &str) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut desc = pango::FontDescription::from_string(font);
    desc.set_absolute_size(size * pango::SCALE as f64);
    layout.set_font_description(Some(&desc));
    layout.set_text(text);
    layout
}

pub fn measure_text(t: &TextItem) -> (f64, f64) {
    MEASURE.with(|cr| {
        let l = text_layout(cr, &t.font, t.size, &t.text);
        let (_, logical) = l.extents();
        let s = pango::SCALE as f64;
        ((logical.width() as f64 / s).max(4.0), (logical.height() as f64 / s).max(t.size))
    })
}

pub fn draw_text(cr: &Context, t: &TextItem, over: Option<Color>) {
    let c = over.unwrap_or(t.color);
    cr.save().ok();
    cr.move_to(t.x, t.y);
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
    let l = text_layout(cr, &t.font, t.size, &t.text);
    pangocairo::functions::show_layout(cr, &l);
    cr.restore().ok();
}

// ------------------------------------------------------------------------ Bilder

/// Dunkelmodus-Filter der Seite (Helligkeit umdrehen, Farbton behalten):
/// aus = K · (c − 2·L(c)) + OFF.
pub const DARK_K: f32 = 0.84;
pub const DARK_OFF: f32 = 0.08 + DARK_K;

pub fn decode(data: &str) -> Option<DecodedImage> {
    let mut surf = decode_image_raw(data)?;
    // Fotos bleiben auf dunklem Papier unverändert, Screenshots werden umgefärbt
    let dark = match adapt_to_paper(&mut surf) {
        Backdrop::Other => None,
        _ => dark_variant(&surf),
    };
    Some(DecodedImage { surf, dark })
}

/// Farbe so, wie sie der Dunkelmodus-Filter zeigt (gleiche Formel wie die GPU-Matrix).
pub fn dark_color(c: Color) -> Color {
    let (k, off) = (DARK_K as f64, DARK_OFF as f64);
    let l = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    let f = |x: f64| (k * (x - 2.0 * l) + off).clamp(0.0, 1.0);
    Color { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a }
}

/// Screenshot für dunkles Papier: jedes Pixel durch den Dunkelmodus-Filter.
fn dark_variant(src: &ImageSurface) -> Option<ImageSurface> {
    let mut out = ImageSurface::create(Format::ARgb32, src.width(), src.height()).ok()?;
    {
        let cr = Context::new(&out).ok()?;
        cr.set_source_surface(src, 0.0, 0.0).ok()?;
        cr.paint().ok()?;
    }
    out.flush();
    {
        let mut data = out.data().ok()?;
        for px in data.chunks_exact_mut(4) {
            let a = px[3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            // BGR, vormultipliziert
            let c = Color::rgb(
                (px[2] as f32 / 255.0 / a).min(1.0) as f64,
                (px[1] as f32 / 255.0 / a).min(1.0) as f64,
                (px[0] as f32 / 255.0 / a).min(1.0) as f64,
            );
            let d = dark_color(c);
            for (k, v) in [d.b, d.g, d.r].into_iter().enumerate() {
                px[k] = (v as f32 * a * 255.0).round() as u8;
            }
        }
    }
    out.mark_dirty();
    Some(out)
}

/// Eigene Kopie einer Surface (zum Verändern, ohne geteilte Bilder anzufassen).
pub fn copy_surface(src: &ImageSurface) -> Option<ImageSurface> {
    let out = ImageSurface::create(Format::ARgb32, src.width(), src.height()).ok()?;
    {
        let cr = Context::new(&out).ok()?;
        cr.set_source_surface(src, 0.0, 0.0).ok()?;
        cr.set_operator(cairo::Operator::Source);
        cr.paint().ok()?;
    }
    Some(out)
}

/// Bild so dekodieren, wie es in der Datei steht (ohne Anpassung ans Papier).
pub fn decode_image_raw(data: &str) -> Option<ImageSurface> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim().as_bytes()).ok()?;
    let src = match ImageSurface::create_from_png(&mut &bytes[..]) {
        Ok(s) => s,
        Err(_) => {
            // JPEG & Co. über GdkPixbuf
            let loader = gtk::gdk_pixbuf::PixbufLoader::new();
            loader.write(&bytes).ok()?;
            loader.close().ok()?;
            let pb = loader.pixbuf()?;
            let surf = ImageSurface::create(Format::ARgb32, pb.width(), pb.height()).ok()?;
            let cr = Context::new(&surf).ok()?;
            gtk::gdk::prelude::GdkCairoContextExt::set_source_pixbuf(&cr, &pb, 0.0, 0.0);
            cr.paint().ok()?;
            drop(cr);
            surf
        }
    };
    // immer in ein eigenes ARGB-Bild kopieren (PNGs ohne Alphakanal sind RGB24)
    let surf = ImageSurface::create(Format::ARgb32, src.width(), src.height()).ok()?;
    {
        let cr = Context::new(&surf).ok()?;
        cr.set_source_surface(&src, 0.0, 0.0).ok()?;
        cr.paint().ok()?;
    }
    Some(surf)
}

/// Hintergrund eines Bildes: fast weiß, fast schwarz oder etwas anderes (Foto, bunt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backdrop {
    Light,
    Dark,
    Other,
}

/// Überwiegende Farbe: zählt fast weiße und fast schwarze (graue) Pixel und gibt
/// die Hintergrundfarbe zurück (BGR, nicht vormultipliziert): das hellste bzw.
/// dunkelste dieser Pixel je Kanal. Der Mittelwert läge knapp neben reinem Weiß,
/// und „Farbe zu Alpha“ macht alles, was jenseits der Hintergrundfarbe liegt, deckend.
pub fn backdrop(surf: &mut ImageSurface) -> (Backdrop, [f32; 3]) {
    surf.flush();
    let Ok(data) = surf.data() else { return (Backdrop::Other, [0.0; 3]) };
    let (mut light, mut dark, mut n) = (0usize, 0usize, 0usize);
    let (mut max_l, mut min_d) = ([0f32; 3], [1f32; 3]);
    // jedes 7. Pixel reicht
    for px in data.chunks_exact(4).step_by(7) {
        let a = px[3] as f32 / 255.0;
        if a < 0.5 {
            continue;
        }
        n += 1;
        let c = [px[0] as f32 / 255.0 / a, px[1] as f32 / 255.0 / a, px[2] as f32 / 255.0 / a];
        let (lo, hi) = (c.iter().cloned().fold(1.0f32, f32::min), c.iter().cloned().fold(0.0f32, f32::max));
        if lo > 0.88 {
            light += 1;
            (0..3).for_each(|k| max_l[k] = max_l[k].max(c[k]));
        } else if hi < 0.25 && hi - lo < 0.08 {
            dark += 1;
            (0..3).for_each(|k| min_d[k] = min_d[k].min(c[k]));
        }
    }
    if n == 0 {
        return (Backdrop::Other, [0.0; 3]);
    }
    if light * 10 >= n * 4 && light >= dark {
        (Backdrop::Light, max_l.map(|v| v.min(1.0)))
    } else if dark * 10 >= n * 4 {
        (Backdrop::Dark, min_d)
    } else {
        (Backdrop::Other, [0.0; 3])
    }
}

/// Screenshots an das Papier anpassen: Ist der Hintergrund fast weiß oder fast
/// schwarz, wird er durchsichtig („Farbe zu Alpha“). Weiße Bilder bleiben wie sie
/// sind; auf hellem Papier passen sie, im Dunkelmodus invertiert sie der Farbfilter.
/// Dunkle Bilder werden zusätzlich vorab invertiert: auf hellem Papier erscheinen
/// sie so hell-auf-dunkel umgedreht, im Dunkelmodus hebt der Filter das wieder auf.
/// Fotos und bunte Bilder bleiben unverändert.
pub fn adapt_to_paper(surf: &mut ImageSurface) -> Backdrop {
    let (kind, key) = backdrop(surf);
    if kind == Backdrop::Other {
        return kind;
    }
    let Ok(mut data) = surf.data() else { return kind };
    // fast Hintergrundfarbe (JPEG-Rauschen, leichte Graustufen) ganz entfernen
    const T: f32 = 0.06;
    for px in data.chunks_exact_mut(4) {
        // Cairo ARGB32 (little endian): B, G, R, A, vormultipliziert
        let a0 = px[3] as f32 / 255.0;
        if a0 <= 0.0 {
            continue;
        }
        let c = [px[0] as f32 / 255.0 / a0, px[1] as f32 / 255.0 / a0, px[2] as f32 / 255.0 / a0].map(|v| v.min(1.0));
        // Abstand zur Hintergrundfarbe = neue Deckkraft („Farbe zu Alpha“)
        let raw = (0..3)
            .map(|k| {
                let (x, b) = (c[k], key[k]);
                if x > b { (x - b) / (1.0 - b).max(1e-3) } else { (b - x) / b.max(1e-3) }
            })
            .fold(0.0f32, f32::max);
        let a = ((raw - T) / (1.0 - T)).clamp(0.0, 1.0);
        if a <= 0.0 {
            px.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        // Farbe so wählen, dass sie über dem Hintergrund genau wie vorher aussieht
        let mut v = [0f32; 3];
        for k in 0..3 {
            v[k] = (key[k] + (c[k] - key[k]) / a).clamp(0.0, 1.0);
        }
        if kind == Backdrop::Dark {
            // Helligkeit umdrehen, Farbton behalten (Gegenstück zum Dunkelmodus-Filter)
            let l = 0.0722 * v[0] + 0.7152 * v[1] + 0.2126 * v[2];
            v = v.map(|x| (x + 1.0 - 2.0 * l).clamp(0.0, 1.0));
        }
        let na = a * a0;
        for k in 0..3 {
            px[k] = (v[k] * na * 255.0).round() as u8;
        }
        px[3] = (na * 255.0).round() as u8;
    }
    drop(data);
    surf.mark_dirty();
    kind
}

fn draw_image_data(cr: &Context, rect: Rect, data: &str, cache: &ImageCache, dark: bool) {
    let dec = cache.get_or_init(|| decode(data));
    match dec {
        Some(d) => {
            let surf = if dark { d.dark.as_ref().unwrap_or(&d.surf) } else { &d.surf };
            cr.save().ok();
            cr.rectangle(rect.x0, rect.y0, rect.width(), rect.height());
            cr.clip();
            cr.translate(rect.x0, rect.y0);
            cr.scale(rect.width() / surf.width() as f64, rect.height() / surf.height() as f64);
            let _ = cr.set_source_surface(surf, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            cr.restore().ok();
        }
        None => {
            cr.rectangle(rect.x0, rect.y0, rect.width(), rect.height());
            cr.set_source_rgba(0.8, 0.2, 0.2, 0.25);
            let _ = cr.fill();
        }
    }
}

/// `dark`: für dunkles Papier zeichnen (Farben umgedreht, Fotos unverändert).
pub fn draw_item_in(cr: &Context, it: &Item, over: Option<Color>, dark: bool) {
    match it {
        Item::Stroke(s) => draw_stroke(cr, s, over.or_else(|| dark.then(|| dark_color(s.color)))),
        Item::Text(t) => draw_text(cr, t, over.or_else(|| dark.then(|| dark_color(t.color)))),
        Item::Image(i) => {
            // gedrehtes Bild: um seine Mitte drehen (Attribut von Graphit)
            let a = i.angle();
            cr.save().ok();
            if a != 0.0 {
                let c = i.rect.center();
                cr.translate(c.x, c.y);
                cr.rotate(a.to_radians());
                cr.translate(-c.x, -c.y);
            }
            draw_image_data(cr, i.rect, &i.data, &i.decoded, dark);
            cr.restore().ok();
        }
        Item::Raw(r) => {
            // teximage enthält i. d. R. ein base64-PNG
            if let Some(b) = r.bbox {
                if !r.elem.text.is_empty() {
                    draw_image_data(cr, b, &r.elem.text, &r.decoded, dark);
                }
            }
        }
    }
}

pub fn draw_items(cr: &Context, layers: &[ItemList], clip: Option<Rect>) {
    draw_items_in(cr, layers, clip, false)
}

pub fn draw_items_in(cr: &Context, layers: &[ItemList], clip: Option<Rect>, dark: bool) {
    for layer in layers {
        for it in layer.iter() {
            if let (Some(c), Some(b)) = (clip, it.bbox()) {
                if !c.intersects(&b) {
                    continue;
                }
            }
            draw_item_in(cr, it, None, dark);
        }
    }
}

// ------------------------------------------------------------------------ Hintergrund

const GRID: f64 = 14.17;

pub fn draw_background(cr: &Context, bg: &Background, w: f64, h: f64, clip: Rect, dark: bool) {
    let rgba = |r: f64, g: f64, b: f64, a: f64| {
        let c = Color { r, g, b, a };
        let c = if dark { dark_color(c) } else { c };
        cr.set_source_rgba(c.r, c.g, c.b, c.a);
    };
    let c = bg.color();
    rgba(c.r, c.g, c.b, 1.0);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();
    if bg.kind() != "solid" {
        return;
    }
    let (x0, y0) = (clip.x0.max(0.0), clip.y0.max(0.0));
    let (x1, y1) = (clip.x1.min(w), clip.y1.min(h));
    cr.set_line_width(0.5);
    rgba(0.25, 0.63, 1.0, 0.5);
    match bg.style() {
        "lined" | "ruled" | "staves" => {
            let start = if bg.style() == "lined" { 80.0 } else { 24.0 };
            let mut y = start;
            while y < h {
                if y >= y0 - 1.0 && y <= y1 + 1.0 {
                    cr.move_to(x0, y);
                    cr.line_to(x1, y);
                }
                y += 24.0;
            }
            let _ = cr.stroke();
            if bg.style() == "lined" {
                rgba(1.0, 0.0, 0.5, 0.5);
                cr.move_to(72.0, y0);
                cr.line_to(72.0, y1);
                let _ = cr.stroke();
            }
        }
        "graph" | "isograph" => {
            let mut x = (x0 / GRID).floor() * GRID;
            while x <= x1 {
                cr.move_to(x, y0);
                cr.line_to(x, y1);
                x += GRID;
            }
            let mut y = (y0 / GRID).floor() * GRID;
            while y <= y1 {
                cr.move_to(x0, y);
                cr.line_to(x1, y);
                y += GRID;
            }
            let _ = cr.stroke();
        }
        "dotted" | "isodotted" => {
            rgba(0.35, 0.35, 0.4, 0.8);
            let mut y = (y0 / GRID).floor() * GRID;
            while y <= y1 {
                let mut x = (x0 / GRID).floor() * GRID;
                while x <= x1 {
                    cr.new_sub_path();
                    cr.arc(x, y, 0.6, 0.0, 2.0 * PI);
                    x += GRID;
                }
                y += GRID;
            }
            let _ = cr.fill();
        }
        _ => {}
    }
}

// ------------------------------------------------------------------------ PDF

thread_local! {
    /// Jeder Thread hat eigene Poppler-Dokumente (Poppler ist nicht thread-sicher).
    static PDFS: RefCell<HashMap<PathBuf, Option<poppler::Document>>> = RefCell::new(HashMap::new());
}

pub fn with_pdf<R>(path: &Path, f: impl FnOnce(&poppler::Document) -> R) -> Option<R> {
    PDFS.with(|m| {
        let mut m = m.borrow_mut();
        let doc = m.entry(path.to_path_buf()).or_insert_with(|| {
            let uri = gtk::glib::filename_to_uri(path, None).ok()?;
            poppler::Document::from_file(&uri, None).ok()
        });
        doc.as_ref().map(f)
    })
}

pub fn forget_pdf(path: &Path) {
    PDFS.with(|m| m.borrow_mut().remove(path));
}

pub fn pdf_page_sizes(path: &Path) -> Result<Vec<(f64, f64)>, String> {
    forget_pdf(path);
    with_pdf(path, |d| (0..d.n_pages()).filter_map(|i| d.page(i)).map(|p| p.size()).collect())
        .ok_or_else(|| crate::tr!("PDF kann nicht geöffnet werden: {}", "PDF cannot be opened: {}", path.display()))
}

pub fn draw_pdf_page(cr: &Context, path: &Path, idx: usize, printing: bool) -> bool {
    with_pdf(path, |d| match d.page(idx as i32) {
        Some(p) => {
            if printing { p.render_for_printing(cr) } else { p.render(cr) }
            true
        }
        None => false,
    })
    .unwrap_or(false)
}

/// PDF-Hintergrund mit eingefügten Lücken: jedes Teilstück wird um die Summe der
/// Lücken darüber nach unten verschoben gezeichnet.
pub fn draw_pdf_background(cr: &Context, path: &Path, bg: &Background, page_w: f64, page_h: f64, printing: bool) -> bool {
    let splits = bg.pdf_splits();
    if splits.is_empty() {
        return draw_pdf_page(cr, path, bg.pdf_index(), printing);
    }
    let mut ok = true;
    let mut from = 0.0; // Anfang des Teilstücks im PDF
    let mut off = 0.0;
    for k in 0..=splits.len() {
        let to = splits.get(k).map(|s| s.0).unwrap_or(page_h.max(from) + 1e5);
        cr.save().ok();
        cr.rectangle(0.0, from + off, page_w, to - from);
        cr.clip();
        cr.translate(0.0, off);
        ok &= draw_pdf_page(cr, path, bg.pdf_index(), printing);
        cr.restore().ok();
        if let Some(s) = splits.get(k) {
            from = s.0;
            off += s.1;
        }
    }
    ok
}

// ------------------------------------------------------------------------ ganze Seite (Export, Vorschau)

pub fn draw_page(cr: &Context, page: &PageData, printing: bool) {
    let full = Rect::new(0.0, 0.0, page.width, page.height);
    cr.save().ok();
    cr.rectangle(0.0, 0.0, page.width, page.height);
    cr.clip();
    match (&page.pdf, page.bg.is_pdf()) {
        (Some(pdf), true) => {
            if !printing {
                cr.set_source_rgb(1.0, 1.0, 1.0);
                let _ = cr.paint();
            }
            draw_pdf_background(cr, pdf, &page.bg, page.width, page.height, printing);
        }
        _ => draw_background(cr, &page.bg, page.width, page.height, full, false),
    }
    draw_items(cr, &page.layers, None);
    cr.restore().ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Stroke, StrokeTool};

    /// Rechtecke aus dem Form-Werkzeug müssen spitze Ecken behalten.
    #[test]
    fn rect_corners_stay_sharp() {
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, 100, 100).unwrap();
        let cr = Context::new(&surf).unwrap();
        let r = Rect::new(10.0, 10.0, 90.0, 90.0);
        let pts = crate::shapes::build(crate::tools::ShapeKind::Rect, Pt::new(r.x0, r.y0), Pt::new(r.x1, r.y1), 2.0).remove(0);
        let s = Stroke::new(StrokeTool::Pen, Color::rgb(0.0, 0.0, 0.0), 2.0, pts, None, Vec::new());
        draw_stroke(&cr, &s, None);
        drop(cr);
        surf.flush();
        let stride = surf.stride() as usize;
        let data = surf.take_data().unwrap();
        let alpha = |x: usize, y: usize| data[y * stride + x * 4 + 3];
        for (x, y) in [(10, 10), (89, 10), (89, 89), (10, 89)] {
            assert!(alpha(x, y) > 200, "Ecke ({x},{y}) fehlt: {}", alpha(x, y));
        }
    }
}

#[cfg(test)]
mod alpha_tests {
    use super::*;

    /// Bild mit Hintergrund `bg` und einem Pixel `fg` in der Ecke.
    fn img(bg: f64, fg: f64) -> ImageSurface {
        let surf = ImageSurface::create(Format::ARgb32, 10, 10).unwrap();
        {
            let cr = Context::new(&surf).unwrap();
            cr.set_source_rgb(bg, bg, bg);
            cr.paint().unwrap();
            cr.set_source_rgb(fg, fg, fg);
            cr.rectangle(0.0, 0.0, 1.0, 1.0);
            cr.fill().unwrap();
        }
        surf
    }

    fn px(s: ImageSurface, i: usize) -> (u8, u8) {
        let d = s.take_data().unwrap();
        (d[i * 4], d[i * 4 + 3])
    }

    #[test]
    fn light_screenshot_only_transparent() {
        let mut s = img(1.0, 0.0);
        assert_eq!(adapt_to_paper(&mut s), Backdrop::Light);
        let mut s2 = img(1.0, 0.0);
        adapt_to_paper(&mut s2);
        assert_eq!(px(s, 0), (0, 255), "Schrift bleibt schwarz");
        assert_eq!(px(s2, 5).1, 0, "Weiß wird durchsichtig");
    }

    #[test]
    fn dark_screenshot_transparent_and_inverted() {
        let mut s = img(0.1, 1.0);
        assert_eq!(adapt_to_paper(&mut s), Backdrop::Dark);
        let mut s2 = img(0.1, 1.0);
        adapt_to_paper(&mut s2);
        assert_eq!(px(s, 0), (0, 255), "weiße Schrift wird schwarz");
        assert_eq!(px(s2, 5).1, 0, "Dunkler Hintergrund wird durchsichtig");
    }

    /// Weiß mit leicht grauen Kantenpixeln (Kantenglättung): Mittelwert läge unter
    /// reinem Weiß, trotzdem muss das reine Weiß verschwinden.
    #[test]
    fn white_with_grayish_pixels_transparent() {
        let mut s = img(1.0, 0.0);
        {
            let cr = Context::new(&s).unwrap();
            cr.set_source_rgb(0.93, 0.93, 0.93);
            cr.rectangle(2.0, 2.0, 3.0, 3.0);
            cr.fill().unwrap();
        }
        assert_eq!(adapt_to_paper(&mut s), Backdrop::Light);
        let d = s.take_data().unwrap();
        let transp = d.chunks_exact(4).filter(|p| p[3] == 0).count();
        assert!(transp >= 90, "nur {transp} von 100 Pixeln durchsichtig");
    }

    #[test]
    fn colorful_image_untouched() {
        let mut s = ImageSurface::create(Format::ARgb32, 10, 10).unwrap();
        {
            let cr = Context::new(&s).unwrap();
            cr.set_source_rgb(0.2, 0.6, 0.3);
            cr.paint().unwrap();
        }
        assert_eq!(adapt_to_paper(&mut s), Backdrop::Other);
        assert_eq!(px(s, 3).1, 255);
    }
}
