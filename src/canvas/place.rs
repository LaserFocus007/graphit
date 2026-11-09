//! Zugriffe für Claudes Einfügen in die Notiz (siehe `crate::place`).

use std::rc::Rc;
use std::sync::Arc;

use super::*;
use crate::history::Change;

impl Canvas {
    /// Testhilfe: kreist die Mitte der Ansicht mit dem Frage-Werkzeug ein.
    pub fn test_circle(&self) {
        self.set_tool(Tool::Ask);
        let (w, h) = (self.width() as f64, self.height() as f64);
        let at = |x: f64, y: f64| Sample { x: w * x, y: h * y, pressure: None, tilt: 0.0 };
        self.begin(Tool::Ask, at(0.2, 0.42), false);
        for (x, y) in [(0.45, 0.4), (0.5, 0.55), (0.3, 0.62), (0.18, 0.5), (0.2, 0.43)] {
            self.motion(&[at(x, y)]);
        }
        self.end(None);
    }

    /// Eingekreiste Stelle als Bild an das Fenster geben, danach zurück zum vorigen Werkzeug.
    pub(super) fn finish_ask(&self, page: PageRef, pts: Vec<Pt>) {
        let back = self.st().before_ask.take().unwrap_or(Tool::Pen);
        self.set_tool(back);
        let Some(bb) = Rect::bounding(&pts) else { return };
        // ein Tipp ist keine Auswahl
        if bb.width().max(bb.height()) < 12.0 {
            return;
        }
        let (index, data) = {
            let st = self.st();
            let Some(i) = st.doc.pages.iter().position(|p| Rc::ptr_eq(p, &page)) else { return };
            let p = page.borrow();
            (i, p.snapshot(st.doc.page_pdf(&p)))
        };
        let area = Rect::new(bb.x0 - 6.0, bb.y0 - 6.0, bb.x1 + 6.0, bb.y1 + 6.0);
        let Some(png) = crate::place::region_png(&data, area) else { return };
        let (w, h) = (data.width.max(1.0), data.height.max(1.0));
        self.request(Request::ClaudeRegion(png, index, [(area.x0 / w).max(0.0), (area.y0 / h).max(0.0), (area.x1 / w).min(1.0), (area.y1 / h).min(1.0)]));
    }

    /// Groben Bereich am gedruckten oder geschriebenen Inhalt einrasten; Seitenkoordinaten.
    pub fn snap_terms(&self, page: usize, areas: &[Rect]) -> Vec<Rect> {
        match self.page_data(page) {
            Some(d) => crate::place::snap_to_content(&d, areas),
            None => areas.to_vec(),
        }
    }

    /// Zu einer Seite blättern.
    pub fn show_page(&self, page: usize) {
        self.st().scroll_to_page(page);
        self.changed();
    }

    pub fn history_serial(&self) -> u64 {
        self.st().history.serial
    }

    pub fn page_ref(&self, page: usize) -> Option<PageRef> {
        self.st().doc.pages.get(page).cloned()
    }

    pub fn page_data(&self, page: usize) -> Option<Arc<PageData>> {
        let st = self.st();
        let p = st.doc.pages.get(page)?.borrow();
        Some(p.snapshot(st.doc.page_pdf(&p)))
    }

    /// Fügt alles in einem einzigen Undo-Schritt ein, wahlweise mit neuer Seite nach `blank.0`.
    pub fn place_batch(&self, blank: Option<(usize, PageRef)>, batch: Vec<(PageRef, Vec<Item>)>, show: usize) -> u64 {
        self.commit_edits();
        let serial = {
            let mut st = self.st();
            let mut changes = Vec::new();
            if let Some((idx, page)) = blank {
                let old = st.doc.pages.clone();
                let mut new = old.clone();
                new.insert((idx + 1).min(new.len()), page);
                st.doc.pages = new.clone();
                changes.push(Change::Pages { old, new });
                st.relayout();
            }
            for (page, items) in batch {
                let layer = page.borrow().current_layer();
                let old = page.borrow().layers[layer].items.clone();
                let mut list: Vec<ItemRef> = (*old).clone();
                list.extend(items.into_iter().map(Arc::new));
                let new: ItemList = Arc::new(list);
                page.borrow_mut().set_items(layer, new.clone());
                changes.push(Change::Items { page, layer, old, new });
            }
            st.history.push(changes);
            if st.current_page() != show {
                st.scroll_to_page(show);
            }
            st.history.serial
        };
        self.changed();
        serial
    }
}
