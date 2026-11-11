//! Temporäre Zeichen von Claude: Kreis, Markierung, Unterstreichung oder Pfeil über
//! der Seite. Sie gehören nicht zum Dokument und werden nie gespeichert.

use std::f64::consts::TAU;
use std::rc::Rc;

use adw::prelude::AnimationExt;

use super::search::to_display;
use super::*;
use crate::geom::Pt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarkKind {
    Circle,
    Highlight,
    Underline,
    Arrow,
    /// Rechteck um die Stelle
    Frame,
    /// farbiger, enger Rahmen um einen Teil einer Formel
    Term,
    /// dunkelt alles andere ab
    Focus,
}

impl MarkKind {
    pub fn parse(s: &str) -> MarkKind {
        match s {
            "markierung" => MarkKind::Highlight,
            "unterstreichen" => MarkKind::Underline,
            "pfeil" => MarkKind::Arrow,
            "rahmen" => MarkKind::Frame,
            "fokus" => MarkKind::Focus,
            _ => MarkKind::Circle,
        }
    }
}

/// Ein Zeichen; `label` steht als kleines Schild daran.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub rects: Vec<Rect>,
    pub kind: MarkKind,
    pub label: Option<String>,
    /// eigene Farbe statt der Standardfarbe
    pub color: Option<gdk::RGBA>,
}

pub struct Guide {
    /// hervorgehobener Term (Index in `marks`), die anderen treten zurück
    pub emphasis: Option<usize>,
    pub page: usize,
    pub marks: Vec<Mark>,
    /// Verbindungen zwischen zwei Zeichen (Indizes in `marks`)
    pub links: Vec<(usize, usize)>,
    /// Skizze: Linienzüge in Seitenkoordinaten
    pub sketch: Vec<Vec<Pt>>,
    /// 0…1: wie weit alles schon gezeichnet ist
    pub progress: f64,
    /// Ziel der Ansicht, solange sie noch dorthin gleitet
    rest: Option<Pt>,
    /// Stirbt mit diesem Zeichen, damit eine alte Animation die Ansicht nicht mehr bewegt
    _alive: Rc<()>,
}

const INK: gdk::RGBA = gdk::RGBA::new(0.85, 0.42, 0.28, 1.0);

/// Anfang eines Linienzugs bis zum Anteil `t` seiner Länge.
fn partial(line: &[Pt], t: f64) -> Vec<Pt> {
    let total: f64 = line.windows(2).map(|w| w[0].dist(w[1])).sum();
    let mut left = total * t;
    let mut out = line.first().copied().into_iter().collect::<Vec<_>>();
    for w in line.windows(2) {
        let d = w[0].dist(w[1]);
        if d >= left {
            if d > 0.0 {
                out.push(w[0].lerp(w[1], left / d));
            }
            break;
        }
        out.push(w[1]);
        left -= d;
    }
    out
}

/// Striche zu Schreibzeilen zusammenfassen: gleiche Höhe, kleiner Abstand.
pub fn ink_lines(mut boxes: Vec<Rect>, page_w: f64) -> Vec<Rect> {
    boxes.sort_by(|a, b| a.center().y.total_cmp(&b.center().y));
    let gap = page_w * 0.06;
    let mut lines: Vec<Rect> = Vec::new();
    for b in boxes {
        let hit = lines.iter_mut().rev().take(12).find(|l| {
            let overlap = b.y1.min(l.y1) - b.y0.max(l.y0);
            overlap > b.height().min(l.height()) * 0.4 && b.x0 < l.x1 + gap && b.x1 > l.x0 - gap
        });
        match hit {
            Some(l) => *l = l.union(&b),
            None => lines.push(b),
        }
    }
    lines
}

/// Grob geschätzten Bereich an die Striche ziehen, die wirklich darin liegen.
pub fn snap_to_ink(area: Rect, ink: &[Rect], page_w: f64) -> Rect {
    let wide = area.expand(page_w * 0.025);
    ink.iter().filter(|b| wide.contains(b.center())).copied().reduce(|a, b| a.union(&b)).unwrap_or(area)
}

impl Canvas {
    /// Seitengröße (für Bereiche in Seitenanteilen).
    pub fn page_size(&self, page: usize) -> Option<(f64, f64)> {
        let st = self.st();
        let p = st.doc.pages.get(page)?.borrow();
        Some((p.width, p.height))
    }

    pub fn page_count(&self) -> usize {
        self.st().doc.pages.len()
    }

    /// Umrisse der Striche einer Seite.
    pub fn page_ink(&self, page: usize) -> Vec<Rect> {
        let st = self.st();
        let Some(p) = st.doc.pages.get(page) else { return Vec::new() };
        let p = p.borrow();
        p.layers.iter().flat_map(|l| l.items.iter()).filter(|i| matches!(&***i, Item::Stroke(_))).filter_map(|i| i.bbox()).collect()
    }

    /// Wörter einer Seite mit ihrer Lage: aus dem PDF und aus Textobjekten.
    pub fn page_words(&self, page: usize) -> Vec<(String, Rect)> {
        let mut st = self.st();
        let mut out: Vec<(String, Rect)> = Vec::new();
        if let (Some(t), Some(bg)) = (st.page_text(page), st.doc.pages.get(page).map(|p| p.borrow().bg.clone())) {
            let mut word: Option<(String, Rect)> = None;
            for (c, r) in t.chars.iter().zip(&t.rects) {
                let breaks = word.as_ref().is_some_and(|(_, w)| (r.center().y - w.center().y).abs() > w.height() * 0.7);
                if c.is_whitespace() || breaks {
                    out.extend(word.take());
                }
                if !c.is_whitespace() {
                    let w = word.get_or_insert_with(|| (String::new(), *r));
                    w.0.push(*c);
                    w.1 = w.1.union(r);
                }
            }
            out.extend(word);
            for w in &mut out {
                w.1 = to_display(&bg, &w.1);
            }
        }
        if let Some(p) = st.doc.pages.get(page) {
            let p = p.borrow();
            for it in p.layers.iter().flat_map(|l| l.items.iter()) {
                if let Item::Text(t) = &**it {
                    out.push((t.text.clone(), t.bbox()));
                }
            }
        }
        out
    }

    /// Wie `find_text`, aber nur auf dieser einen Seite.
    pub fn find_on_page(&self, query: &str, page: usize, near: Option<Rect>) -> Option<Vec<Rect>> {
        let mut st = self.st();
        Self::hits_on_page(&mut st, query, page, near)
    }

    /// Alle Fundstellen auf einer Seite, aus dem PDF und aus Textobjekten. Von mehreren gewinnt
    /// die nächste an `near`, ohne Angabe die erste: kurze Suchtexte wie "ki" kommen oft mehrfach vor.
    fn hits_on_page(st: &mut State, query: &str, page: usize, near: Option<Rect>) -> Option<Vec<Rect>> {
        let q = query.to_lowercase();
        let bg = st.doc.pages.get(page)?.borrow().bg.clone();
        let mut hits: Vec<Vec<Rect>> = st
            .page_text(page)
            .map(|t| t.find_loose_all(query))
            .unwrap_or_default()
            .into_iter()
            .map(|lines| lines.iter().map(|r| to_display(&bg, r)).collect())
            .collect();
        let p = st.doc.pages[page].borrow();
        hits.extend(p.layers.iter().flat_map(|l| l.items.iter()).filter_map(|it| match &**it {
            Item::Text(t) if t.text.to_lowercase().contains(&q) => Some(vec![t.bbox()]),
            _ => None,
        }));
        let Some(near) = near else { return hits.into_iter().next() };
        let centre = near.center();
        let dist = |lines: &Vec<Rect>| lines.iter().copied().reduce(|a, b| a.union(&b)).map_or(f64::INFINITY, |r| r.center().dist(centre));
        hits.into_iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))
    }

    /// Ein Stück Formel auf der Seite suchen (Seitenkoordinaten). Von mehreren Fundstellen gewinnt
    /// die, die zugleich nah an der Schätzung `near` und an den Zeichen des Schritts liegt. Weiter
    /// als `reach` (Anteil der Seitenhöhe) von der Schätzung zählt nicht: dann ist es eine andere Stelle.
    pub fn find_near(&self, glyphs: &str, page: usize, near: Rect, marks: &[Rect], reach: f64) -> Option<Rect> {
        let mut st = self.st();
        let bg = st.doc.pages.get(page)?.borrow().bg.clone();
        let height = st.doc.pages[page].borrow().height;
        let hits = st.page_text(page)?.find_glyphs(glyphs);
        let centre = near.center();
        let to_marks = |r: &Rect| marks.iter().map(|m| m.center().dist(r.center())).fold(0.0_f64, |a, d| if a == 0.0 { d } else { a.min(d) });
        hits.iter()
            .filter_map(|lines| lines.iter().copied().reduce(|a, b| a.union(&b)))
            .map(|r| to_display(&bg, &r))
            .filter(|r| r.center().dist(centre) < height * reach)
            .map(|r| (r, r.center().dist(centre) + to_marks(&r)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(r, _)| r)
    }

    /// Ein Stück Formel auf derselben Zeile wie `r` suchen, höchstens `gap` Punkte neben dessen Kante.
    pub fn find_beside(&self, glyphs: &str, page: usize, r: Rect, gap: f64) -> Option<Rect> {
        let mut st = self.st();
        let bg = st.doc.pages.get(page)?.borrow().bg.clone();
        let hits = st.page_text(page)?.find_glyphs(glyphs);
        let apart = |h: &Rect| (h.x0 - r.x1).max(r.x0 - h.x1).max(0.0);
        hits.iter()
            .filter_map(|lines| lines.iter().copied().reduce(|a, b| a.union(&b)))
            .map(|h| to_display(&bg, &h))
            .filter(|h| (h.center().y - r.center().y).abs() < r.height().max(h.height()) && apart(h) <= gap)
            .min_by(|a, b| apart(a).total_cmp(&apart(b)))
    }

    /// Text im PDF oder in Textobjekten suchen, zuerst auf Seite `hint`, dann auf den
    /// übrigen. Auf `hint` gewinnt die Fundstelle am nächsten an `near`. Liefert Seite und
    /// Rechtecke je Zeile.
    pub fn find_text(&self, query: &str, hint: usize, near: Option<Rect>) -> Option<(usize, Vec<Rect>)> {
        let mut st = self.st();
        let n = st.doc.pages.len();
        let hint = hint.min(n.saturating_sub(1));
        for i in std::iter::once(hint).chain((0..n).filter(|i| *i != hint)) {
            if let Some(hit) = Self::hits_on_page(&mut st, query, i, near.filter(|_| i == hint)) {
                return Some((i, hit));
            }
        }
        None
    }

    /// Zeichen zeigen: Ansicht gleitet zur Stelle, die Zeichen zeichnen sich selbst.
    pub fn show_guide(&self, page: usize, marks: Vec<Mark>, links: Vec<(usize, usize)>, sketch: Vec<Vec<Pt>>) {
        let (from, to, alive) = {
            let mut st = self.st();
            let Some((pw, ph)) = st.doc.pages.get(page).map(|p| (p.borrow().width, p.borrow().height)) else { return };
            let o = st.page_origin(page);
            let vis = st.view_layout();
            let mut to = st.cam;
            match guide_area(&marks, &sketch) {
                Some(a) => {
                    let l = a.translate(o.x, o.y);
                    if l.height() <= vis.height() * 0.9 {
                        let cut_off = l.y0 < vis.y0 || l.y1 > vis.y1;
                        // Ganz unten bliebe der Karte nur Platz über dem Text: kleine Stellen kommen in die obere Hälfte
                        let low = l.height() < vis.height() * 0.5 && l.center().y > vis.y0 + vis.height() * 0.65;
                        if !vis.expand(-40.0 / st.zoom).contains(l.center()) || cut_off || low {
                            to.y = l.center().y - vis.height() * 0.4;
                        }
                    } else if l.y0 < vis.y0 || l.y0 > vis.y0 + vis.height() * 0.4 {
                        // Passt nicht alles ins Fenster, kommt der Anfang ins Bild: dort steht meist die Formel
                        to.y = l.y0 - vis.height() * 0.12;
                    }
                    if l.x0 < vis.x0 || l.x1 > vis.x1 {
                        to.x = l.center().x - vis.width() / 2.0;
                    }
                }
                // ohne Zeichen gibt es nichts anzusteuern: nur blättern, wenn die Seite kaum zu sehen ist
                None => {
                    let seen = (o.y + ph).min(vis.y1) - o.y.max(vis.y0);
                    if seen < 0.25 * vis.height().min(ph) {
                        to.y = o.y - 16.0 / st.zoom;
                        if o.x < vis.x0 || o.x + pw > vis.x1 {
                            to.x = o.x + pw / 2.0 - vis.width() / 2.0;
                        }
                    }
                }
            }
            let alive = Rc::new(());
            st.guide = Some(Guide { emphasis: None, page, marks, links, sketch, progress: 0.0, rest: Some(to), _alive: alive.clone() });
            (st.cam, to, Rc::downgrade(&alive))
        };
        let w = self.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            let Some(c) = w.upgrade().filter(|_| alive.strong_count() > 0) else { return };
            {
                let mut guard = c.st();
                let st = &mut *guard;
                let Some(g) = &mut st.guide else { return };
                g.progress = ((v - 0.3) / 0.7).clamp(0.0, 1.0);
                if g.rest.is_some() {
                    let t = (v / 0.45).min(1.0);
                    st.cam = Pt::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                    if t >= 1.0 {
                        g.rest = None;
                    }
                }
            }
            c.changed();
        });
        let long = self.st().guide.as_ref().is_some_and(|g| !g.sketch.is_empty());
        let a = adw::TimedAnimation::new(self, 0.0, 1.0, if long { 1600 } else { 640 }, target);
        a.set_easing(adw::Easing::EaseInOutCubic);
        a.play();
    }

    /// Einen Term hervorheben.
    pub fn emphasize_mark(&self, mark: Option<usize>) {
        if let Some(g) = &mut self.st().guide {
            g.emphasis = mark;
        }
        self.queue_draw();
    }

    pub fn clear_guide(&self) {
        self.st().guide = None;
        self.queue_draw();
    }

    /// Skizze der Führung als echte Striche in die Notiz übernehmen.
    pub fn take_sketch(&self) -> bool {
        let (page, strokes) = {
            let st = self.st();
            let Some(g) = &st.guide else { return false };
            let color = st.tools.pen.color;
            let strokes: Vec<Item> = g
                .sketch
                .iter()
                .filter(|l| l.len() >= 2)
                .map(|l| Item::Stroke(Stroke::new(StrokeTool::Pen, color, 1.41, l.clone(), None, Vec::new())))
                .collect();
            (st.doc.pages[g.page].clone(), strokes)
        };
        let any = !strokes.is_empty();
        if any {
            self.add_items(&page, strokes);
            if let Some(g) = &mut self.st().guide {
                g.sketch.clear();
            }
            self.changed();
        }
        any
    }

    /// Zeichen einzeln in Seitenkoordinaten (mit Schild oder ohne) und Punkte auf ihren Verbindungen.
    pub fn guide_shapes(&self) -> (Vec<(Rect, bool)>, Vec<Pt>) {
        let st = self.st();
        let Some(g) = &st.guide else { return (Vec::new(), Vec::new()) };
        let marks = g.marks.iter().flat_map(|m| m.rects.iter().map(|r| (*r, m.label.is_some())));
        let sketch = g.sketch.iter().filter_map(|l| Rect::bounding(l)).map(|r| (r, false));
        let shapes = marks.chain(sketch).collect();
        let whole = |i: usize| g.marks.get(i).and_then(|m| m.rects.iter().copied().reduce(|a, b| a.union(&b)));
        let links = g
            .links
            .iter()
            .filter_map(|(a, b)| Some((whole(*a)?.center(), whole(*b)?.center())))
            .flat_map(|(a, b)| (1..12).map(move |k| a.lerp(b, k as f64 / 12.0)))
            .collect();
        (shapes, links)
    }

    /// Um wie viele Pixel die Zeichen noch rücken, bis die Ansicht angekommen ist.
    pub fn guide_rest_shift(&self) -> (f64, f64) {
        let st = self.st();
        st.guide.as_ref().and_then(|g| g.rest).map_or((0.0, 0.0), |r| ((st.cam.x - r.x) * st.zoom, (st.cam.y - r.y) * st.zoom))
    }

    /// Steht die Ansicht schon an der Stelle der Zeichen?
    pub fn guide_settled(&self) -> bool {
        self.st().guide.as_ref().is_none_or(|g| g.rest.is_none())
    }

    /// Ansicht als Ursprung in Layout-Koordinaten und Zoom.
    pub fn camera(&self) -> (f64, f64, f64) {
        let st = self.st();
        (st.cam.x, st.cam.y, st.zoom)
    }

    /// Rechteck der Zeichen im Widget, auch wenn es gerade außerhalb liegt.
    pub fn guide_screen_rect(&self) -> Option<Rect> {
        let st = self.st();
        let g = st.guide.as_ref()?;
        let o = st.page_origin(g.page);
        let r = guide_area(&g.marks, &g.sketch)?;
        let z = st.zoom;
        Some(Rect::new((o.x + r.x0 - st.cam.x) * z, (o.y + r.y0 - st.cam.y) * z, (o.x + r.x1 - st.cam.x) * z, (o.y + r.y1 - st.cam.y) * z))
    }

    /// Seite und Rechteck der Zeichen in Seitenkoordinaten.
    pub fn guide_page_area(&self) -> Option<(usize, Rect)> {
        let st = self.st();
        let g = st.guide.as_ref()?;
        Some((g.page, guide_area(&g.marks, &g.sketch)?))
    }

    /// Rechteck einer Seite in Widget-Koordinaten.
    pub fn page_rect_on_screen(&self, page: usize, r: Rect) -> Rect {
        let st = self.st();
        let (o, z) = (st.page_origin(page), st.zoom);
        Rect::new((o.x + r.x0 - st.cam.x) * z, (o.y + r.y0 - st.cam.y) * z, (o.x + r.x1 - st.cam.x) * z, (o.y + r.y1 - st.cam.y) * z)
    }

    pub(super) fn draw_guide(&self, st: &State, snap: &gtk::Snapshot) {
        let Some(g) = &st.guide else { return };
        let p = g.progress;
        if p <= 0.0 {
            return;
        }
        let (z, cam, o) = (st.zoom, st.cam, st.page_origin(g.page));
        let at = |q: Pt| Pt::new((o.x + q.x - cam.x) * z, (o.y + q.y - cam.y) * z);
        let scr = |r: &Rect| {
            let (a, b) = (at(Pt::new(r.x0, r.y0)), at(Pt::new(r.x1, r.y1)));
            Rect::new(a.x, a.y, b.x, b.y)
        };
        let pen = gsk::Stroke::new(3.0);
        pen.set_line_cap(gsk::LineCap::Round);
        pen.set_line_join(gsk::LineJoin::Round);
        // Fokus zuerst: die anderen Zeichen liegen darüber
        for m in g.marks.iter().filter(|m| m.kind == MarkKind::Focus) {
            let Some(r) = m.rects.iter().map(scr).reduce(|a, b| a.union(&b)) else { continue };
            let r = r.expand(14.0);
            let (w, h) = (self.width() as f32, self.height() as f32);
            let c = gdk::RGBA::new(0.0, 0.0, 0.0, (0.5 * p) as f32);
            let (x0, y0, x1, y1) = (r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32);
            for b in [
                graphene::Rect::new(0.0, 0.0, w, y0.max(0.0)),
                graphene::Rect::new(0.0, y1, w, (h - y1).max(0.0)),
                graphene::Rect::new(0.0, y0, x0.max(0.0), y1 - y0),
                graphene::Rect::new(x1, y0, (w - x1).max(0.0), y1 - y0),
            ] {
                snap.append_color(&c, &b);
            }
        }
        let boxes: Vec<Option<Rect>> = g.marks.iter().map(|m| m.rects.iter().map(scr).reduce(|a, b| a.union(&b))).collect();
        for (k, (m, all)) in g.marks.iter().zip(&boxes).enumerate() {
            let Some(all) = all else { continue };
            if m.kind == MarkKind::Term {
                let faded = g.emphasis.is_some_and(|e| e != k);
                let chosen = g.emphasis == Some(k);
                let c = m.color.unwrap_or(INK);
                let c = gdk::RGBA::new(c.red(), c.green(), c.blue(), if faded { 0.28 } else { 1.0 });
                let r = all.expand(2.0);
                let bounds = graphene::Rect::new(r.x0 as f32, r.y0 as f32, r.width() as f32, r.height() as f32);
                let round = gsk::RoundedRect::from_rect(bounds, 3.0);
                if !faded {
                    let tint = gdk::RGBA::new(c.red(), c.green(), c.blue(), if chosen { 0.26 } else { 0.12 });
                    snap.push_rounded_clip(&round);
                    snap.append_color(&tint, &bounds);
                    snap.pop();
                }
                let width = if chosen { 2.6 } else { 1.8 } as f32;
                snap.append_border(&round, &[width; 4], &[c; 4]);
                continue;
            }
            draw_mark(snap, m, *all, &m.rects.iter().map(scr).collect::<Vec<_>>(), p, &pen);
        }
        for (a, b) in &g.links {
            if let (Some(Some(a)), Some(Some(b))) = (boxes.get(*a), boxes.get(*b)) {
                draw_link(snap, *a, *b, p);
            }
        }
        for line in &g.sketch {
            let pts: Vec<Pt> = partial(line, p).into_iter().map(at).collect();
            let Some(first) = pts.first() else { continue };
            let b = gsk::PathBuilder::new();
            b.move_to(first.x as f32, first.y as f32);
            for q in &pts[1..] {
                b.line_to(q.x as f32, q.y as f32);
            }
            snap.append_stroke(&b.to_path(), &pen, &INK);
        }
        if p >= 0.8 {
            for (k, (m, all)) in g.marks.iter().zip(&boxes).enumerate() {
                // bei Termen nur das Schild des gewählten, sonst überdecken sie die Formel
                if m.kind == MarkKind::Term && g.emphasis != Some(k) {
                    continue;
                }
                if let (Some(t), Some(r)) = (&m.label, all) {
                    self.draw_label(snap, t, *r);
                }
            }
        }
    }

    /// Kleines Schild über der Stelle.
    fn draw_label(&self, snap: &gtk::Snapshot, text: &str, r: Rect) {
        let layout = self.create_pango_layout(Some(text));
        let mut font = gtk::pango::FontDescription::new();
        font.set_size(10 * gtk::pango::SCALE);
        font.set_weight(gtk::pango::Weight::Bold);
        layout.set_font_description(Some(&font));
        let (w, h) = layout.pixel_size();
        let (x, y) = ((r.x0 as f32).max(4.0), (r.y0 as f32 - h as f32 - 16.0).max(4.0));
        let bg = graphene::Rect::new(x, y, w as f32 + 14.0, h as f32 + 6.0);
        snap.push_rounded_clip(&gsk::RoundedRect::from_rect(bg, 8.0));
        snap.append_color(&INK, &bg);
        snap.pop();
        snap.save();
        snap.translate(&graphene::Point::new(x + 7.0, y + 3.0));
        snap.append_layout(&layout, &gdk::RGBA::WHITE);
        snap.restore();
    }
}

fn guide_area(marks: &[Mark], sketch: &[Vec<Pt>]) -> Option<Rect> {
    let m = marks.iter().flat_map(|m| m.rects.iter().copied());
    let s = sketch.iter().filter_map(|l| Rect::bounding(l));
    m.chain(s).reduce(|a, b| a.union(&b))
}

fn draw_mark(snap: &gtk::Snapshot, m: &Mark, all: Rect, rects: &[Rect], p: f64, pen: &gsk::Stroke) {
    match m.kind {
        MarkKind::Focus | MarkKind::Term => {}
        MarkKind::Highlight => {
            let c = gdk::RGBA::new(1.0, 0.72, 0.25, 0.42);
            for r in rects {
                let bar = graphene::Rect::new(r.x0 as f32 - 3.0, r.y0 as f32 - 2.0, ((r.width() + 6.0) * p) as f32, r.height() as f32 + 4.0);
                snap.append_color(&c, &bar);
            }
        }
        MarkKind::Underline => {
            for r in rects {
                let b = gsk::PathBuilder::new();
                let n = 24;
                for k in 0..=n {
                    let t = k as f64 / n as f64 * p;
                    let (x, y) = (r.x0 + r.width() * t, r.y1 + 3.0 + (t * 11.0).sin() * 1.2);
                    if k == 0 { b.move_to(x as f32, y as f32) } else { b.line_to(x as f32, y as f32) }
                }
                snap.append_stroke(&b.to_path(), pen, &INK);
            }
        }
        MarkKind::Frame => {
            let r = all.expand(7.0);
            let line = [Pt::new(r.x0, r.y0), Pt::new(r.x1, r.y0), Pt::new(r.x1, r.y1), Pt::new(r.x0, r.y1), Pt::new(r.x0, r.y0)];
            let pts = partial(&line, p);
            let b = gsk::PathBuilder::new();
            b.move_to(pts[0].x as f32, pts[0].y as f32);
            for q in &pts[1..] {
                b.line_to(q.x as f32, q.y as f32);
            }
            snap.append_stroke(&b.to_path(), pen, &INK);
        }
        MarkKind::Circle => {
            // von Hand gezogen: leicht unrund, das Ende läuft über den Anfang hinaus
            let c = all.center();
            let (rx, ry) = (all.width() / 2.0 + 12.0, all.height() / 2.0 + 10.0);
            let b = gsk::PathBuilder::new();
            let n = 72;
            for k in 0..=n {
                let f = k as f64 / n as f64 * p;
                let t = -2.2 + f * TAU * 1.07;
                let w = 1.0 + 0.035 * (t * 2.0 + 0.6).sin() + 0.05 * f;
                let (x, y) = (c.x + rx * w * t.cos(), c.y + ry * w * t.sin());
                if k == 0 { b.move_to(x as f32, y as f32) } else { b.line_to(x as f32, y as f32) }
            }
            snap.append_stroke(&b.to_path(), pen, &INK);
        }
        MarkKind::Arrow => {
            // kommt von links oben, liegt die Stelle weit links von rechts oben
            let from_left = all.x0 > 90.0;
            let tip = Pt::new(if from_left { all.x0 - 8.0 } else { all.x1 + 8.0 }, all.center().y);
            let s = if from_left { -1.0 } else { 1.0 };
            let tail = Pt::new(tip.x + s * 70.0, tip.y - 46.0);
            draw_curve(snap, tail, Pt::new(tip.x + s * 56.0, tip.y - 6.0), tip, p, pen);
        }
    }
}

/// Gebogene Linie mit Pfeilspitze, wächst mit `p`.
fn draw_curve(snap: &gtk::Snapshot, from: Pt, ctrl: Pt, to: Pt, p: f64, pen: &gsk::Stroke) {
    let at = |t: f64| {
        let u = 1.0 - t;
        Pt::new(u * u * from.x + 2.0 * u * t * ctrl.x + t * t * to.x, u * u * from.y + 2.0 * u * t * ctrl.y + t * t * to.y)
    };
    let b = gsk::PathBuilder::new();
    b.move_to(from.x as f32, from.y as f32);
    for k in 1..=28 {
        let q = at(k as f64 / 28.0 * p);
        b.line_to(q.x as f32, q.y as f32);
    }
    if p > 0.92 {
        let e = at(0.9);
        let a = (to.y - e.y).atan2(to.x - e.x);
        for d in [2.6f64, -2.6] {
            b.move_to(to.x as f32, to.y as f32);
            b.line_to((to.x + 12.0 * (a + d).cos()) as f32, (to.y + 12.0 * (a + d).sin()) as f32);
        }
    }
    snap.append_stroke(&b.to_path(), pen, &INK);
}

/// Verbindet zwei Stellen: von Kante zu Kante, leicht gebogen.
fn draw_link(snap: &gtk::Snapshot, a: Rect, b: Rect, p: f64) {
    let (ca, cb) = (a.center(), b.center());
    let edge = |r: Rect, toward: Pt| {
        let c = r.center();
        let (dx, dy) = (toward.x - c.x, toward.y - c.y);
        let k = ((r.width() / 2.0 + 10.0) / dx.abs().max(1e-6)).min((r.height() / 2.0 + 10.0) / dy.abs().max(1e-6));
        Pt::new(c.x + dx * k, c.y + dy * k)
    };
    let (from, to) = (edge(a, cb), edge(b, ca));
    let mid = from.lerp(to, 0.5);
    let bend = from.dist(to) * 0.18;
    let n = (to.y - from.y, from.x - to.x);
    let len = n.0.hypot(n.1).max(1.0);
    let ctrl = Pt::new(mid.x + n.0 / len * bend, mid.y + n.1 / len * bend);
    let pen = gsk::Stroke::new(2.4);
    pen.set_line_cap(gsk::LineCap::Round);
    pen.set_dash(&[7.0, 6.0]);
    draw_curve(snap, from, ctrl, to, p, &pen);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sketch_grows_along_its_length() {
        let line = [Pt::new(0.0, 0.0), Pt::new(10.0, 0.0), Pt::new(10.0, 10.0)];
        assert_eq!(partial(&line, 1.0).last(), Some(&Pt::new(10.0, 10.0)));
        assert_eq!(partial(&line, 0.25).last(), Some(&Pt::new(5.0, 0.0)));
        assert_eq!(partial(&line, 0.75).len(), 3);
    }

    #[test]
    fn strokes_become_lines() {
        let boxes = vec![
            Rect::new(10.0, 100.0, 30.0, 120.0),
            Rect::new(35.0, 102.0, 60.0, 118.0),
            Rect::new(400.0, 101.0, 440.0, 119.0), // gleiche Höhe, aber weit weg
            Rect::new(10.0, 200.0, 50.0, 220.0),
        ];
        let l = ink_lines(boxes, 600.0);
        assert_eq!(l.len(), 3);
        assert!(l.iter().any(|r| r.x0 == 10.0 && r.x1 == 60.0 && r.y0 == 100.0));
    }

    #[test]
    fn estimate_snaps_to_ink() {
        let ink = [Rect::new(100.0, 100.0, 200.0, 120.0), Rect::new(100.0, 400.0, 200.0, 420.0)];
        // Schätzung liegt etwas daneben: trifft trotzdem nur die obere Zeile
        let r = snap_to_ink(Rect::new(120.0, 118.0, 210.0, 135.0), &ink, 600.0);
        assert_eq!(r, ink[0]);
        // kein Strich in der Nähe: Schätzung bleibt
        let far = Rect::new(400.0, 600.0, 500.0, 650.0);
        assert_eq!(snap_to_ink(far, &ink, 600.0), far);
    }
}
