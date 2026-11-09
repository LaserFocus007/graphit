//! Einstellungsdialog. Änderungen wirken sofort und werden gespeichert.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::canvas::Canvas;
use crate::settings::{ButtonAction, CursorStyle, InkStyle, PalmMode, Settings, PAPERS};

/// Ändert eine Einstellung auf dem Canvas (und speichert sie).
fn update(canvas: &Canvas, f: impl FnOnce(&mut Settings)) {
    let mut cfg = canvas.st().cfg.clone();
    f(&mut cfg);
    canvas.apply_settings(cfg);
}

fn combo(title: &str, subtitle: &str, items: &[&str], selected: usize, on: impl Fn(usize) + 'static) -> adw::ComboRow {
    let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(items)).selected(selected as u32).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.connect_selected_notify(move |r| on(r.selected() as usize));
    row
}

fn switch(title: &str, subtitle: &str, active: bool, on: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.connect_active_notify(move |r| on(r.is_active()));
    row
}

fn calib_subtitle(lo: f64, hi: f64) -> String {
    if lo <= 0.001 && hi >= 0.999 {
        "Nicht kalibriert: voller Druckbereich des Stifts".into()
    } else {
        format!("Kalibriert: Druck {lo:.2}–{hi:.2} ergibt dünn bis dick")
    }
}

/// Index des Eintrags, dessen Wert am nächsten liegt.
fn nearest(values: &[f64], v: f64) -> usize {
    values.iter().enumerate().min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs())).map(|e| e.0).unwrap_or(0)
}

pub fn show(parent: &impl IsA<gtk::Widget>, canvas: &Canvas) {
    let cfg = canvas.st().cfg.clone();
    let c = Rc::new(canvas.clone());
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Einstellungen");

    // ---------------------------------------------------------------- Stift
    let page = adw::PreferencesPage::builder().title("Stift").icon_name("fk-pen-symbolic").build();

    let feel = adw::PreferencesGroup::builder().title("Schreibgefühl").build();
    let cc = c.clone();
    feel.add(&switch("Druckempfindlichkeit", "Strichbreite folgt dem Stiftdruck", cfg.pressure, move |v| {
        update(&cc, |s| s.pressure = v)
    }));
    const CURVES: [f64; 4] = [0.6, 0.85, 1.0, 1.5];
    let cc = c.clone();
    feel.add(&combo(
        "Druckkurve",
        "Wie stark leichter Druck schon breite Striche ergibt",
        &["Weich", "Normal", "Linear", "Hart"],
        nearest(&CURVES, cfg.pressure_curve),
        move |i| update(&cc, |s| s.pressure_curve = CURVES[i]),
    ));
    let smooth_row = adw::ActionRow::builder().title("Glättung").subtitle("Beruhigt Zittern bei langsamer Bewegung; schnelle Schrift bleibt unverändert").build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
    scale.set_value(cfg.smoothing * 100.0);
    scale.set_width_request(180);
    scale.set_valign(gtk::Align::Center);
    scale.add_mark(40.0, gtk::PositionType::Bottom, None);
    let cc = c.clone();
    scale.connect_value_changed(move |s| update(&cc, |cfg| cfg.smoothing = s.value() / 100.0));
    smooth_row.add_suffix(&scale);
    feel.add(&smooth_row);
    const PRED: [f64; 5] = [0.0, 8.0, 14.0, 22.0, 30.0];
    let cc = c.clone();
    feel.add(&combo(
        "Vorhersage",
        "Zeichnet den Strich ein Stück voraus und gleicht so die Bildschirm-Verzögerung aus",
        &["Aus", "Leicht (8 ms)", "Normal (14 ms)", "Stark (22 ms)", "Sehr stark (30 ms)"],
        nearest(&PRED, cfg.prediction_ms),
        move |i| update(&cc, |s| s.prediction_ms = PRED[i]),
    ));
    let calib_row = adw::ActionRow::builder()
        .title("Druck kalibrieren")
        .subtitle(&calib_subtitle(cfg.pressure_lo, cfg.pressure_hi))
        .build();
    let calib_btn = gtk::Button::with_label("Starten…");
    calib_btn.set_valign(gtk::Align::Center);
    let reset_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
    reset_btn.set_valign(gtk::Align::Center);
    reset_btn.set_tooltip_text(Some("Kalibrierung zurücksetzen"));
    reset_btn.add_css_class("flat");
    calib_row.add_suffix(&reset_btn);
    calib_row.add_suffix(&calib_btn);
    let (cc, row) = (c.clone(), calib_row.clone());
    calib_btn.connect_clicked(move |b| {
        let row = row.clone();
        crate::calibrate::show(b, &cc, move |lo, hi| row.set_subtitle(&calib_subtitle(lo, hi)));
    });
    let (cc, row) = (c.clone(), calib_row.clone());
    reset_btn.connect_clicked(move |_| {
        update(&cc, |s| {
            s.pressure_lo = 0.0;
            s.pressure_hi = 1.0;
        });
        row.set_subtitle(&calib_subtitle(0.0, 1.0));
    });
    feel.add(&calib_row);
    let cc = c.clone();
    feel.add(&switch(
        "Neigung wie beim Bleistift",
        "Schräg gehaltener Stift (ab etwa 35°) macht den Strich breiter, zum Schraffieren",
        cfg.tilt_shading,
        move |v| update(&cc, |s| s.tilt_shading = v),
    ));
    page.add(&feel);

    let look = adw::PreferencesGroup::builder().title("Darstellung").build();
    let cc = c.clone();
    look.add(&combo(
        "Strichdarstellung",
        "Ändert nur die Anzeige, die Datei bleibt gleich",
        &["Glatt (stufenlose Breite)", "Klassisch (wie Xournal++)"],
        if cfg.ink_style == InkStyle::Classic { 1 } else { 0 },
        move |i| update(&cc, |s| s.ink_style = if i == 1 { InkStyle::Classic } else { InkStyle::Smooth }),
    ));
    let cc = c.clone();
    look.add(&combo(
        "Cursor beim Schweben",
        "Beim Schreiben selbst ist der Cursor immer ausgeblendet",
        &["Punkt in Stiftfarbe", "Fadenkreuz", "Keiner"],
        match cfg.cursor {
            CursorStyle::Dot => 0,
            CursorStyle::Crosshair => 1,
            CursorStyle::None => 2,
        },
        move |i| {
            update(&cc, |s| {
                s.cursor = [CursorStyle::Dot, CursorStyle::Crosshair, CursorStyle::None][i];
            })
        },
    ));
    page.add(&look);

    let help = adw::PreferencesGroup::builder().title("Helfer").build();
    let cc = c.clone();
    help.add(&switch(
        "Halten zum Begradigen",
        "Stift mitten im Strich kurz still halten macht ihn zur geraden Linie",
        cfg.hold_to_straighten,
        move |v| update(&cc, |s| s.hold_to_straighten = v),
    ));
    const HOLD: [f64; 3] = [300.0, 450.0, 700.0];
    let cc = c.clone();
    help.add(&combo(
        "Haltedauer",
        "",
        &["Kurz (0,3 s)", "Normal (0,45 s)", "Lang (0,7 s)"],
        nearest(&HOLD, cfg.hold_delay_ms as f64),
        move |i| update(&cc, |s| s.hold_delay_ms = HOLD[i] as u64),
    ));
    let cc = c.clone();
    help.add(&switch(
        "Formen beim Absetzen erkennen",
        "Kreise, Rechtecke, Dreiecke und Linien werden automatisch sauber",
        cfg.auto_shapes,
        move |v| update(&cc, |s| s.auto_shapes = v),
    ));
    page.add(&help);

    let eraser = adw::PreferencesGroup::builder().title("Radierer").build();
    let cc = c.clone();
    eraser.add(&switch(
        "Ganze Striche löschen",
        "Kein Radius: jeder Strich, den die Spitze kreuzt, verschwindet komplett",
        cfg.eraser_whole,
        move |v| update(&cc, |s| s.eraser_whole = v),
    ));
    page.add(&eraser);

    let buttons = adw::PreferencesGroup::builder()
        .title("Stifttasten")
        .description("Taste gedrückt halten und schreiben. Sind die Tasten vertauscht, hier einfach tauschen.")
        .build();
    let labels: Vec<&str> = ButtonAction::ALL.iter().map(|a| a.label()).collect();
    let idx = |a: ButtonAction| ButtonAction::ALL.iter().position(|x| *x == a).unwrap_or(0);
    let cc = c.clone();
    buttons.add(&combo("Stifttaste 1", "Untere Taste (näher an der Spitze)", &labels, idx(cfg.button1), move |i| {
        update(&cc, |s| s.button1 = ButtonAction::ALL[i])
    }));
    let cc = c.clone();
    buttons.add(&combo("Stifttaste 2", "Obere Taste", &labels, idx(cfg.button2), move |i| {
        update(&cc, |s| s.button2 = ButtonAction::ALL[i])
    }));
    let cc = c.clone();
    buttons.add(&combo("Radierer-Ende", "Stift umgedreht", &labels, idx(cfg.eraser_end), move |i| {
        update(&cc, |s| s.eraser_end = ButtonAction::ALL[i])
    }));
    page.add(&buttons);

    let tap = adw::PreferencesGroup::builder()
        .title("Kurzes Tippen")
        .description("Wie der Strichfilter in Xournal++: ein sehr kurzer Tipp wird nicht als Punkt gezeichnet. Ein Tipp direkt nach einem Strich bleibt ein Punkt (i-Punkt).")
        .build();
    let cc = c.clone();
    tap.add(&switch("Objekt unter dem Stift auswählen", "", cfg.tap_select, move |v| update(&cc, |s| s.tap_select = v)));
    let cc = c.clone();
    tap.add(&switch("Sonst Radialmenü öffnen", "", cfg.tap_radial, move |v| update(&cc, |s| s.tap_radial = v)));
    const TMS: [f64; 4] = [50.0, 70.0, 100.0, 150.0];
    let cc = c.clone();
    tap.add(&combo(
        "Höchstdauer eines Tipps",
        "",
        &["50 ms", "70 ms", "100 ms", "150 ms"],
        nearest(&TMS, cfg.tap_ms as f64),
        move |i| update(&cc, |s| s.tap_ms = TMS[i] as u64),
    ));
    page.add(&tap);

    let radial = adw::PreferencesGroup::builder()
        .title("Radialmenü")
        .description("Ring mit Werkzeugen und Farben direkt an der Stiftspitze. Auch per Rechtsklick.")
        .build();
    let cc = c.clone();
    radial.add(&switch("Auch per langem Druck öffnen", "Stift still auf eine Stelle halten", cfg.radial_longpress, move |v| {
        update(&cc, |s| s.radial_longpress = v)
    }));
    const RD: [f64; 3] = [300.0, 450.0, 700.0];
    let cc = c.clone();
    radial.add(&combo(
        "Dauer",
        "",
        &["Kurz (0,3 s)", "Normal (0,45 s)", "Lang (0,7 s)"],
        nearest(&RD, cfg.radial_delay_ms as f64),
        move |i| update(&cc, |s| s.radial_delay_ms = RD[i] as u64),
    ));
    page.add(&radial);

    dialog.add(&page);

    // ---------------------------------------------------------------- Papier
    let page = adw::PreferencesPage::builder().title("Papier").icon_name("fk-pages-symbolic").build();
    let folders = adw::PreferencesGroup::builder().title("Ordner").description("Änderungen gelten nach einem Neustart von Graphit.").build();
    folders.add(&crate::welcome::folder_row("Notizordner", &dialog, Rc::new(RefCell::new(Some(crate::settings::library()))), "", Rc::new(|p| crate::settings::choose_library(p))));
    let cc = c.clone();
    let exams = (!cfg.exam_root.is_empty()).then(|| std::path::PathBuf::from(&cfg.exam_root));
    folders.add(&crate::welcome::folder_row("Altklausuren", &dialog, Rc::new(RefCell::new(exams)), "Old_exams im Notizordner", Rc::new(move |p| update(&cc, |s| s.exam_root = p.display().to_string()))));
    page.add(&folders);
    let g = adw::PreferencesGroup::builder()
        .title("Neue Seiten")
        .description("Gilt für neue Dokumente und für Seiten, die nach einem PDF eingefügt werden. Einzelne Seiten änderst du im Menü unter „Papier dieser Seite“ oder in der Seitenleiste.")
        .build();
    let labels: Vec<&str> = PAPERS.iter().map(|p| p.0).collect();
    let cc = c.clone();
    g.add(&combo(
        "Standardpapier",
        "",
        &labels,
        PAPERS.iter().position(|p| p.1 == cfg.paper_style).unwrap_or(0),
        move |i| update(&cc, |s| s.paper_style = PAPERS[i].1.into()),
    ));
    page.add(&g);
    let g = adw::PreferencesGroup::builder().title("Darstellung").build();
    let cc = c.clone();
    g.add(&switch("Dunkles Papier", "Auch über den Mond-Knopf oben rechts oder Strg+Umschalt+D", cfg.dark_paper, move |v| {
        if cc.st().dark_paper != v {
            let _ = cc.activate_action("win.dark-paper", None);
        }
    }));
    page.add(&g);
    dialog.add(&page);

    // ---------------------------------------------------------------- Touch
    let page = adw::PreferencesPage::builder().title("Touch").icon_name("fk-hand-symbolic").build();
    let g = adw::PreferencesGroup::builder()
        .title("Finger")
        .description("Finger schreiben nie, sie verschieben und zoomen nur.")
        .build();
    let cc = c.clone();
    g.add(&combo(
        "Handballen-Erkennung",
        "Wann Berührungen ignoriert werden",
        &["Nur während der Stift aufliegt", "Solange der Stift in der Nähe ist", "Aus"],
        match cfg.palm {
            PalmMode::Contact => 0,
            PalmMode::Proximity => 1,
            PalmMode::Off => 2,
        },
        move |i| update(&cc, |s| s.palm = [PalmMode::Contact, PalmMode::Proximity, PalmMode::Off][i]),
    ));
    const TPRED: [f64; 4] = [0.0, 8.0, 14.0, 22.0];
    let cc = c.clone();
    g.add(&combo(
        "Finger-Vorhersage",
        "Schiebt und zoomt um ein paar Millisekunden voraus, damit die Seite am Finger klebt",
        &["Aus", "Leicht (8 ms)", "Normal (14 ms)", "Stark (22 ms)"],
        nearest(&TPRED, cfg.touch_predict_ms),
        move |i| update(&cc, |s| s.touch_predict_ms = TPRED[i]),
    ));
    page.add(&g);

    let g = adw::PreferencesGroup::builder().title("Schwung").description("Wie die Seite nach einem Wisch mit einem Finger weitergleitet.").build();
    let cc = c.clone();
    g.add(&switch("Schwung beim Wischen", "Seite gleitet nach dem Loslassen weiter", cfg.kinetic, move |v| {
        update(&cc, |s| s.kinetic = v)
    }));
    let slider = |title: &str, sub: &str, lo: f64, hi: f64, val: f64, mark: f64, low: &str, high: &str| {
        let row = adw::ActionRow::builder().title(title).subtitle(sub).build();
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let l = gtk::Label::new(Some(low));
        l.add_css_class("dim-label");
        l.add_css_class("caption");
        let h = gtk::Label::new(Some(high));
        h.add_css_class("dim-label");
        h.add_css_class("caption");
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, lo, hi, (hi - lo) / 20.0);
        scale.set_value(val);
        scale.set_width_request(160);
        scale.set_valign(gtk::Align::Center);
        scale.add_mark(mark, gtk::PositionType::Bottom, None);
        bx.append(&l);
        bx.append(&scale);
        bx.append(&h);
        bx.set_valign(gtk::Align::Center);
        row.add_suffix(&bx);
        (row, scale)
    };
    let (row, scale) = slider("Gleitweite", "Wie lange die Seite nachläuft", 0.0, 1.0, cfg.kinetic_glide, 0.5, "kurz", "lang");
    let cc = c.clone();
    scale.connect_value_changed(move |s| update(&cc, |cfg| cfg.kinetic_glide = s.value()));
    g.add(&row);
    let (row, scale) = slider("Stärke", "Wie viel Tempo ein Wisch mitgibt", 0.4, 2.0, cfg.kinetic_boost, 1.0, "sanft", "kräftig");
    let cc = c.clone();
    scale.connect_value_changed(move |s| update(&cc, |cfg| cfg.kinetic_boost = s.value()));
    g.add(&row);
    let cc = c.clone();
    g.add(&switch(
        "Nur in Wischrichtung",
        "Beim Wischen nach oben oder unten driftet die Seite nicht seitlich weg",
        cfg.kinetic_axis_lock,
        move |v| update(&cc, |s| s.kinetic_axis_lock = v),
    ));
    page.add(&g);
    dialog.add(&page);

    // ---------------------------------------------------------------- KI
    let page = adw::PreferencesPage::builder().title("KI").icon_name("fk-spark-symbolic").build();
    let claude = adw::PreferencesGroup::builder()
        .title("Claude")
        .description("Esc öffnet den Dialog. Pro Modul läuft ein eigenes Gespräch.")
        .build();
    let cc = c.clone();
    let names: Vec<&str> = crate::claude::MODELS.iter().map(|m| m.0).collect();
    claude.add(&combo(
        "Standardmodell",
        "Damit startet der Dialog; dort lässt es sich jederzeit wechseln",
        &names,
        crate::claude::MODELS.iter().position(|m| m.1 == cfg.claude_model).unwrap_or(2),
        move |i| update(&cc, |s| s.claude_model = crate::claude::MODELS[i].1.into()),
    ));
    let cc = c.clone();
    let names: Vec<&str> = crate::claude::EFFORTS.iter().map(|e| e.0).collect();
    claude.add(&combo(
        "Standard-Effort",
        "Wie gründlich Claude nachdenkt: höher ist genauer, aber langsamer",
        &names,
        crate::claude::EFFORTS.iter().position(|e| e.1 == cfg.claude_effort).unwrap_or(0),
        move |i| update(&cc, |s| s.claude_effort = crate::claude::EFFORTS[i].1.into()),
    ));
    let cc = c.clone();
    claude.add(&switch(
        "Bildschirmtastatur automatisch öffnen",
        "Beim Öffnen über Stern oder Menü, nicht bei Esc",
        cfg.claude_osk,
        move |v| update(&cc, |s| s.claude_osk = v),
    ));
    // Altklausuren: Vorschlag aus den Ordnernamen, hier korrigierbar
    let root = crate::window::library_dir();
    let folders = crate::index::exam_folders(&root);
    let modules = crate::index::modules(&root);
    if !folders.is_empty() && !modules.is_empty() {
        let exams = adw::ExpanderRow::builder().title("Altklausuren je Modul").subtitle("Claude durchsucht diese Ordner mit. Vorgeschlagen aus den Ordnernamen.").build();
        let mut names: Vec<&str> = vec!["Keine"];
        names.extend(folders.iter().map(String::as_str));
        let choices = crate::index::parse_choices(&cfg.claude_exams);
        for module in modules {
            let current = choices.get(&module).map(String::as_str).or(crate::index::suggest(&module, &folders).map(String::as_str));
            let selected = current.and_then(|c| folders.iter().position(|f| f == c)).map_or(0, |i| i + 1);
            let (cc, list, name) = (c.clone(), folders.clone(), module.clone());
            exams.add_row(&combo(&module, "", &names, selected, move |i| {
                update(&cc, |s| {
                    let mut map = crate::index::parse_choices(&s.claude_exams);
                    map.insert(name.clone(), if i == 0 { String::new() } else { list[i - 1].clone() });
                    s.claude_exams = crate::index::format_choices(&map);
                })
            }));
        }
        claude.add(&exams);
    }
    page.add(&claude);
    dialog.add(&page);

    dialog.present(Some(parent));
}
