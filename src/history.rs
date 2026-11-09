//! Undo/Redo. Da Elemente unveränderlich sind, speichert ein Schritt nur die
//! alten und neuen Listen-Referenzen - billig, egal wie groß das Dokument ist.

use crate::model::{Background, Document, ItemList, PageRef};

#[derive(Clone)]
pub enum Change {
    Items { page: PageRef, layer: usize, old: ItemList, new: ItemList },
    Pages { old: Vec<PageRef>, new: Vec<PageRef> },
    Page { page: PageRef, old: (f64, f64, Background), new: (f64, f64, Background) },
}

impl Change {
    fn apply(&self, doc: &mut Document, forward: bool) {
        match self {
            Change::Items { page, layer, old, new } => {
                let list = if forward { new } else { old };
                let mut p = page.borrow_mut();
                if *layer < p.layers.len() {
                    p.set_items(*layer, list.clone());
                }
            }
            Change::Pages { old, new } => {
                doc.pages = if forward { new.clone() } else { old.clone() };
            }
            Change::Page { page, old, new } => {
                let (w, h, bg) = if forward { new } else { old };
                let mut p = page.borrow_mut();
                p.width = *w;
                p.height = *h;
                p.bg = bg.clone();
                p.bg_ver = crate::model::next_version();
                p.mark_ink(None);
            }
        }
    }
}

#[derive(Default)]
pub struct History {
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    /// Zählt alle Änderungen; `saved_at` merkt sich den Stand beim Speichern.
    pub serial: u64,
    pub saved_at: u64,
    next_id: u64,
    pos: Vec<u64>,
    redo_pos: Vec<u64>,
}

const MAX_STEPS: usize = 500;

impl History {
    pub fn push(&mut self, changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        self.next_id += 1;
        self.serial = self.next_id;
        self.undo.push(changes);
        self.pos.push(self.serial);
        self.redo.clear();
        self.redo_pos.clear();
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
            self.pos.remove(0);
        }
    }

    /// Hängt eine Änderung an den letzten Schritt an (z. B. automatisch angehängte Seite).
    pub fn append_to_last(&mut self, c: Change) {
        match self.undo.last_mut() {
            Some(step) => step.push(c),
            None => self.push(vec![c]),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Liefert die betroffenen Seiten (für die Neuzeichnung).
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(step) = self.undo.pop() else { return false };
        let p = self.pos.pop().unwrap_or(0);
        for c in step.iter().rev() {
            c.apply(doc, false);
        }
        self.redo.push(step);
        self.redo_pos.push(p);
        self.serial = self.pos.last().copied().unwrap_or(0);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(step) = self.redo.pop() else { return false };
        let p = self.redo_pos.pop().unwrap_or(0);
        for c in step.iter() {
            c.apply(doc, true);
        }
        self.undo.push(step);
        self.pos.push(p);
        self.serial = p;
        true
    }

    pub fn dirty(&self) -> bool {
        self.serial != self.saved_at
    }

    pub fn mark_saved(&mut self) {
        self.saved_at = self.serial;
    }
}
