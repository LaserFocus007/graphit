//! Eine Dokumentansicht: Zeichenfläche + Scrollbalken + Text-Editor + Auswahlleiste.
//! Jeder Tab und der Lese-Viewer ist eine DocView.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::canvas::Canvas;
use crate::tools::{COLOR_NAMES, PALETTE, QUICK_COLORS};

pub struct DocView {
    pub root: gtk::Overlay,
    pub canvas: Canvas,
    /// öffnet den Claude-Dialog (vom Fenster gesetzt)
    pub on_claude: std::cell::RefCell<Option<Rc<dyn Fn()>>>,
    text_view: gtk::TextView,
    text_css: gtk::CssProvider,
    sel_bar: gtk::Box,
    /// Autosave: zuletzt gesehener und zuletzt gesicherter Änderungsstand
    pub autosave_serial: Cell<u64>,
    pub autosaved_serial: Cell<u64>,
    pub autosaving: Cell<bool>,
    toast: Rc<dyn Fn(&str)>,
    search_rev: gtk::Revealer,
    search_entry: gtk::SearchEntry,
    search_count: gtk::Label,
}

pub fn rgb_css(c: &crate::geom::Color) -> String {
    format!("rgb({},{},{})", (c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8)
}

/// Bildlaufleiste nur am Schieber ziehbar: Tippen oder Klicken daneben (z. B. mit dem
/// Handballen) springt nicht mehr an eine andere Stelle.
fn drag_only(bar: &gtk::Scrollbar) {
    fn slider(w: &gtk::Widget) -> Option<gtk::Widget> {
        let mut c = w.first_child();
        while let Some(ch) = c {
            if ch.css_name() == "slider" {
                return Some(ch);
            }
            if let Some(s) = slider(&ch) {
                return Some(s);
            }
            c = ch.next_sibling();
        }
        None
    }
    // etwas Spielraum längs der Leiste, damit der Schieber mit dem Finger gut zu treffen ist
    const SLOP: f32 = 12.0;
    let vertical = bar.orientation() == gtk::Orientation::Vertical;
    let g = gtk::GestureClick::new();
    g.set_button(0);
    g.set_propagation_phase(gtk::PropagationPhase::Capture);
    g.connect_pressed(move |g, _, x, y| {
        let Some(bar) = g.widget() else { return };
        let on_slider = slider(&bar).and_then(|s| s.compute_bounds(&bar)).is_some_and(|b| {
            let (p, lo, hi) = if vertical { (y as f32, b.y(), b.y() + b.height()) } else { (x as f32, b.x(), b.x() + b.width()) };
            p >= lo - SLOP && p <= hi + SLOP
        });
        if !on_slider {
            // beansprucht → die Leiste selbst bekommt den Klick nicht
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    bar.add_controller(g);
}

impl DocView {
    pub fn new(readonly: bool, toast: Rc<dyn Fn(&str)>) -> Rc<DocView> {
        let canvas = Canvas::new();
        canvas.st().readonly = readonly;
        let root = gtk::Overlay::new();
        root.set_child(Some(&canvas));
        let vbar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&canvas.vadjustment()));
        vbar.set_halign(gtk::Align::End);
        vbar.add_css_class("overlay-indicator");
        drag_only(&vbar);
        root.add_overlay(&vbar);
        let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&canvas.hadjustment()));
        hbar.set_valign(gtk::Align::End);
        hbar.add_css_class("overlay-indicator");
        drag_only(&hbar);
        root.add_overlay(&hbar);

        let text_view = gtk::TextView::new();
        text_view.add_css_class("fk-textedit");
        text_view.set_halign(gtk::Align::Start);
        text_view.set_valign(gtk::Align::Start);
        text_view.set_visible(false);
        text_view.set_size_request(60, -1);
        let text_css = gtk::CssProvider::new();
        #[allow(deprecated)]
        text_view.style_context().add_provider(&text_css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        root.add_overlay(&text_view);

        let sel_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        sel_bar.add_css_class("fk-selbar");
        sel_bar.set_halign(gtk::Align::Start);
        sel_bar.set_valign(gtk::Align::Start);
        sel_bar.set_visible(false);
        root.add_overlay(&sel_bar);

        // Suchleiste (Strg+F)
        let search_rev = gtk::Revealer::new();
        search_rev.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        search_rev.set_halign(gtk::Align::End);
        search_rev.set_valign(gtk::Align::Start);
        search_rev.set_margin_end(18);
        search_rev.set_margin_top(10);
        let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        sbox.add_css_class("fk-selbar");
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Im Dokument suchen"));
        search_entry.set_width_chars(22);
        let search_count = gtk::Label::new(None);
        search_count.add_css_class("dim-label");
        search_count.add_css_class("numeric");
        search_count.set_width_chars(6);
        let prev = gtk::Button::from_icon_name("go-up-symbolic");
        prev.add_css_class("flat");
        prev.set_tooltip_text(Some("Vorheriger Treffer (Umschalt+Enter)"));
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.add_css_class("flat");
        next.set_tooltip_text(Some("Nächster Treffer (Enter)"));
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        sbox.append(&search_entry);
        sbox.append(&search_count);
        sbox.append(&prev);
        sbox.append(&next);
        sbox.append(&close);
        search_rev.set_child(Some(&sbox));
        root.add_overlay(&search_rev);

        let v = Rc::new(DocView {
            root,
            canvas,
            on_claude: std::cell::RefCell::new(None),
            text_view,
            text_css,
            sel_bar,
            autosave_serial: Cell::new(0),
            autosaved_serial: Cell::new(0),
            autosaving: Cell::new(false),
            toast,
            search_rev,
            search_entry,
            search_count,
        });
        v.build_sel_bar();
        v.setup_text();
        let w = Rc::downgrade(&v);
        v.search_entry.connect_search_changed(move |e| {
            if let Some(v) = w.upgrade() {
                let n = v.canvas.search(&e.text());
                v.update_count(n > 0 || e.text().is_empty());
            }
        });
        let w = Rc::downgrade(&v);
        v.search_entry.connect_activate(move |_| {
            if let Some(v) = w.upgrade() {
                v.canvas.search_step(1);
                v.update_count(true);
            }
        });
        let w = Rc::downgrade(&v);
        v.search_entry.connect_stop_search(move |_| {
            if let Some(v) = w.upgrade() {
                v.hide_search();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(&v);
        keys.connect_key_pressed(move |_, key, _, mods| {
            if key == gdk::Key::Return && mods.contains(gdk::ModifierType::SHIFT_MASK) {
                if let Some(v) = w.upgrade() {
                    v.canvas.search_step(-1);
                    v.update_count(true);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        v.search_entry.add_controller(keys);
        for (b, d) in [(prev, -1), (next, 1)] {
            let w = Rc::downgrade(&v);
            b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.canvas.search_step(d);
                    v.update_count(true);
                }
            });
        }
        let w = Rc::downgrade(&v);
        close.connect_clicked(move |_| {
            if let Some(v) = w.upgrade() {
                v.hide_search();
            }
        });
        let w = Rc::downgrade(&v);
        v.canvas.connect_changed(move |_| {
            if let Some(v) = w.upgrade() {
                v.refresh_overlays();
            }
        });
        v
    }

    pub fn show_search(&self) {
        self.search_rev.set_reveal_child(true);
        self.search_entry.grab_focus();
        self.search_entry.select_region(0, -1);
    }

    pub fn hide_search(&self) {
        self.search_rev.set_reveal_child(false);
        self.search_entry.set_text("");
        self.canvas.clear_search();
        self.search_count.set_text("");
        self.canvas.grab_focus();
    }

    fn update_count(&self, found: bool) {
        let (i, n) = self.canvas.search_pos();
        self.search_count.set_text(&if n > 0 { format!("{i}/{n}") } else if found { String::new() } else { "keine".into() });
        if found { self.search_entry.remove_css_class("error") } else { self.search_entry.add_css_class("error") }
    }

    /// Titel für Tab/Fenster (mit • bei ungespeicherten Änderungen).
    pub fn title(&self) -> (String, bool) {
        let st = self.canvas.st();
        let name = st
            .doc
            .path
            .as_ref()
            .or(st.doc.pdf_path.as_ref())
            .and_then(|p| p.file_stem())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unbenannt".into());
        (name, st.history.dirty())
    }

    /// Leeres, unverändertes Dokument (kann beim Öffnen ersetzt werden)?
    pub fn is_pristine(&self) -> bool {
        let st = self.canvas.st();
        st.doc.path.is_none()
            && st.doc.pdf_path.is_none()
            && !st.history.dirty()
            && !st.history.can_undo()
            && st.doc.pages.len() == 1
            && st.doc.pages[0].borrow().layers.iter().all(|l| l.items.is_empty())
    }

    fn build_sel_bar(self: &Rc<Self>) {
        let add = |icon: &str, tip: &str, f: Box<dyn Fn(&Rc<DocView>)>| {
            let b = gtk::Button::from_icon_name(icon);
            b.add_css_class("flat");
            b.set_tooltip_text(Some(tip));
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    f(&v);
                }
            });
            self.sel_bar.append(&b);
        };
        add("user-trash-symbolic", "Löschen (Entf)", Box::new(|v| v.canvas.delete_selection()));
        add("edit-cut-symbolic", "Ausschneiden (Strg+X)", Box::new(|v| v.copy(true)));
        add("edit-copy-symbolic", "Kopieren (Strg+C)", Box::new(|v| v.copy(false)));
        add("fk-pages-symbolic", "Duplizieren (Strg+D)", Box::new(|v| v.canvas.duplicate_selection()));
        add(
            "fk-spark-symbolic",
            "Claude dazu fragen: die Auswahl geht als Bild mit",
            Box::new(|v| {
                let f = v.on_claude.borrow().clone();
                if let Some(f) = f {
                    f();
                }
            }),
        );
        add(
            "changes-prevent-symbolic",
            "Fixieren: danach nicht mehr auswählbar oder radierbar (lösen über Menü)",
            Box::new(|v| v.canvas.lock_selection()),
        );
        self.sel_bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        for i in QUICK_COLORS {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.add_css_class("fk-color");
            b.set_tooltip_text(Some(COLOR_NAMES[i]));
            let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            sw.add_css_class("fk-swatch");
            sw.add_css_class(&format!("fk-c{i}"));
            sw.set_halign(gtk::Align::Center);
            sw.set_valign(gtk::Align::Center);
            b.set_child(Some(&sw));
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.canvas.recolor_selection(PALETTE[i]);
                }
            });
            self.sel_bar.append(&b);
        }
    }

    fn setup_text(self: &Rc<Self>) {
        let tv = self.text_view.clone();
        self.canvas.set_text_source(move || {
            let b = tv.buffer();
            b.text(&b.start_iter(), &b.end_iter(), false).to_string()
        });
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(v) = w.upgrade() {
                    v.canvas.commit_text();
                    v.canvas.grab_focus();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.text_view.add_controller(keys);
    }

    /// Text-Editor und Auswahlleiste an die aktuelle Position im Canvas setzen.
    pub fn refresh_overlays(&self) {
        match self.canvas.text_edit_info() {
            Some((x, y, px, color, font, initial)) => {
                let tv = &self.text_view;
                if !tv.is_visible() {
                    tv.buffer().set_text(&initial);
                    tv.set_visible(true);
                    tv.grab_focus();
                }
                tv.set_margin_start(x.max(0.0) as i32);
                tv.set_margin_top(y.max(0.0) as i32);
                self.text_css.load_from_string(&format!(
                    "textview.fk-textedit, textview.fk-textedit text {{ font-family: \"{}\"; font-size: {:.1}px; color: {}; }}",
                    font.replace('"', ""),
                    px.max(4.0),
                    rgb_css(&color)
                ));
            }
            None => {
                if self.text_view.is_visible() {
                    self.text_view.set_visible(false);
                    self.text_view.buffer().set_text("");
                }
            }
        }
        match self.canvas.selection_screen_rect() {
            Some(r) => {
                self.sel_bar.set_visible(true);
                let (bw, _, _, _) = self.sel_bar.measure(gtk::Orientation::Horizontal, -1);
                // measure() enthält die Ränder - die setzen wir selbst
                let bw = (bw - self.sel_bar.margin_start() - self.sel_bar.margin_end()) as f64;
                let x = ((r.x0 + r.x1) / 2.0 - bw / 2.0).clamp(8.0, (self.canvas.width() as f64 - bw - 8.0).max(8.0));
                let y = if r.y0 > 56.0 { r.y0 - 50.0 } else { r.y1 + 14.0 };
                self.sel_bar.set_margin_start(x as i32);
                self.sel_bar.set_margin_top(y.clamp(4.0, (self.canvas.height() as f64 - 50.0).max(4.0)) as i32);
            }
            None => self.sel_bar.set_visible(false),
        }
    }

    pub fn copy(&self, cut: bool) {
        if !self.canvas.copy_selection() {
            return;
        }
        // Systemzwischenablage "belegen", damit Strg+V wieder unsere Auswahl einfügt
        self.canvas.clipboard().set_text("[Graphit-Auswahl]");
        if cut {
            self.canvas.delete_selection();
        }
    }

    pub fn paste(self: &Rc<Self>) {
        let cb = self.canvas.clipboard();
        let has_image = cb.formats().contains_type(gdk::Texture::static_type());
        if !cb.is_local() && has_image {
            let v = self.clone();
            cb.read_texture_async(gio::Cancellable::NONE, move |res| {
                if let Ok(Some(tex)) = res {
                    let (page, pos) = v.canvas.paste_target();
                    let png = tex.save_to_png_bytes();
                    if let Err(e) = v.canvas.insert_image(page, pos, &png) {
                        (v.toast)(&format!("Einfügen fehlgeschlagen: {e}"));
                    }
                }
            });
            return;
        }
        if !cb.is_local() && cb.formats().contain_mime_type("text/plain") {
            let v = self.clone();
            cb.read_text_async(gio::Cancellable::NONE, move |res| {
                if let Ok(Some(text)) = res {
                    v.canvas.paste_text(&text);
                }
            });
            return;
        }
        self.canvas.paste();
    }
}
