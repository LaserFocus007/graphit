mod calibrate;
mod canvas;
mod claude;
mod docview;
mod drive;
mod geom;
mod history;
mod i18n;
mod icons;
mod index;
mod latex;
mod library;
mod model;
mod pdftext;
mod plot;
mod pdfsearch;
mod place;
mod positions;
mod prefs;
mod recent;
mod render;
mod settings;
mod shapes;
mod speech;
mod sidebar;
mod tiles;
mod tools;
mod update;
mod welcome;
mod window;
mod xml;
mod xopp;

use adw::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.graphit";

fn main() -> glib::ExitCode {
    // GTKs eigene Texte folgen der gewählten Sprache; hier läuft noch kein zweiter Thread
    unsafe { std::env::set_var("LANGUAGE", i18n::code()) };
    // Abstürze in ~/.cache/graphit/crash.log protokollieren
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let log = glib::user_cache_dir().join("graphit").join("crash.log");
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("{}\n{info}\n{bt}\n\n", glib::DateTime::now_local().map(|d| d.format_iso8601().unwrap_or_default().to_string()).unwrap_or_default());
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let _ = f.write_all(msg.as_bytes());
        }
        default_hook(info);
    }));
    claude::restore_keyboard();
    // Selbsttest ohne GUI: graphit --check datei.xopp ...
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|a| a == "--check").unwrap_or(false) {
        return check(&args[2..]);
    }
    if args.len() == 4 && args[1] == "--resave" {
        let d = xopp::load(std::path::Path::new(&args[2])).expect("laden");
        xopp::save(&xopp::SaveData::from_doc(&d), std::path::Path::new(&args[3])).expect("speichern");
        return glib::ExitCode::SUCCESS;
    }

    // Testläufe nie an eine laufende Instanz weiterreichen
    let testing = ["GRAPHIT_SELFTEST", "GRAPHIT_PROFILE", "GRAPHIT_ACTION", "GRAPHIT_VIEWER", "GRAPHIT_VIEWER2", "GRAPHIT_DARK", "GRAPHIT_PENLOG", "GRAPHIT_TABS", "GRAPHIT_CLAUDE", "GRAPHIT_CLAUDE_PIN", "GRAPHIT_CLAUDE_NEXT", "GRAPHIT_CLAUDE_PAD", "GRAPHIT_CLAUDE_SOURCE", "GRAPHIT_TOOL_ASK", "GRAPHIT_TOUR", "GRAPHIT_WELCOME", "GRAPHIT_UPDATE"].iter().any(|v| std::env::var(v).is_ok());
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if testing {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(APP_ID).flags(flags).build();
    app.connect_startup(|_| icons::install());
    app.connect_startup(|_| gtk::Window::set_default_icon_name(APP_ID));
    // „Neues Fenster“ im Rechtsklick-Menü von Dock/Taskleiste: GNOME bietet es bei
    // laufender App nur an, wenn sie diese Aktion hat
    app.connect_startup(|app| {
        let a = gio::SimpleAction::new("new-window", None);
        let weak = app.downgrade();
        a.connect_activate(move |_, _| {
            let Some(app) = weak.upgrade() else { return };
            let w = window::Win::new(&app);
            w.window.present();
            leak(w);
        });
        app.add_action(&a);
    });
    app.connect_activate(|app| {
        welcome::then(app, |app| {
            let w = window::Win::new(app);
            w.window.present();
            test_hooks(&w);
            update::start(&w);
            leak(w);
        });
    });
    app.connect_open(|app, files, _| {
        let files = files.to_vec();
        welcome::then(app, move |app| {
            let w = window::Win::new(app);
            for f in &files {
                match f.path() {
                    Some(p) => w.open_path(&p),
                    None => w.open_remote(f.clone()),
                }
            }
            w.window.present();
            test_hooks(&w);
            update::start(&w);
            leak(w);
        });
    });
    app.run()
}

/// Hilfen für automatische Tests (GRAPHIT_SELFTEST, GRAPHIT_ACTION=win.xyz).
fn test_hooks(w: &std::rc::Rc<window::Win>) {
    if std::env::var("GRAPHIT_SELFTEST").is_ok() {
        let c = w.canvas();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || c.selftest());
    }
    // Dunkelmodus nur für diesen Testlauf an (1) oder aus (0), wird nicht gespeichert
    if let Ok(v) = std::env::var("GRAPHIT_DARK") {
        w.canvas().st().dark_paper = v != "0";
    }
    if let Ok(p) = std::env::var("GRAPHIT_VIEWER") {
        w.show_viewer_with(std::path::Path::new(&p));
    }
    if let Ok(p) = std::env::var("GRAPHIT_VIEWER2") {
        w.show_viewer2_with(std::path::Path::new(&p));
    }
    // Tabs der Reihe nach durchschalten (prüft u. a. die Lese-Viewer je Tab)
    if let Ok(v) = std::env::var("GRAPHIT_TABS") {
        for (k, i) in v.split(',').filter_map(|s| s.parse::<usize>().ok()).enumerate() {
            let w = w.clone();
            let step = std::env::var("GRAPHIT_TABS_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(700);
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500 + step * k as u64), move || w.select_tab(i));
        }
    }
    if let Ok(p) = std::env::var("GRAPHIT_TOUR") {
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1800), move || w.claude.start_tour_file(std::path::Path::new(&p)));
    }
    if let Ok(q) = std::env::var("GRAPHIT_CLAUDE") {
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            w.claude.open();
            if let Ok(i) = std::env::var("GRAPHIT_CLAUDE_IMAGE") {
                w.claude.attach_image(std::path::Path::new(&i));
            }
            // leer: nur öffnen
            if !q.is_empty() {
                w.claude.ask(&q);
            }
        });
    }
    // "ms:Frage": spätere Frage im selben Gespräch
    if let Some((ms, q)) = std::env::var("GRAPHIT_CLAUDE_NEXT").ok().and_then(|v| v.split_once(':').map(|(a, b)| (a.parse().unwrap_or(0), b.to_string()))) {
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || w.claude.ask(&q));
    }
    // "ms": schreibt nach dieser Zeit "2+2" ins Schreibfeld und sendet es kurz darauf
    if let Some(ms) = std::env::var("GRAPHIT_CLAUDE_PAD").ok().and_then(|v| v.parse::<u64>().ok()) {
        let w2 = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || w2.claude.test_pad());
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms + 2500), move || w.claude.ask(""));
    }
    if let Some(ms) = std::env::var("GRAPHIT_CLAUDE_PEN").ok().and_then(|v| v.parse::<u64>().ok()) {
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || w.claude.test_pen());
    }
    // "ms|Seite|Pfad": Quelle wie über ihren Knopf öffnen
    if let Ok(v) = std::env::var("GRAPHIT_CLAUDE_SOURCE") {
        let parts: Vec<String> = v.splitn(3, '|').map(String::from).collect();
        if let [ms, page, path] = &parts[..] {
            let (w, page, path) = (w.clone(), page.parse().unwrap_or(1), std::path::PathBuf::from(path));
            glib::timeout_add_local_once(std::time::Duration::from_millis(ms.parse().unwrap_or(2000)), move || w.claude.test_source(&path, page));
        }
    }
    if std::env::var("GRAPHIT_TOOL_ASK").is_ok() {
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || w.canvas().test_circle());
    }
    // Zeiten in ms, zu denen das KI-Fenster an- oder abgeheftet wird
    if let Ok(times) = std::env::var("GRAPHIT_CLAUDE_PIN") {
        for ms in times.split(',').filter_map(|t| t.parse().ok()) {
            let w = w.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || w.claude.toggle_pin());
        }
    }
    if let Ok(a) = std::env::var("GRAPHIT_ACTION") {
        let win = w.window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            let _ = gtk::prelude::WidgetExt::activate_action(&win, &a, None);
        });
    }
}

/// Fenster leben bis zum Schließen; die Rc-Referenz hängt am GTK-Fenster.
fn leak(w: std::rc::Rc<window::Win>) {
    let holder = std::cell::RefCell::new(Some(w.clone()));
    w.window.connect_destroy(move |_| {
        holder.borrow_mut().take();
    });
}

fn check(files: &[String]) -> glib::ExitCode {
    let mut bad = 0;
    for a in files {
        let p = std::path::Path::new(a);
        match xopp::load(p) {
            Ok(d) => {
                let n: usize = d.pages.iter().map(|p| p.borrow().layers.iter().map(|l| l.items.len()).sum::<usize>()).sum();
                let s = xopp::serialize(&xopp::SaveData::from_doc(&d), p);
                let root = xml::parse(&s).unwrap();
                let n2: usize = root
                    .children
                    .iter()
                    .filter(|c| c.name == "page")
                    .flat_map(|p| p.children.iter().filter(|l| l.name == "layer"))
                    .map(|l| l.children.len())
                    .sum();
                if n != n2 {
                    println!("UNTERSCHIED {a}: {n} vs {n2}");
                    bad += 1;
                }
            }
            Err(e) => {
                println!("FEHLER {a}: {e}");
                bad += 1;
            }
        }
    }
    println!("{} Dateien, {bad} Probleme", files.len());
    if bad == 0 { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE }
}
