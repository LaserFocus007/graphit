//! Erster Start: Notizordner wählen und zeigen, welche optionalen Helfer vorhanden sind.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::settings::{self, Settings};

/// Status einer optionalen Abhängigkeit.
fn status_row(title: &str, found: bool, ok: &str, missing: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(if found { ok } else { missing }).build();
    let icon = gtk::Image::from_icon_name(if found { "object-select-symbolic" } else { "dialog-information-symbolic" });
    row.add_suffix(&icon);
    row
}

fn present(program: &str) -> bool {
    glib::find_program_in_path(program).is_some()
}

/// Zeile mit Ordner und Knopf zum Ändern.
pub fn folder_row(title: &str, window: &impl IsA<gtk::Widget>, value: Rc<RefCell<Option<PathBuf>>>, empty: &str, changed: Rc<dyn Fn(&PathBuf)>) -> adw::ActionRow {
    let show = |v: &Option<PathBuf>| v.as_ref().map_or(empty.to_string(), |p| p.display().to_string());
    let row = adw::ActionRow::builder().title(title).subtitle(show(&value.borrow())).build();
    let button = gtk::Button::with_label("Ändern…");
    button.set_valign(gtk::Align::Center);
    let (win, r, empty) = (window.as_ref().clone(), row.clone(), empty.to_string());
    button.connect_clicked(move |_| {
        let dialog = gtk::FileDialog::builder().title("Ordner wählen").modal(true).build();
        if let Some(p) = value.borrow().as_ref().filter(|p| p.is_dir()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(p)));
        }
        let (v, r, changed, empty) = (value.clone(), r.clone(), changed.clone(), empty.clone());
        let parent = win.root().and_downcast::<gtk::Window>();
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Some(p) = res.ok().and_then(|f| f.path()) {
                changed(&p);
                v.replace(Some(p));
                r.set_subtitle(&v.borrow().as_ref().map_or(empty.clone(), |p| p.display().to_string()));
            }
        });
    });
    row.add_suffix(&button);
    row
}

/// Zeigt beim ersten Start den Dialog und ruft danach `done` auf.
pub fn then(app: &adw::Application, done: impl FnOnce(&adw::Application) + 'static) {
    if settings::library_chosen() || std::env::vars().any(|(k, _)| k.starts_with("GRAPHIT_") && k != "GRAPHIT_WELCOME" && k != "GRAPHIT_NO_RECENT") {
        done(app);
        return;
    }
    let window = adw::ApplicationWindow::builder().application(app).title("Willkommen bei Graphit").default_width(520).default_height(760).build();
    let library: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(Some(settings::default_library())));
    let exams: Rc<RefCell<Option<PathBuf>>> = Rc::default();

    let icon = gtk::Image::from_icon_name(crate::APP_ID);
    icon.set_pixel_size(84);
    let title = gtk::Label::new(Some("Willkommen bei Graphit"));
    title.add_css_class("title-1");
    let intro = gtk::Label::new(Some("Handschriftliche Notizen und PDFs, mit Claude als Lernhilfe. Du kannst alles später in den Einstellungen ändern."));
    intro.set_wrap(true);
    intro.set_justify(gtk::Justification::Center);
    intro.add_css_class("dim-label");

    let folders = adw::PreferencesGroup::builder().title("Deine Ordner").build();
    folders.add(&folder_row("Notizordner", &window, library.clone(), "", Rc::new(|_| {})));
    folders.add(&folder_row("Altklausuren (optional)", &window, exams.clone(), "Keiner gewählt", Rc::new(|_| {})));

    let helpers = adw::PreferencesGroup::builder().title("Optionale Helfer").description("Graphit läuft auch ohne sie, nur die jeweilige Funktion fehlt dann.").build();
    let claude = present("claude") || glib::home_dir().join(".local/bin/claude").exists();
    helpers.add(&status_row("Claude Code", claude, "Gefunden, die KI-Funktionen sind bereit", "Nicht gefunden: Installiere Claude Code und melde dich an, um die KI zu nutzen"));
    helpers.add(&status_row("LaTeX und Pandoc", present("pdflatex") && present("pandoc"), "Gefunden, Formeln werden sauber gesetzt", "Nicht gefunden: Formeln erscheinen in Claudes Antworten als Rohtext"));

    let start = gtk::Button::with_label("Los geht’s");
    start.add_css_class("suggested-action");
    start.add_css_class("pill");
    start.set_halign(gtk::Align::Center);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    for w in [icon.upcast_ref::<gtk::Widget>(), title.upcast_ref(), intro.upcast_ref(), folders.upcast_ref(), helpers.upcast_ref(), start.upcast_ref()] {
        body.append(w);
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    view.set_content(Some(&gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).build()));
    window.set_content(Some(&view));

    let (app2, win, done) = (app.clone(), window.clone(), RefCell::new(Some(done)));
    start.connect_clicked(move |_| {
        if let Some(p) = library.borrow().as_ref() {
            let _ = std::fs::create_dir_all(p);
            settings::choose_library(p);
        }
        if let Some(p) = exams.borrow().as_ref() {
            let mut cfg = Settings::load();
            cfg.exam_root = p.display().to_string();
            cfg.save();
        }
        if let Some(f) = done.borrow_mut().take() {
            f(&app2);
        }
        win.close();
    });
    window.present();
}
