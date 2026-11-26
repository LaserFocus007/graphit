//! PDF-Text: Marker, der an Textzeilen einrastet, und Volltextsuche.

use super::*;
use crate::pdftext::PageText;

impl State {
    /// Text der PDF-Seite hinter Seite `i` (gecacht).
    pub(super) fn page_text(&mut self, i: usize) -> Option<Arc<PageText>> {
        let pdf = self.doc.pdf_path.clone()?;
        let idx = {
            let p = self.doc.pages.get(i)?.borrow();
            if !p.bg.is_pdf() {
                return None;
            }
            p.bg.pdf_index()
        };
        self.text_cache.entry(idx).or_insert_with(|| crate::pdftext::load(&pdf, idx).map(Arc::new)).clone()
    }

    /// Verknüpfungen der PDF-Seite hinter Seite `i` (gecacht).
    fn page_links(&mut self, i: usize) -> Option<Arc<Vec<(Rect, crate::pdftext::LinkTarget)>>> {
        let pdf = self.doc.pdf_path.clone()?;
        let idx = {
            let p = self.doc.pages.get(i)?.borrow();
            if !p.bg.is_pdf() {
                return None;
            }
            p.bg.pdf_index()
        };
        Some(self.link_cache.entry(idx).or_insert_with(|| Arc::new(crate::pdftext::load_links(&pdf, idx))).clone())
    }

    /// Verknüpfung des PDFs unter dem Bildschirmpunkt.
    pub(super) fn link_at(&mut self, x: f64, y: f64) -> Option<crate::pdftext::LinkTarget> {
        let (i, p) = self.page_at(x, y, 0.0)?;
        let links = self.page_links(i)?;
        let q = Pt::new(p.x, self.doc.pages[i].borrow().bg.display_to_pdf(p.y));
        links.iter().find(|(r, _)| r.expand(1.0).contains(q)).map(|(_, t)| t.clone())
    }

    /// PDF-Zeichen direkt unter dem Bildschirmpunkt: Seite, Text und Zeichenindex.
    pub(super) fn text_at(&mut self, x: f64, y: f64) -> Option<(usize, Arc<PageText>, usize)> {
        let (i, p) = self.page_at(x, y, 0.0)?;
        let text = self.page_text(i)?;
        let q = Pt::new(p.x, self.doc.pages[i].borrow().bg.display_to_pdf(p.y));
        let (k, d) = text.nearest(q)?;
        // auch die Lücke zwischen Wörtern und Kapitälchen zählen noch als Text
        (d < 5.0).then_some((i, text, k))
    }

    /// Ausgewählter Text auf dem Bildschirm, als Rechteck um alle Zeilen.
    fn text_sel_screen_rect(&self) -> Option<Rect> {
        let t = self.text_sel.as_ref()?;
        let i = self.doc.index_of(&t.page)?;
        let o = self.page_origin(i);
        let bg = t.page.borrow().bg.clone();
        let r = t.text.line_rects(t.ia, t.ib).iter().map(|r| to_display(&bg, r)).reduce(|a, b| a.union(&b))?;
        let (z, cam) = (self.zoom, self.cam);
        Some(Rect::new((o.x + r.x0 - cam.x) * z, (o.y + r.y0 - cam.y) * z, (o.x + r.x1 - cam.x) * z, (o.y + r.y1 - cam.y) * z))
    }
}

impl Canvas {
    /// Einer Verknüpfung des PDFs folgen: Sprung im Dokument oder Webadresse im Browser.
    pub(super) fn follow_link(&self, target: crate::pdftext::LinkTarget) {
        use crate::pdftext::LinkTarget;
        match target {
            LinkTarget::Uri(uri) => {
                // nur Web und Mail, nichts, was ein Programm starten könnte
                if ["http://", "https://", "mailto:"].iter().any(|s| uri.starts_with(s)) {
                    let _ = gtk::gio::AppInfo::launch_default_for_uri(&uri, gtk::gio::AppLaunchContext::NONE);
                }
            }
            LinkTarget::Page { page, y } => {
                {
                    let mut st = self.st();
                    let Some(i) = st.doc.pages.iter().position(|p| {
                        let b = p.borrow();
                        b.bg.is_pdf() && b.bg.pdf_index() == page
                    }) else {
                        return;
                    };
                    let from = st.cam.y;
                    st.link_back.push(from);
                    if st.link_back.len() > 50 {
                        st.link_back.remove(0);
                    }
                    let top = st.tops[i];
                    st.cam.y = match y {
                        // Ziel etwas unter der Oberkante, damit der Kontext davor sichtbar bleibt
                        Some(y) => top + st.doc.pages[i].borrow().bg.pdf_to_display(y) - (st.view.1 / st.zoom) * 0.2,
                        None => top - GAP / 2.0,
                    };
                    st.text_sel = None;
                }
                self.changed();
            }
        }
    }

    /// Zurück an die Stelle vor dem letzten Sprung (Alt+Links wie im PDF-Reader).
    pub fn link_back(&self) -> bool {
        let y = self.st().link_back.pop();
        match y {
            Some(y) => {
                self.st().cam.y = y;
                self.changed();
                true
            }
            None => false,
        }
    }

    /// Ausgewählter PDF-Text als Zeichenkette.
    pub fn text_selection(&self) -> Option<String> {
        let st = self.st();
        let t = st.text_sel.as_ref()?;
        let (a, b) = (t.ia.min(t.ib), t.ia.max(t.ib).min(t.text.chars.len().saturating_sub(1)));
        let s: String = t.text.chars[a..=b].iter().collect();
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// Ausgewählten Text mit dem Marker hervorheben.
    pub fn highlight_text_selection(&self) {
        let t = self.st().text_sel.take();
        if let Some(t) = t {
            self.finish_text_mark(t.page, t.text, t.ia, t.ib);
        }
        self.changed();
    }

    pub fn clear_text_selection(&self) {
        if self.st().text_sel.take().is_some() {
            self.changed();
        }
    }

    /// Wo die Leiste zur Textauswahl hingehört (Bildschirmkoordinaten des Canvas).
    pub fn text_selection_rect(&self) -> Option<Rect> {
        self.st().text_sel_screen_rect()
    }
}

/// PDF-Rechteck → angezeigte Seitenkoordinaten (berücksichtigt eingefügte Lücken).
pub(super) fn to_display(bg: &Background, r: &Rect) -> Rect {
    let dy = bg.pdf_to_display(r.center().y) - r.center().y;
    r.translate(0.0, dy)
}

impl Canvas {
    /// Startet den Text-Marker, wenn am Punkt PDF-Text liegt.
    pub(super) fn try_text_mark(&self, st: &mut State, i: usize, p: Pt) -> bool {
        if !st.tools.hl_snap {
            return false;
        }
        let Some(text) = st.page_text(i) else { return false };
        let page = st.doc.pages[i].clone();
        let q = Pt::new(p.x, page.borrow().bg.display_to_pdf(p.y));
        match text.nearest(q) {
            Some((ia, d)) if d < 6.0 => {
                let origin = st.page_origin(i);
                st.action = Some(Action::TextMark { page, origin, text, ia, ib: ia });
                true
            }
            _ => false,
        }
    }

    pub(super) fn text_mark_motion(st: &mut State, s: &Sample) {
        let zoom = st.zoom;
        let cam = st.cam;
        if let Some(Action::TextMark { page, origin, text, ib, .. } | Action::TextSelect { page, origin, text, ib, .. }) = &mut st.action {
            let p = Pt::new(cam.x + s.x / zoom - origin.x, cam.y + s.y / zoom - origin.y);
            let q = Pt::new(p.x, page.borrow().bg.display_to_pdf(p.y));
            if let Some((i, _)) = text.nearest(q) {
                *ib = i;
            }
        }
    }

    /// Markierte Zeilen als ganz normale Xournal-Markerstriche anlegen.
    pub(super) fn finish_text_mark(&self, page: PageRef, text: Arc<PageText>, ia: usize, ib: usize) {
        let (color, rects) = {
            let st = self.st();
            let bg = page.borrow().bg.clone();
            (st.tools.highlighter.color, text.line_rects(ia, ib).iter().map(|r| to_display(&bg, r)).collect::<Vec<_>>())
        };
        let strokes: Vec<Item> = rects
            .iter()
            .map(|r| {
                let y = r.center().y;
                let w = r.height() * 1.05;
                Item::Stroke(Stroke::new(
                    StrokeTool::Highlighter,
                    color,
                    w,
                    vec![Pt::new(r.x0, y), Pt::new(r.x1, y)],
                    None,
                    vec![("capStyle".into(), "butt".into())],
                ))
            })
            .collect();
        if !strokes.is_empty() {
            self.add_items(&page, strokes);
        }
    }

    /// Volltextsuche in PDF-Text und Textobjekten. Liefert die Anzahl Treffer.
    pub fn search(&self, query: &str) -> usize {
        let q = query.trim().to_lowercase();
        {
            let mut st = self.st();
            st.search.clear();
            st.search_idx = 0;
            if q.is_empty() {
                drop(st);
                self.queue_draw();
                return 0;
            }
            for i in 0..st.doc.pages.len() {
                let (id, bg, texts): (u64, Background, Vec<Rect>) = {
                    let p = st.doc.pages[i].borrow();
                    let texts = p
                        .layers
                        .iter()
                        .flat_map(|l| l.items.iter())
                        .filter_map(|it| match &**it {
                            Item::Text(t) if t.text.to_lowercase().contains(&q) => Some(t.bbox()),
                            _ => None,
                        })
                        .collect();
                    (p.id, p.bg.clone(), texts)
                };
                if let Some(text) = st.page_text(i) {
                    for m in text.find(&q) {
                        st.search.push((id, m.iter().map(|r| to_display(&bg, r)).collect()));
                    }
                }
                for r in texts {
                    st.search.push((id, vec![r]));
                }
            }
        }
        let n = self.st().search.len();
        if n > 0 {
            self.search_goto(0);
        }
        self.queue_draw();
        n
    }

    /// Zum nächsten/vorigen Treffer springen. Liefert (Position, Anzahl).
    pub fn search_step(&self, d: i32) -> (usize, usize) {
        let n = self.st().search.len();
        if n == 0 {
            return (0, 0);
        }
        let i = (self.st().search_idx as i64 + d as i64).rem_euclid(n as i64) as usize;
        self.search_goto(i);
        (i + 1, n)
    }

    pub fn search_pos(&self) -> (usize, usize) {
        let st = self.st();
        (if st.search.is_empty() { 0 } else { st.search_idx + 1 }, st.search.len())
    }

    fn search_goto(&self, i: usize) {
        {
            let mut st = self.st();
            st.search_idx = i;
            let (id, r) = match st.search.get(i) {
                Some((id, rs)) => (*id, rs.iter().copied().reduce(|a, b| a.union(&b))),
                None => return,
            };
            let Some(pi) = st.doc.pages.iter().position(|p| p.borrow().id == id) else { return };
            let Some(r) = r else { return };
            let o = st.page_origin(pi);
            let vis = st.view_layout();
            let lr = r.translate(o.x, o.y);
            if !vis.expand(-20.0 / st.zoom).contains(lr.center()) {
                st.cam.y = lr.y0 - (vis.height() * 0.35);
                if lr.x0 < vis.x0 || lr.x1 > vis.x1 {
                    st.cam.x = lr.center().x - vis.width() / 2.0;
                }
            }
        }
        self.changed();
    }

    pub fn clear_search(&self) {
        self.st().search.clear();
        self.queue_draw();
    }

    pub(super) fn draw_search_and_mark(&self, st: &State, snap: &gtk::Snapshot) {
        let z = st.zoom;
        let vis = st.view_layout();
        let rect = |o: Pt, r: &Rect| {
            graphene::Rect::new(((o.x + r.x0 - st.cam.x) * z) as f32, ((o.y + r.y0 - st.cam.y) * z) as f32, (r.width() * z) as f32, (r.height() * z) as f32)
        };
        if !st.search.is_empty() {
            for (k, (id, rs)) in st.search.iter().enumerate() {
                let Some(pi) = st.doc.pages.iter().position(|p| p.borrow().id == *id) else { continue };
                if !st.page_layout_rect(pi).intersects(&vis) {
                    continue;
                }
                let o = st.page_origin(pi);
                let c = if k == st.search_idx { gdk::RGBA::new(1.0, 0.55, 0.0, 0.45) } else { gdk::RGBA::new(1.0, 0.9, 0.0, 0.35) };
                for r in rs {
                    snap.append_color(&c, &rect(o, &r.expand(1.0)));
                }
            }
        }
        // Textauswahl wie in einem PDF-Reader: blau hinterlegt
        let sel = match &st.action {
            Some(Action::TextSelect { page, origin, text, ia, ib }) => Some((page.clone(), *origin, text.clone(), *ia, *ib)),
            _ => st.text_sel.as_ref().and_then(|t| Some((t.page.clone(), st.page_origin(st.doc.index_of(&t.page)?), t.text.clone(), t.ia, t.ib))),
        };
        if let Some((page, origin, text, ia, ib)) = sel {
            let accent = self.color_lookup("accent_color").unwrap_or(gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
            let c = gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.28);
            let bg = page.borrow().bg.clone();
            for r in text.line_rects(ia, ib) {
                let r = to_display(&bg, &r).expand(0.6);
                snap.append_color(&c, &rect(origin, &r));
            }
        }
        if let Some(Action::TextMark { page, origin, text, ia, ib }) = &st.action {
            let mut c = st.tools.highlighter.color.to_gdk();
            c.set_alpha(c.alpha().min(0.5));
            let bg = page.borrow().bg.clone();
            for r in text.line_rects(*ia, *ib) {
                let r = to_display(&bg, &r);
                let h = r.height() * 1.05;
                let rr = Rect::new(r.x0, r.center().y - h / 2.0, r.x1, r.center().y + h / 2.0);
                snap.append_color(&c, &rect(*origin, &rr));
            }
        }
    }
}
