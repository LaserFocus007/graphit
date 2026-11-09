//! Seitenverwaltung (einfügen, löschen, verschieben, Hintergrund) und Vorschaubilder.

use std::rc::Rc;

use super::*;

/// Breite der Vorschaubilder in Geräte-Pixeln.
pub const THUMB_W: f64 = 150.0;

impl Canvas {
    fn pages_change(&self, f: impl FnOnce(&mut Vec<PageRef>)) {
        self.commit_edits();
        let mut st = self.st();
        let old = st.doc.pages.clone();
        let mut new = old.clone();
        f(&mut new);
        if new.is_empty() {
            return;
        }
        st.doc.pages = new.clone();
        st.history.push(vec![Change::Pages { old, new }]);
        st.relayout();
    }

    /// Neue leere Seite nach `idx`. `style` = Papiermuster (graph, lined, dotted, plain).
    pub fn insert_page(&self, idx: usize, style: Option<&str>) {
        let page = self.new_page_after(idx, style);
        self.pages_change(|v| v.insert((idx + 1).min(v.len()), page));
        self.st().scroll_to_page(idx + 1);
        self.changed();
    }

    /// Leere Seite, die zur Seite `idx` passt; noch nicht im Dokument.
    pub fn blank_page_after(&self, idx: usize) -> PageRef {
        self.new_page_after(idx, None)
    }

    fn new_page_after(&self, idx: usize, style: Option<&str>) -> PageRef {
        {
            let st = self.st();
            let default = st.cfg.paper_style.clone();
            let src = st.doc.pages.get(idx).or(st.doc.pages.last()).map(|p| p.borrow());
            let (w, h, bg) = match &src {
                Some(p) => {
                    let style = style.map(|s| s.to_string()).unwrap_or_else(|| {
                        if p.bg.is_pdf() { default.clone() } else { p.bg.style().to_string() }
                    });
                    (p.width, if p.bg.is_pdf() { 841.89f64.max(p.height - p.bg.pdf_gap_total()) } else { p.height }, Background::solid(&style))
                }
                None => (595.276, 841.89, Background::solid(style.unwrap_or(&default))),
            };
            Page::new(w, h, bg).into_ref()
        }
    }

    pub fn delete_page(&self, idx: usize) {
        if self.st().doc.pages.len() <= 1 {
            return;
        }
        self.pages_change(|v| {
            if idx < v.len() {
                v.remove(idx);
            }
        });
        self.changed();
    }

    pub fn duplicate_page(&self, idx: usize) {
        let Some(copy) = self.st().doc.pages.get(idx).map(|p| p.borrow().duplicate().into_ref()) else { return };
        self.pages_change(|v| v.insert(idx + 1, copy));
        self.changed();
    }

    pub fn move_page(&self, idx: usize, to: usize) {
        let n = self.st().doc.pages.len();
        if idx >= n || to >= n || idx == to {
            return;
        }
        self.pages_change(|v| {
            let p = v.remove(idx);
            v.insert(to, p);
        });
        self.st().scroll_to_page(to);
        self.changed();
    }

    /// Papiermuster einer (Nicht-PDF-)Seite ändern.
    pub fn set_page_style(&self, idx: usize, style: &str) {
        self.commit_edits();
        let mut st = self.st();
        let Some(page) = st.doc.pages.get(idx).cloned() else { return };
        let (w, h, old) = {
            let p = page.borrow();
            (p.width, p.height, p.bg.clone())
        };
        if old.is_pdf() {
            return;
        }
        let mut new = old.clone();
        set_attr(&mut new.attrs, "style", style);
        {
            let mut p = page.borrow_mut();
            p.bg = new.clone();
            p.bg_ver = next_version();
            p.mark_ink(None);
        }
        st.history.push(vec![Change::Page { page, old: (w, h, old), new: (w, h, new) }]);
        drop(st);
        self.changed();
    }

    pub fn scroll_to(&self, idx: usize) {
        self.st().scroll_to_page(idx);
        self.changed();
    }

    /// Nach dem ersten Strich auf der letzten Seite eine leere Seite anhängen
    /// (wie "emptyLastPageAppend" in Xournal++).
    pub(super) fn maybe_append_page(&self, page: &PageRef) {
        let last = {
            let st = self.st();
            st.cfg.append_page && st.doc.pages.last().map(|p| Rc::ptr_eq(p, page)).unwrap_or(false)
        };
        if !last {
            return;
        }
        let n = self.st().doc.pages.len();
        // nicht als eigener Undo-Schritt: an den letzten Schritt anhängen
        let page = {
            let st = self.st();
            let p = st.doc.pages[n - 1].borrow();
            let style = if p.bg.is_pdf() { st.cfg.paper_style.clone() } else { p.bg.style().to_string() };
            let h = if p.bg.is_pdf() { p.height - p.bg.pdf_gap_total() } else { p.height };
            Page::new(p.width, h, Background::solid(&style)).into_ref()
        };
        let mut st = self.st();
        let old = st.doc.pages.clone();
        let mut new = old.clone();
        new.push(page);
        st.doc.pages = new.clone();
        st.history.append_to_last(Change::Pages { old, new });
        st.relayout();
    }

    // ---------------------------------------------------------------- Vorschaubilder

    pub fn connect_thumbs(&self, f: impl Fn(&Canvas) + 'static) {
        self.imp().thumb_listeners.borrow_mut().push(Box::new(f));
    }

    pub(super) fn thumbs_updated(&self) {
        for l in self.imp().thumb_listeners.borrow().iter() {
            l(self);
        }
    }

    /// Vorschaubild einer Seite (falls schon gerendert; sonst wird es angefordert).
    pub fn thumbnail(&self, idx: usize) -> Option<gdk::MemoryTexture> {
        let mut st = self.st();
        let page = st.doc.pages.get(idx)?.clone();
        let p = page.borrow();
        let level = tiles::level_of(THUMB_W / p.width);
        st.thumbs_wanted.insert(p.id);
        st.cache
            .tiles
            .iter()
            .filter(|(k, _)| k.page == p.id && k.kind == Kind::Thumb && k.level == level)
            .map(|(_, t)| t.tex.clone())
            .next()
    }
}

impl State {
    /// Jobs für fehlende/veraltete Vorschaubilder (niedrige Priorität).
    pub(super) fn thumb_jobs(&mut self, jobs: &mut Vec<(f64, Job)>) {
        if self.thumbs_wanted.is_empty() {
            return;
        }
        let frame = self.cache.frame;
        for (i, page) in self.doc.pages.iter().enumerate() {
            let p = page.borrow();
            if !self.thumbs_wanted.contains(&p.id) {
                continue;
            }
            let level = tiles::level_of(THUMB_W / p.width);
            let key = Key { page: p.id, kind: Kind::Thumb, level, tx: 0, ty: 0 };
            let ver = p.ink_ver.max(p.bg_ver);
            let fresh = match self.cache.tiles.get_mut(&key) {
                Some(t) => {
                    t.used = frame;
                    t.ver >= ver
                }
                None => false,
            };
            if !fresh {
                let data = p.snapshot(self.doc.page_pdf(&p));
                jobs.push((2e6 + i as f64, Job { key, ver, data }));
            }
        }
    }
}
