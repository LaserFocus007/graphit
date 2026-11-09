//! Text einer PDF-Seite mit Zeichenpositionen: für Marker, die an Textzeilen
//! einrasten, und für die Suche.

use std::path::Path;

use glib::translate::ToGlibPtr;
use gtk::glib;

use crate::geom::{Pt, Rect};

pub struct PageText {
    pub chars: Vec<char>,
    /// Rechteck je Zeichen (PDF-Punkte, Ursprung oben links)
    pub rects: Vec<Rect>,
}

pub fn load(pdf: &Path, idx: usize) -> Option<PageText> {
    crate::render::with_pdf(pdf, |d| {
        let page = d.page(idx as i32)?;
        let text = page.text()?.to_string();
        let mut ptr: *mut poppler::ffi::PopplerRectangle = std::ptr::null_mut();
        let mut n: u32 = 0;
        let ok = unsafe {
            let p: *mut poppler::ffi::PopplerPage = page.to_glib_none().0;
            poppler::ffi::poppler_page_get_text_layout(p, &mut ptr, &mut n)
        };
        if ok == 0 || ptr.is_null() {
            return None;
        }
        let rects: Vec<Rect> = unsafe {
            let s = std::slice::from_raw_parts(ptr, n as usize);
            let v = s.iter().map(|r| Rect::new(r.x1.min(r.x2), r.y1.min(r.y2), r.x1.max(r.x2), r.y1.max(r.y2))).collect();
            glib::ffi::g_free(ptr as *mut _);
            v
        };
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len().min(rects.len());
        Some(PageText { chars: chars[..n].to_vec(), rects: rects[..n].to_vec() })
    })
    .flatten()
}

impl PageText {
    fn is_glyph(&self, i: usize) -> bool {
        !self.chars[i].is_whitespace() && self.rects[i].width() > 0.01
    }

    /// Index des Zeichens, das am nächsten an `p` liegt (bevorzugt in derselben Zeile).
    pub fn nearest(&self, p: Pt) -> Option<(usize, f64)> {
        (0..self.chars.len())
            .filter(|&i| self.is_glyph(i))
            .map(|i| {
                let r = self.rects[i];
                let dx = if p.x < r.x0 { r.x0 - p.x } else if p.x > r.x1 { p.x - r.x1 } else { 0.0 };
                let dy = if p.y < r.y0 { r.y0 - p.y } else if p.y > r.y1 { p.y - r.y1 } else { 0.0 };
                // senkrechte Abweichung zählt stärker: Zeilen sollen nicht springen
                (i, dx + dy * 3.0)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Rechtecke pro Textzeile für den Zeichenbereich [a, b].
    pub fn line_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b), a.max(b));
        let mut out: Vec<Rect> = Vec::new();
        let mut cur: Option<Rect> = None;
        for i in a..=b.min(self.chars.len().saturating_sub(1)) {
            if !self.is_glyph(i) {
                continue;
            }
            let r = self.rects[i];
            match cur {
                Some(c) => {
                    let same_line = (r.center().y - c.center().y).abs() < c.height().max(r.height()) * 0.5 && r.x0 >= c.x0 - 2.0;
                    if same_line {
                        cur = Some(Rect::new(c.x0, c.y0.min(r.y0), c.x1.max(r.x1), c.y1.max(r.y1)));
                    } else {
                        out.push(c);
                        cur = Some(r);
                    }
                }
                None => cur = Some(r),
            }
        }
        out.extend(cur);
        out
    }

    /// Erste Fundstelle ohne Rücksicht auf Leerraum und Zeilenumbrüche.
    pub fn find_loose(&self, query: &str) -> Option<Vec<Rect>> {
        let squeeze = |c: &char| !c.is_whitespace();
        let q: Vec<char> = query.to_lowercase().chars().filter(squeeze).collect();
        if q.is_empty() {
            return None;
        }
        let hay: Vec<(usize, char)> = self
            .chars
            .iter()
            .enumerate()
            .filter(|(_, c)| squeeze(c))
            .map(|(i, c)| (i, c.to_lowercase().next().unwrap_or(*c)))
            .collect();
        hay.windows(q.len()).find(|w| w.iter().map(|e| e.1).eq(q.iter().copied())).map(|w| self.line_rects(w[0].0, w[q.len() - 1].0))
    }

    /// Alle Fundstellen (ohne Groß-/Kleinschreibung), je als Zeilen-Rechtecke.
    pub fn find(&self, query: &str) -> Vec<Vec<Rect>> {
        let q: Vec<char> = query.to_lowercase().chars().collect();
        if q.is_empty() {
            return Vec::new();
        }
        let hay: Vec<char> = self.chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i + q.len() <= hay.len() {
            if hay[i..i + q.len()] == q[..] {
                out.push(self.line_rects(i, i + q.len() - 1));
                i += q.len();
            } else {
                i += 1;
            }
        }
        out
    }
}
