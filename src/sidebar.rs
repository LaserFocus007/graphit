//! Seitenleiste mit Vorschaubildern und Seitenverwaltung.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::canvas::{Canvas, THUMB_W};

pub struct Sidebar {
    pub root: gtk::Box,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    canvas: RefCell<Option<Canvas>>,
    rows: RefCell<Vec<(u64, gtk::Picture)>>,
    connected: RefCell<HashSet<usize>>,
    syncing: Cell<bool>,
    last_current: Cell<usize>,
}

use crate::settings::PAPERS;

impl Sidebar {
    pub fn new() -> Rc<Sidebar> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("fk-sidebar");
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        root.append(&scroller);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        bar.add_css_class("toolbar");
        bar.set_halign(gtk::Align::Center);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&bar);

        let sb = Rc::new(Sidebar {
            root,
            list,
            scroller,
            canvas: RefCell::new(None),
            rows: RefCell::new(Vec::new()),
            connected: RefCell::new(HashSet::new()),
            syncing: Cell::new(false),
            last_current: Cell::new(usize::MAX),
        });

        // Seite einfügen (mit Papierwahl)
        let add = gtk::MenuButton::new();
        add.set_icon_name("list-add-symbolic");
        add.set_tooltip_text(Some(crate::i18n::tl("Seite einfügen")));
        add.add_css_class("flat");
        let pop = gtk::Popover::new();
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 2);
        bx.add_css_class("fk-options");
        for (label, style) in [(crate::i18n::tl("Wie aktuelle Seite"), "")].into_iter().chain(PAPERS) {
            let label = crate::i18n::tl(label);
            let b = gtk::Button::with_label(label);
            b.add_css_class("flat");
            let w = Rc::downgrade(&sb);
            let p = pop.clone();
            b.connect_clicked(move |_| {
                p.popdown();
                if let Some(s) = w.upgrade() {
                    if let Some(c) = s.canvas() {
                        let i = s.selected();
                        c.insert_page(i, if style.is_empty() { None } else { Some(style) });
                    }
                }
            });
            bx.append(&b);
        }
        pop.set_child(Some(&bx));
        add.set_popover(Some(&pop));
        bar.append(&add);

        let btn = |icon: &str, tip: &str, f: Box<dyn Fn(&Canvas, usize)>| {
            let b = gtk::Button::from_icon_name(icon);
            b.add_css_class("flat");
            b.set_tooltip_text(Some(tip));
            let w = Rc::downgrade(&sb);
            b.connect_clicked(move |_| {
                if let Some(s) = w.upgrade() {
                    if let Some(c) = s.canvas() {
                        f(&c, s.selected());
                    }
                }
            });
            bar.append(&b);
        };
        btn("fk-pages-symbolic", crate::i18n::tl("Seite duplizieren"), Box::new(|c, i| c.duplicate_page(i)));
        btn("go-up-symbolic", crate::i18n::tl("Nach oben"), Box::new(|c, i| c.move_page(i, i.saturating_sub(1))));
        btn("go-down-symbolic", crate::i18n::tl("Nach unten"), Box::new(|c, i| c.move_page(i, i + 1)));
        btn("user-trash-symbolic", crate::i18n::tl("Seite löschen"), Box::new(|c, i| c.delete_page(i)));

        // Papier der gewählten Seite ändern
        let paper = gtk::MenuButton::new();
        paper.set_icon_name("fk-shape-rect-symbolic");
        paper.set_tooltip_text(Some(crate::i18n::tl("Papier dieser Seite")));
        paper.add_css_class("flat");
        let menu = gio::Menu::new();
        let group = gio::SimpleActionGroup::new();
        for (label, style) in PAPERS {
            let label = crate::i18n::tl(label);
            menu.append(Some(label), Some(&format!("sb.paper-{style}")));
            let a = gio::SimpleAction::new(&format!("paper-{style}"), None);
            let w = Rc::downgrade(&sb);
            a.connect_activate(move |_, _| {
                if let Some(s) = w.upgrade() {
                    if let Some(c) = s.canvas() {
                        c.set_page_style(s.selected(), style);
                    }
                }
            });
            group.add_action(&a);
        }
        sb.root.insert_action_group("sb", Some(&group));
        paper.set_menu_model(Some(&menu));
        bar.append(&paper);

        let w = Rc::downgrade(&sb);
        sb.list.connect_row_selected(move |_, row| {
            let (Some(s), Some(row)) = (w.upgrade(), row) else { return };
            if s.syncing.get() {
                return;
            }
            if let Some(c) = s.canvas() {
                s.last_current.set(row.index() as usize);
                c.scroll_to(row.index() as usize);
            }
        });
        sb
    }

    fn canvas(&self) -> Option<Canvas> {
        self.canvas.borrow().clone()
    }

    fn selected(&self) -> usize {
        self.list.selected_row().map(|r| r.index().max(0) as usize).unwrap_or(0)
    }

    pub fn set_canvas(self: &Rc<Self>, c: &Canvas) {
        let key = c.as_ptr() as usize;
        if self.connected.borrow_mut().insert(key) {
            let w = Rc::downgrade(self);
            let cc = c.downgrade();
            c.connect_thumbs(move |_| {
                if let (Some(s), Some(c)) = (w.upgrade(), cc.upgrade()) {
                    if s.canvas().as_ref() == Some(&c) {
                        s.update_thumbs();
                    }
                }
            });
        }
        self.canvas.replace(Some(c.clone()));
        self.rows.borrow_mut().clear();
        self.last_current.set(usize::MAX);
        self.refresh();
    }

    /// Nach Änderungen: Liste neu aufbauen, falls sich Seiten geändert haben,
    /// und die aktuelle Seite markieren.
    pub fn refresh(&self) {
        let Some(c) = self.canvas() else { return };
        if !self.root.is_mapped() {
            return;
        }
        let (ids, sizes, cur): (Vec<u64>, Vec<(f64, f64)>, usize) = {
            let st = c.st();
            (
                st.doc.pages.iter().map(|p| p.borrow().id).collect(),
                st.doc.pages.iter().map(|p| { let p = p.borrow(); (p.width, p.height) }).collect(),
                st.current_page(),
            )
        };
        let old: Vec<u64> = self.rows.borrow().iter().map(|r| r.0).collect();
        if old != ids {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
            let mut rows = Vec::new();
            for (i, (id, (w, h))) in ids.iter().zip(sizes).enumerate() {
                let bx = gtk::Box::new(gtk::Orientation::Vertical, 4);
                bx.set_margin_top(6);
                bx.set_margin_bottom(6);
                let pic = gtk::Picture::new();
                pic.set_content_fit(gtk::ContentFit::Contain);
                pic.set_can_shrink(true);
                pic.set_size_request(THUMB_W as i32, (THUMB_W * h / w).min(400.0) as i32);
                pic.add_css_class("fk-thumb");
                bx.append(&pic);
                let l = gtk::Label::new(Some(&(i + 1).to_string()));
                l.add_css_class("caption");
                l.add_css_class("dim-label");
                bx.append(&l);
                self.list.append(&bx);
                rows.push((*id, pic));
            }
            *self.rows.borrow_mut() = rows;
            self.last_current.set(usize::MAX);
        }
        if cur != self.last_current.get() {
            self.last_current.set(cur);
            if let Some(row) = self.list.row_at_index(cur as i32) {
                self.syncing.set(true);
                self.list.select_row(Some(&row));
                self.syncing.set(false);
                // sichtbar halten
                let adj = self.scroller.vadjustment();
                if let Some(b) = row.compute_bounds(&self.list) {
                    let (y, h) = (b.y() as f64, b.height() as f64);
                    if y < adj.value() || y + h > adj.value() + adj.page_size() {
                        adj.set_value((y - adj.page_size() / 3.0).max(0.0));
                    }
                }
            }
        }
        self.update_thumbs();
    }

    fn update_thumbs(&self) {
        let Some(c) = self.canvas() else { return };
        let mut missing = false;
        for (i, (_, pic)) in self.rows.borrow().iter().enumerate() {
            match c.thumbnail(i) {
                Some(tex) => {
                    if pic.paintable().map(|p| p.as_ptr() as usize) != Some(tex.as_ptr() as usize) {
                        pic.set_paintable(Some(&tex));
                    }
                }
                None => missing = true,
            }
        }
        if missing {
            // Canvas reiht die Vorschau-Jobs beim nächsten Frame ein
            let c2 = c.clone();
            glib::idle_add_local_once(move || c2.queue_draw());
        }
    }
}
