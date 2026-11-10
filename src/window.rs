//! Hauptfenster: Tabs, Seitenleiste, Split-View mit Lese-Viewer, Bibliothek
//! und das schwebende Dock.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::canvas::{Canvas, ZOOM_100};
use crate::docview::{rgb_css, DocView};
use crate::library::Library;
use crate::model::Document;
use crate::render;
use crate::sidebar::Sidebar;
use crate::tools::*;
use crate::xopp;

const CSS: &str = r#"
.fk-dock {
  background-color: @popover_bg_color;
  border-radius: 24px;
  padding: 5px 8px;
  box-shadow: 0 1px 3px alpha(black, .18), 0 6px 18px alpha(black, .16), 0 0 0 1px alpha(@window_fg_color, .07);
  transition: opacity 250ms ease;
}
.fk-dock button { min-width: 34px; min-height: 34px; padding: 0; border-radius: 17px; }
.fk-dock button.fk-tool:checked { background-color: alpha(@accent_bg_color, .22); color: @accent_color; }
.fk-dock separator { margin: 6px 4px; }
.fk-swatch { min-width: 20px; min-height: 20px; border-radius: 10px; box-shadow: inset 0 0 0 1px alpha(black, .18); }
.fk-dock button.fk-color { min-width: 28px; min-height: 28px; border-radius: 14px; }
.fk-dock button.fk-color.active { box-shadow: 0 0 0 2px @accent_color; }
.fk-dot { background-color: @window_fg_color; border-radius: 99px; }
.fk-dock button.fk-width.active { background-color: alpha(@accent_bg_color, .22); }
.fk-options { padding: 8px; }
.fk-claude, .fk-claude-guide { background-color: #1f1f23; color: #f1f0ed; border-radius: 18px;
  box-shadow: 0 24px 64px alpha(black, .45), 0 4px 14px alpha(black, .28), inset 0 0 0 1px alpha(white, .09); }
@keyframes fk-claude-pop { from { opacity: 0; transform: translateY(-10px) scale(.985); } to { opacity: 1; transform: none; } }
@keyframes fk-claude-step-a { from { opacity: .4; transform: translateY(5px); } to { opacity: 1; transform: none; } }
@keyframes fk-claude-step-b { from { opacity: .4; transform: translateY(5px); } to { opacity: 1; transform: none; } }
@keyframes fk-claude-breathe { 0% { opacity: 1; } 50% { opacity: .35; } 100% { opacity: 1; } }
.fk-claude-in { animation: fk-claude-pop 170ms cubic-bezier(.2, .9, .3, 1); }
.fk-claude-step-a { animation: fk-claude-step-a 160ms ease-out; }
.fk-claude-step-b { animation: fk-claude-step-b 160ms ease-out; }
.fk-claude-busy, .fk-claude-thinking { animation: fk-claude-breathe 1.5s ease-in-out infinite; }
.fk-claude button, .fk-claude-guide button { color: #d8d6d1; }
.fk-claude.fk-claude-pinned { border-radius: 0; box-shadow: none; }
.fk-claude-column { background-color: #1f1f23; }
.fk-claude-edge { min-width: 5px; background-color: alpha(white, .07); }
.fk-claude-edge:hover { background-color: alpha(white, .22); }
.fk-claude-handle { padding: 7px 0 0; }
.fk-claude-pill { background-color: alpha(white, .16); border-radius: 3px; min-width: 36px; min-height: 4px; }
.fk-claude-handle:hover .fk-claude-pill, .fk-claude-grip:hover .fk-claude-pill { background-color: alpha(white, .42); }
.fk-claude-pinhead { padding: 8px 8px 6px 18px; }
.fk-claude-title { font-weight: 600; color: #a9a7a2; }
.fk-claude-bar { padding: 2px 10px 6px 16px; }
.fk-claude-pinned .fk-claude-bar { background-color: #2c2c31; border-radius: 14px; margin: 8px 12px 2px; padding: 2px 8px 2px 12px; }
.fk-claude-bar entry { background: none; box-shadow: none; outline: none; border: none; color: #f1f0ed;
  caret-color: #d97757; font-size: 1.15em; min-height: 42px; }
.fk-claude-spark { color: #d97757; }
.fk-claude-key { color: #8d8b86; font-size: .8em; padding: 2px 7px; border-radius: 6px; box-shadow: inset 0 0 0 1px alpha(white, .14); }
.fk-claude-rich, .fk-claude-rich text { background: none; background-color: transparent; color: #f1f0ed; caret-color: transparent; }
.fk-claude-rich text selection { background-color: alpha(#d97757, .35); }
.fk-claude-table { border-radius: 10px; box-shadow: inset 0 0 0 1px alpha(white, .14); }
textview.fk-claude-cell { border-bottom: 1px solid alpha(white, .09); }
textview.fk-claude-th, textview.fk-claude-th text { background-color: alpha(white, .08); }
list.fk-claude-quick { background: none; padding: 6px; border-top: 1px solid alpha(white, .08); }
list.fk-claude-quick row { color: #e4e2dd; border-radius: 10px; padding: 9px 11px; background: none; }
list.fk-claude-quick row:hover, list.fk-claude-quick row:selected { background-color: #2c2c31; color: #f1f0ed; }
.fk-claude-padbox { background-color: #2c2c31; border-radius: 12px; margin: 2px 12px 8px; }
.fk-claude-padbox > button { margin: 4px; min-width: 30px; min-height: 30px; }
.fk-claude-progress { padding: 8px; }
.fk-claude-progress-row { padding: 4px 6px; }
.fk-claude-source { font-size: .88em; }
.fk-term-dot { min-width: 10px; min-height: 10px; border-radius: 5px; }
.fk-term-0 { background-color: #d92b3a; } .fk-term-1 { background-color: #1f5fd1; } .fk-term-2 { background-color: #1f9a4f; }
.fk-term-3 { background-color: #e07c0d; } .fk-term-4 { background-color: #7e45c7; } .fk-term-5 { background-color: #14a3a0; }
button.fk-claude-term { background-color: #2c2c31; border-radius: 10px; padding: 6px 10px; }
button.fk-claude-term:hover { background-color: #36363c; }
button.fk-claude-term.fk-claude-term-on { background-color: #44444c; }
.fk-claude-plot { background-color: #2c2c31; border-radius: 10px; }
.fk-claude-slider { font-size: .9em; color: #d8d6d1; }
.fk-claude-foot { padding: 5px 8px 7px 10px; border-top: 1px solid alpha(white, .08); }
.fk-claude-foot button { min-width: 32px; min-height: 32px; padding: 0; }
.fk-claude-foot button:checked { background-color: alpha(white, .14); color: #f1f0ed; }
.fk-claude-model button { background: none; font-size: .88em; color: #a9a7a2; padding: 2px 8px; border-radius: 10px; min-height: 30px; }
.fk-claude-model button:hover { background-color: alpha(white, .08); }
.fk-claude-foot button.fk-claude-context { font-size: .88em; font-weight: 400; color: #8d8b86; padding: 0 8px; border-radius: 10px; }
.fk-claude-foot button.fk-claude-context-high { color: #e8c26a; }
button.fk-claude-send { background-color: #d97757; color: white; min-width: 36px; min-height: 36px; }
button.fk-claude-send:hover { background-color: #e58a6c; }
button.fk-claude-thumb { padding: 0; border-radius: 8px; min-width: 30px; min-height: 30px; }
button.fk-claude-thumb picture { border-radius: 8px; }
.fk-claude-body { border-top: 1px solid alpha(white, .08); }
.fk-claude-list { padding: 14px 18px; }
.fk-claude-user { background-color: #2c2c31; border-radius: 12px; padding: 6px 12px; }
.fk-claude-status { padding: 2px 18px 10px; color: #a9a7a2; }
button.fk-claude-insert { padding: 2px 9px; margin-left: -9px; min-height: 26px; border-radius: 8px; color: #a9a7a2; }
button.fk-claude-insert:hover { background-color: alpha(white, .08); color: #f1f0ed; }
.fk-claude-guide { padding: 12px 14px 10px; border-radius: 16px; }
.fk-claude-kind-detail { font-size: 0.82em; opacity: 0.62; }
.fk-claude-idea { min-height: 38px; padding: 5px 7px; border-radius: 10px; font-size: 0.86em; background-color: #2c2c31; color: #f1f0ed; }
.fk-claude-idea:hover { background-color: #3a3a41; }
.fk-claude-idea-on { background-color: #d97757; color: #1f1f23; }
.fk-claude-kind { font-weight: 600; font-size: .88em; color: #a9a7a2; }
.fk-claude-count { color: #8d8b86; font-size: .88em; }
.fk-claude-seg { min-height: 3px; border-radius: 2px; background-color: alpha(white, .14); }
.fk-claude-seg-on { background-color: #f1f0ed; }
button.fk-claude-next { background-color: #f1f0ed; color: #18181b; border-radius: 11px; padding: 4px 16px; }
button.fk-claude-next:hover { background-color: white; }
.fk-claude-grip { min-height: 13px; padding-top: 5px; }
.fk-claude-chips { padding: 0 14px 8px; }
button.fk-claude-hint { background: none; box-shadow: inset 0 0 0 1px alpha(white, .15); color: #d8d6d1; border-radius: 11px; padding: 5px 11px; font-size: .92em; }
button.fk-claude-hint:hover { background-color: alpha(white, .08); }
button.fk-claude-option { background-color: #2c2c31; border-radius: 10px; padding: 8px 10px; }
button.fk-claude-option:hover { background-color: #36363c; }
.fk-claude-letter { color: #8d8b86; font-size: .92em; margin-top: 2px; }
button.fk-claude-option:disabled { filter: none; opacity: 1; }
.fk-claude-option:disabled .fk-claude-rich, .fk-claude-option:disabled .fk-claude-rich text { color: #f1f0ed; opacity: 1; }
button.fk-claude-option.fk-claude-right { background-color: alpha(#4caf6e, .42); }
button.fk-claude-option.fk-claude-wrong { background-color: alpha(#e0523f, .42); }
.fk-canvas-scroll { background: transparent; }
.fk-textedit, .fk-textedit text { background-color: alpha(@accent_bg_color, .06); }
.fk-textedit { border: 1px dashed alpha(@accent_color, .7); padding: 0; }
.fk-selbar { background-color: @popover_bg_color; border-radius: 18px; padding: 3px 6px;
  box-shadow: 0 1px 3px alpha(black, .2), 0 4px 12px alpha(black, .14), 0 0 0 1px alpha(@window_fg_color, .07); }
.fk-selbar button { min-width: 30px; min-height: 30px; padding: 0 4px; border-radius: 15px; }
.fk-selbar button.fk-color { min-width: 26px; }
.fk-selbar .fk-swatch { min-width: 16px; min-height: 16px; border-radius: 8px; }
.fk-card { padding: 6px; border-radius: 12px; }
.fk-card-thumb { border-radius: 6px; background-color: white; box-shadow: 0 1px 3px alpha(black, .25); }
.fk-thumb { background-color: white; box-shadow: 0 1px 3px alpha(black, .3); }
.fk-viewer-bar { padding: 2px 6px; border-bottom: 1px solid alpha(@window_fg_color, .1); }
"#;

pub struct Win {
    pub window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    pub claude: Rc<crate::claude::ClaudePanel>,
    dock: gtk::Box,
    tool_buttons: RefCell<Vec<(Tool, gtk::ToggleButton)>>,
    color_buttons: RefCell<Vec<(usize, gtk::Button)>>,
    width_buttons: RefCell<Vec<gtk::Button>>,
    tabs: adw::TabView,
    views: RefCell<Vec<Rc<DocView>>>,
    current: RefCell<Option<Rc<DocView>>>,
    /// Ersatz-Canvas, solange kein Dokument offen ist (Aktionen laufen ins Leere).
    fallback: Canvas,
    sidebar: Rc<Sidebar>,
    split: adw::OverlaySplitView,
    paned: gtk::Paned,
    /// links: oberer und (optional) unterer Lese-Viewer übereinander
    left: gtk::Paned,
    viewer_box: gtk::Box,
    viewer_title: gtk::Label,
    viewer: RefCell<Option<Rc<DocView>>>,
    viewer2_box: gtk::Box,
    viewer2_title: gtk::Label,
    viewer2: RefCell<Option<Rc<DocView>>>,
    viewer2_btn: gtk::ToggleButton,
    /// während die Viewer eines gerade geöffneten Dokuments wiederhergestellt werden
    restoring_viewers: Cell<bool>,
    stack: gtk::Stack,
    library: Rc<Library>,
    sidebar_btn: gtk::ToggleButton,
    split_btn: gtk::ToggleButton,
}

fn swatch_css() -> String {
    let mut s = String::from(CSS);
    for (i, c) in PALETTE.iter().enumerate() {
        s.push_str(&format!(".fk-c{i} {{ background-color: {}; }}\n", rgb_css(c)));
    }
    s
}

pub fn library_dir() -> PathBuf {
    crate::settings::library()
}

impl Win {
    pub fn new(app: &adw::Application) -> Rc<Win> {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&swatch_css());
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let window = adw::ApplicationWindow::builder().application(app).default_width(1300).default_height(880).build();
        let title = adw::WindowTitle::new("Graphit", "");

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let home = gtk::Button::from_icon_name("go-home-symbolic");
        home.set_tooltip_text(Some(crate::i18n::tl("Bibliothek")));
        home.set_action_name(Some("win.library"));
        header.pack_start(&home);
        let sidebar_btn = gtk::ToggleButton::new();
        sidebar_btn.set_icon_name("sidebar-show-symbolic");
        sidebar_btn.set_tooltip_text(Some(crate::i18n::tl("Seitenleiste (F9)")));
        header.pack_start(&sidebar_btn);
        let split_btn = gtk::ToggleButton::new();
        split_btn.set_icon_name("fk-split-symbolic");
        split_btn.set_tooltip_text(Some(crate::i18n::tl("Zweites Dokument daneben lesen (F8)")));
        header.pack_start(&split_btn);

        let menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some(crate::i18n::tl("Neues Dokument")), Some("win.new"));
        s1.append(Some(crate::i18n::tl("Öffnen…")), Some("win.open"));
        s1.append(Some(crate::i18n::tl("Speichern unter…")), Some("win.save-as"));
        s1.append(Some(crate::i18n::tl("Als PDF exportieren…")), Some("win.export-pdf"));
        s1.append(Some(crate::i18n::tl("Hintergrund-PDF wählen…")), Some("win.choose-pdf"));
        s1.append(Some(crate::i18n::tl("Fixierte Objekte dieser Seite lösen")), Some("win.unlock-page"));
        s1.append(Some(crate::i18n::tl("Claude fragen (Esc)")), Some("win.claude"));
        menu.append_section(None, &s1);
        let s2 = gio::Menu::new();
        let papers = gio::Menu::new();
        for (label, style) in crate::settings::PAPERS {
            let label = crate::i18n::tl(label);
            papers.append(Some(label), Some(&format!("win.page-paper::{style}")));
        }
        s2.append_submenu(Some(crate::i18n::tl("Papier dieser Seite")), &papers);
        s2.append(Some(crate::i18n::tl("Seitenbreite")), Some("win.zoom-fit"));
        s2.append(Some(crate::i18n::tl("Vollbild")), Some("win.fullscreen"));
        menu.append_section(None, &s2);
        let s3 = gio::Menu::new();
        s3.append(Some(crate::i18n::tl("Einstellungen")), Some("win.preferences"));
        menu.append_section(None, &s3);
        let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build();
        menu_btn.set_primary(true);
        header.pack_end(&menu_btn);
        let dark_btn = gtk::ToggleButton::new();
        dark_btn.set_icon_name("weather-clear-night-symbolic");
        dark_btn.set_tooltip_text(Some(crate::i18n::tl("Dunkles Papier (Strg+Umschalt+D)")));
        dark_btn.set_action_name(Some("win.dark-paper"));
        header.pack_end(&dark_btn);
        let save_btn = gtk::Button::from_icon_name("document-save-symbolic");
        save_btn.set_tooltip_text(Some(crate::i18n::tl("Speichern (Strg+S)")));
        save_btn.set_action_name(Some("win.save"));
        header.pack_end(&save_btn);
        let open_btn = gtk::Button::from_icon_name("document-open-symbolic");
        open_btn.set_tooltip_text(Some(crate::i18n::tl("Öffnen (Strg+O)")));
        open_btn.set_action_name(Some("win.open"));
        header.pack_end(&open_btn);

        // Tabs + Dock
        let tabs = adw::TabView::new();
        let tab_bar = adw::TabBar::new();
        tab_bar.set_view(Some(&tabs));
        tab_bar.set_autohide(true);
        // Einen Tab aus der Leiste ziehen stürzt ab: libadwaita 1.5 nutzt beim Ende des Ziehens
        // unter Wayland freigegebenen Speicher (Beispiel: leicht verrutschter Tipp mit dem
        // Stift). Daher vorher abbrechen; Antippen und Umsortieren in der Leiste bleiben.
        let hold = gtk::GestureDrag::new();
        hold.set_button(0);
        hold.set_propagation_phase(gtk::PropagationPhase::Capture);
        let bar = tab_bar.downgrade();
        hold.connect_drag_update(move |g, dx, dy| {
            let (Some(bar), Some((sx, sy))) = (bar.upgrade(), g.start_point()) else { return };
            let (x, y) = (sx + dx, sy + dy);
            if x < -6.0 || y < -6.0 || x > bar.width() as f64 + 6.0 || y > bar.height() as f64 + 6.0 {
                g.set_state(gtk::EventSequenceState::Claimed);
                if std::env::var("GRAPHIT_PROFILE").is_ok() {
                    eprintln!("PROFILE Tab-Ziehen abgebrochen");
                }
            }
        });
        tab_bar.add_controller(hold);
        let editor_overlay = gtk::Overlay::new();
        editor_overlay.set_child(Some(&tabs));
        let dock = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        dock.add_css_class("fk-dock");
        dock.set_halign(gtk::Align::Center);
        dock.set_valign(gtk::Align::End);
        dock.set_margin_bottom(18);
        editor_overlay.add_overlay(&dock);
        let editor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        editor.append(&tab_bar);
        editor.append(&editor_overlay);
        editor_overlay.set_vexpand(true);

        // Seitenleiste
        let sidebar = Sidebar::new();
        let split = adw::OverlaySplitView::new();
        split.set_sidebar(Some(&sidebar.root));
        split.set_content(Some(&editor));
        split.set_show_sidebar(false);
        split.set_min_sidebar_width(190.0);
        split.set_max_sidebar_width(220.0);

        // Split-View: links Lese-Viewer, rechts Editor
        let viewer_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let vbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        vbar.add_css_class("fk-viewer-bar");
        let viewer_title = gtk::Label::new(Some(crate::i18n::tl("Lese-Viewer")));
        viewer_title.set_hexpand(true);
        viewer_title.set_xalign(0.0);
        viewer_title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        viewer_title.add_css_class("heading");
        let vopen = gtk::Button::from_icon_name("document-open-symbolic");
        vopen.add_css_class("flat");
        vopen.set_tooltip_text(Some(crate::i18n::tl("Dokument im Viewer öffnen")));
        vopen.set_action_name(Some("win.viewer-open"));
        let vclose = gtk::Button::from_icon_name("window-close-symbolic");
        vclose.add_css_class("flat");
        vclose.set_tooltip_text(Some(crate::i18n::tl("Viewer schließen")));
        let viewer2_btn = gtk::ToggleButton::new();
        viewer2_btn.set_icon_name("fk-split-below-symbolic");
        viewer2_btn.add_css_class("flat");
        viewer2_btn.set_tooltip_text(Some(crate::i18n::tl("Drittes Dokument unten lesen")));
        vbar.append(&gtk::Image::from_icon_name("x-office-document-symbolic"));
        vbar.append(&viewer_title);
        vbar.append(&viewer2_btn);
        vbar.append(&vopen);
        vbar.append(&vclose);
        viewer_box.append(&vbar);
        viewer_box.set_vexpand(true);

        // unterer Lese-Viewer (optional)
        let viewer2_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let vbar2 = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        vbar2.add_css_class("fk-viewer-bar");
        let viewer2_title = gtk::Label::new(Some(crate::i18n::tl("Lese-Viewer")));
        viewer2_title.set_hexpand(true);
        viewer2_title.set_xalign(0.0);
        viewer2_title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        viewer2_title.add_css_class("heading");
        let vopen2 = gtk::Button::from_icon_name("document-open-symbolic");
        vopen2.add_css_class("flat");
        vopen2.set_tooltip_text(Some(crate::i18n::tl("Dokument unten öffnen")));
        vopen2.set_action_name(Some("win.viewer2-open"));
        let vclose2 = gtk::Button::from_icon_name("window-close-symbolic");
        vclose2.add_css_class("flat");
        vclose2.set_tooltip_text(Some(crate::i18n::tl("Unteren Viewer schließen")));
        vbar2.append(&gtk::Image::from_icon_name("x-office-document-symbolic"));
        vbar2.append(&viewer2_title);
        vbar2.append(&vopen2);
        vbar2.append(&vclose2);
        viewer2_box.append(&vbar2);
        viewer2_box.set_vexpand(true);
        viewer2_box.set_visible(false);

        let left = gtk::Paned::new(gtk::Orientation::Vertical);
        left.set_start_child(Some(&viewer_box));
        left.set_end_child(Some(&viewer2_box));
        left.set_shrink_start_child(false);
        left.set_shrink_end_child(false);
        left.set_visible(false);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&left));
        paned.set_end_child(Some(&split));
        paned.set_resize_start_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);

        let library = Library::new(library_dir());
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&library.root, Some("library"));
        stack.add_named(&paned, Some("editor"));

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        // Inhalt und rechts daneben die angeheftete KI-Spalte
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        stack.set_hexpand(true);
        content.append(&stack);
        tv.set_content(Some(&content));
        let toasts = adw::ToastOverlay::new();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&tv));
        toasts.set_child(Some(&overlay));
        window.set_content(Some(&toasts));

        // Der Dialog fragt das Fenster nach dem aktiven Tab; das Fenster gibt es hier noch nicht
        let win_ref: Rc<RefCell<std::rc::Weak<Win>>> = Rc::new(RefCell::new(std::rc::Weak::new()));
        let (r1, r2, r3) = (win_ref.clone(), win_ref.clone(), win_ref.clone());
        let claude = crate::claude::ClaudePanel::new(
            library_dir(),
            &overlay,
            Rc::new(move |name: &str| {
                let w = r1.borrow().upgrade()?;
                match name {
                    "viewer1" => w.visible_viewer(0),
                    "viewer2" => w.visible_viewer(1),
                    _ => w.view(),
                }
            }),
            Rc::new(move |m: &str| {
                if let Some(w) = r2.borrow().upgrade() {
                    w.toast(m);
                }
            }),
            Rc::new(move |path: &Path, page: usize| {
                let Some(w) = r3.borrow().upgrade() else { return };
                w.open_path(path);
                // erst nach dem Wiederherstellen der Leseposition blättern
                glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
                    if let Some(v) = w.view() {
                        v.canvas.show_page(page);
                    }
                });
            }),
        );
        overlay.add_overlay(&claude.clamp);
        content.append(&claude.column);
        overlay.add_overlay(&claude.guide);

        let win = Rc::new(Win {
            window,
            title,
            toasts,
            claude,
            dock,
            tool_buttons: RefCell::new(Vec::new()),
            color_buttons: RefCell::new(Vec::new()),
            width_buttons: RefCell::new(Vec::new()),
            tabs,
            views: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            fallback: Canvas::new(),
            sidebar,
            split,
            paned,
            left,
            viewer_box,
            viewer_title,
            viewer: RefCell::new(None),
            viewer2_box,
            viewer2_title,
            viewer2: RefCell::new(None),
            viewer2_btn,
            restoring_viewers: Cell::new(false),
            stack,
            library,
            sidebar_btn,
            split_btn,
        });
        win_ref.replace(Rc::downgrade(&win));
        win.build_dock();
        win.setup_actions(app);

        // Esc: was nichts anderes braucht, kommt hier an und öffnet oder schließt Claude
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(&win);
        keys.connect_key_pressed(move |_, key, _, _| match w.upgrade() {
            Some(w) if w.claude.key_escape(key) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        win.window.add_controller(keys);

        // Pfeile vor allen anderen, sonst schluckt sie ein fokussierter Regler auf der Karte.
        // Eine Auswahl oder ein offenes Textfeld auf der Notiz behält sie.
        let arrows = gtk::EventControllerKey::new();
        arrows.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(&win);
        arrows.connect_key_pressed(move |_, key, _, mods| {
            let Some(w) = w.upgrade() else { return glib::Propagation::Proceed };
            let editing = w.view().is_some_and(|v| v.canvas.has_selection() || v.canvas.st().text.is_some());
            if !editing && w.claude.key_arrow(key, mods) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.window.add_controller(arrows);

        let w = Rc::downgrade(&win);
        win.tabs.connect_selected_page_notify(move |_| {
            if let Some(w) = w.upgrade() {
                w.on_tab_switched();
            }
        });
        let w = Rc::downgrade(&win);
        win.tabs.connect_close_page(move |tv, page| {
            let Some(w) = w.upgrade() else { return glib::Propagation::Proceed };
            w.on_close_tab(tv, page);
            glib::Propagation::Stop
        });
        let w = Rc::downgrade(&win);
        win.library.connect_open(move |p| {
            if let Some(w) = w.upgrade() {
                w.open_path(p);
            }
        });
        let w = Rc::downgrade(&win);
        win.sidebar_btn.connect_toggled(move |b| {
            if let Some(w) = w.upgrade() {
                w.split.set_show_sidebar(b.is_active());
                w.sidebar.refresh();
            }
        });
        let w = Rc::downgrade(&win);
        win.split_btn.connect_toggled(move |b| {
            if let Some(w) = w.upgrade() {
                w.toggle_viewer(b.is_active());
            }
        });
        let w = Rc::downgrade(&win);
        vclose.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.split_btn.set_active(false);
            }
        });
        let w = Rc::downgrade(&win);
        win.viewer2_btn.connect_toggled(move |b| {
            if let Some(w) = w.upgrade() {
                w.toggle_viewer2(b.is_active());
            }
        });
        let w = Rc::downgrade(&win);
        vclose2.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.viewer2_btn.set_active(false);
            }
        });
        let w = Rc::downgrade(&win);
        win.window.connect_close_request(move |_| match w.upgrade() {
            Some(w) => w.on_close(),
            None => glib::Propagation::Proceed,
        });
        let w = Rc::downgrade(&win);
        glib::timeout_add_seconds_local(3, move || match w.upgrade() {
            Some(w) => {
                w.autosave();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        win.show_library();
        win
    }

    // ---------------------------------------------------------------- Ansichten

    /// Canvas des aktiven Tabs (oder ein unsichtbarer Ersatz, wenn keiner offen ist).
    pub fn canvas(&self) -> Canvas {
        self.current.borrow().as_ref().map(|v| v.canvas.clone()).unwrap_or_else(|| self.fallback.clone())
    }

    fn view(&self) -> Option<Rc<DocView>> {
        self.current.borrow().clone()
    }

    fn toaster(self: &Rc<Self>) -> Rc<dyn Fn(&str)> {
        let w = Rc::downgrade(self);
        Rc::new(move |m: &str| {
            if let Some(w) = w.upgrade() {
                w.toast(m);
            }
        })
    }

    /// Neuer Tab mit einem Dokument.
    fn add_view(self: &Rc<Self>, doc: Document) -> Rc<DocView> {
        let v = DocView::new(false, self.toaster());
        // Werkzeuge und Einstellungen vom bisherigen Tab übernehmen
        {
            let cur = self.canvas();
            let (tools, cfg) = {
                let st = cur.st();
                (st.tools.clone(), st.cfg.clone())
            };
            let mut st = v.canvas.st();
            st.tools = tools;
            st.cfg = cfg;
        }
        v.canvas.set_document(doc);
        let w = Rc::downgrade(self);
        v.on_claude.replace(Some(Rc::new(move || {
            if let Some(w) = w.upgrade() {
                w.claude.toggle_touch();
            }
        })));
        self.setup_keys(&v);
        let w = Rc::downgrade(self);
        v.canvas.connect_request(move |_, r| {
            if let Some(w) = w.upgrade() {
                w.handle_request(r);
            }
        });
        let page = self.tabs.append(&v.root);
        let w = Rc::downgrade(self);
        let vw = Rc::downgrade(&v);
        let pg = page.clone();
        v.canvas.connect_changed(move |c| {
            let (Some(w), Some(v)) = (w.upgrade(), vw.upgrade()) else { return };
            let (name, dirty) = v.title();
            pg.set_title(&name);
            let dot = gio::ThemedIcon::new("media-record-symbolic");
            pg.set_indicator_icon(if dirty { Some(&dot) } else { None });
            if w.canvas() == *c {
                w.refresh();
            }
        });
        let (name, _) = v.title();
        page.set_title(&name);
        self.views.borrow_mut().push(v.clone());
        self.tabs.set_selected_page(&page);
        self.stack.set_visible_child_name("editor");
        self.on_tab_switched();
        v
    }

    fn view_for_page(&self, page: &adw::TabPage) -> Option<Rc<DocView>> {
        let child = page.child();
        self.views.borrow().iter().find(|v| v.root.upcast_ref::<gtk::Widget>() == &child).cloned()
    }

    fn on_tab_switched(self: &Rc<Self>) {
        let Some(page) = self.tabs.selected_page() else {
            self.current.replace(None);
            self.refresh();
            return;
        };
        let Some(v) = self.view_for_page(&page) else { return };
        // Werkzeugwahl wandert mit
        let prev = self.current.borrow().clone();
        if let Some(prev) = prev {
            if !Rc::ptr_eq(&prev, &v) {
                prev.canvas.commit_edits();
                let (tools, cfg, dark) = {
                    let st = prev.canvas.st();
                    (st.tools.clone(), st.cfg.clone(), st.dark_paper)
                };
                let mut st = v.canvas.st();
                st.tools = tools;
                st.cfg = cfg;
                st.dark_paper = dark;
            }
        }
        self.current.replace(Some(v.clone()));
        self.sidebar.set_canvas(&v.canvas);
        self.apply_tab_viewers();
        self.claude.tab_changed();
        v.canvas.update_cursor();
        v.canvas.grab_focus();
        self.refresh();
    }

    fn on_close_tab(self: &Rc<Self>, tv: &adw::TabView, page: &adw::TabPage) {
        let Some(v) = self.view_for_page(page) else {
            tv.close_page_finish(page, true);
            return;
        };
        v.canvas.commit_edits();
        v.canvas.remember_position();
        let finish = {
            let w = Rc::downgrade(self);
            let vv = v.clone();
            move |ok: bool, page: &adw::TabPage| {
                let Some(w) = w.upgrade() else { return };
                w.tabs.close_page_finish(page, ok);
                if ok {
                    w.views.borrow_mut().retain(|x| !Rc::ptr_eq(x, &vv));
                    if w.current.borrow().as_ref().map(|c| Rc::ptr_eq(c, &vv)).unwrap_or(false) {
                        w.current.replace(None);
                    }
                    if w.tabs.n_pages() == 0 {
                        w.show_library();
                    } else {
                        w.on_tab_switched();
                    }
                }
            }
        };
        if !v.canvas.st().history.dirty() || self.save_now(&v) {
            finish(true, page);
            return;
        }
        let (name, _) = v.title();
        let d = adw::AlertDialog::new(Some(&crate::tr!("„{name}“ speichern?", "Save “{name}”?")), Some(crate::i18n::tl("Ungespeicherte Änderungen gehen sonst verloren.")));
        d.add_responses(&[("cancel", crate::i18n::tl("Abbrechen")), ("discard", crate::i18n::tl("Verwerfen")), ("save", crate::i18n::tl("Speichern"))]);
        d.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("save"));
        let w = self.clone();
        let page = page.clone();
        d.choose(Some(&self.window), gio::Cancellable::NONE, move |r| match r.as_str() {
            "discard" => finish(true, &page),
            "save" => {
                finish(false, &page);
                w.tabs.set_selected_page(&page);
                w.save(false);
            }
            _ => finish(false, &page),
        });
    }

    fn show_library(self: &Rc<Self>) {
        self.library.reload();
        self.stack.set_visible_child_name("library");
        self.refresh();
    }

    /// Beide Lese-Viewer (sofern angelegt).
    fn viewers(&self) -> Vec<Rc<DocView>> {
        self.viewer.borrow().iter().chain(self.viewer2.borrow().iter()).cloned().collect()
    }

    /// Lese-Viewer anlegen (0 = oben, 1 = unten).
    fn ensure_viewer(self: &Rc<Self>, slot: usize) -> Rc<DocView> {
        let (cell, bx) = if slot == 0 { (&self.viewer, &self.viewer_box) } else { (&self.viewer2, &self.viewer2_box) };
        if let Some(v) = cell.borrow().clone() {
            return v;
        }
        let v = DocView::new(true, self.toaster());
        v.root.set_vexpand(true);
        // Papiermodus wie im Editor
        v.canvas.st().dark_paper = self.canvas().st().dark_paper;
        bx.append(&v.root);
        cell.replace(Some(v.clone()));
        v
    }

    fn toggle_viewer(self: &Rc<Self>, on: bool) {
        self.left.set_visible(on);
        if on {
            // Viewer noch leer oder er zeigt das PDF eines anderen Tabs: Datei wählen
            if self.viewer.borrow().is_none() || (!self.restoring_viewers.get() && self.tab_viewer(0).is_none()) {
                self.ensure_viewer(0);
                self.viewer_open_dialog(0);
            }
            let w = self.window.width().max(800);
            if self.paned.position() < 100 {
                self.paned.set_position((w as f64 * 0.42) as i32);
            }
            self.stack.set_visible_child_name("editor");
        }
        self.remember_viewers();
    }

    fn toggle_viewer2(self: &Rc<Self>, on: bool) {
        self.viewer2_box.set_visible(on);
        if on {
            if self.viewer2.borrow().is_none() || (!self.restoring_viewers.get() && self.tab_viewer(1).is_none()) {
                self.ensure_viewer(1);
                self.viewer_open_dialog(1);
            }
            // halbe Höhe; direkt nach dem Start ist die Höhe noch unbekannt
            let w = self.clone();
            glib::idle_add_local_once(move || {
                let h = w.left.height().max(400);
                if w.left.position() < 150 || w.left.position() > h - 150 {
                    w.left.set_position(h / 2);
                }
            });
        }
        self.remember_viewers();
    }

    fn viewer_open_dialog(self: &Rc<Self>, slot: usize) {
        let dialog = self.open_file_dialog(if slot == 0 { crate::i18n::tl("Im Viewer öffnen") } else { crate::i18n::tl("Unten im Viewer öffnen") });
        let w = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res {
                if let Some(p) = f.path() {
                    w.viewer_open(slot, &p);
                }
            }
        });
    }

    /// Viewer öffnen und direkt eine Datei anzeigen (ohne Dialog).
    pub fn show_viewer_with(self: &Rc<Self>, path: &Path) {
        self.ensure_viewer(0);
        self.split_btn.set_active(true);
        self.viewer_open(0, path);
    }

    /// Unteren Viewer öffnen und direkt eine Datei anzeigen (für Tests).
    pub fn show_viewer2_with(self: &Rc<Self>, path: &Path) {
        self.ensure_viewer(1);
        self.viewer2_btn.set_active(true);
        self.viewer_open(1, path);
    }

    fn viewer_open(self: &Rc<Self>, slot: usize, path: &Path) {
        if crate::drive::is_remote(path) {
            self.with_remote(gio::File::for_path(path), move |w, p| w.viewer_open(slot, p));
            return;
        }
        match load_any(path) {
            Ok(mut doc) => {
                // Viewer ist nur zum Lesen: gefundenes PDF nutzen, aber nichts speichern
                if let (_, Some(m)) = crate::pdfsearch::repair(&mut doc) {
                    self.toast(&crate::tr!("Hintergrund-PDF „{}“ nicht gefunden", "Background PDF “{}” not found", file_name(&m)));
                }
                let v = self.ensure_viewer(slot);
                // Leseposition des bisher angezeigten Dokuments sichern
                v.canvas.remember_position();
                v.canvas.set_document(doc);
                let title = if slot == 0 { &self.viewer_title } else { &self.viewer2_title };
                title.set_text(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                self.remember_viewers();
            }
            Err(e) => self.toast(&crate::tr!("Öffnen fehlgeschlagen: {e}", "Opening failed: {e}")),
        }
    }

    /// Dokument, das gerade sichtbar im Lese-Viewer `slot` liegt.
    fn viewer_path(&self, slot: usize) -> Option<PathBuf> {
        let shown = self.split_btn.is_active() && (slot == 0 || self.viewer2_btn.is_active());
        let cell = if slot == 0 { &self.viewer } else { &self.viewer2 };
        let v = cell.borrow().clone().filter(|_| shown)?;
        let key = v.canvas.st().doc_key();
        key
    }

    /// Offene Lese-Viewer beim aktuellen Hauptdokument merken, damit sie beim nächsten
    /// Öffnen der Datei wieder mitkommen.
    fn remember_viewers(&self) {
        if self.restoring_viewers.get() {
            return;
        }
        let Some(main) = self.view().and_then(|v| v.canvas.st().doc_key()) else { return };
        crate::positions::set_viewers(&main, [self.viewer_path(0), self.viewer_path(1)]);
    }

    /// Für den aktiven Tab gemerkter Lese-Viewer `slot`.
    fn tab_viewer(&self, slot: usize) -> Option<PathBuf> {
        let main = self.view().and_then(|v| v.canvas.st().doc_key())?;
        crate::positions::viewers(&main)[slot].clone()
    }

    /// Tab `i` auswählen (für Tests).
    pub fn select_tab(&self, i: usize) {
        if (i as i32) < self.tabs.n_pages() {
            self.tabs.set_selected_page(&self.tabs.nth_page(i as i32));
        }
    }

    /// Lese-Viewer `slot`, falls er gerade zu sehen ist.
    fn visible_viewer(&self, slot: usize) -> Option<Rc<DocView>> {
        self.viewer_path(slot)?;
        if slot == 0 { self.viewer.borrow().clone() } else { self.viewer2.borrow().clone() }
    }

    /// Datei, die im Lese-Viewer `slot` geladen ist (auch wenn er gerade verborgen ist).
    fn viewer_loaded(&self, slot: usize) -> Option<PathBuf> {
        let cell = if slot == 0 { &self.viewer } else { &self.viewer2 };
        let v = cell.borrow().clone()?;
        let key = v.canvas.st().doc_key();
        key
    }

    /// Lese-Viewer des aktiven Tabs zeigen: jeder Tab hat seine eigenen (gemerkt pro
    /// Hauptdatei). Tabs ohne gemerkte Viewer blenden sie aus; neue, noch unbenannte
    /// Dokumente lassen die Viewer, wie sie sind.
    fn apply_tab_viewers(self: &Rc<Self>) {
        let Some(main) = self.view().and_then(|v| v.canvas.st().doc_key()) else { return };
        let [top, bottom] = crate::positions::viewers(&main);
        self.restoring_viewers.set(true);
        match top.filter(|p| p.exists()) {
            Some(t) => {
                if self.viewer_loaded(0).as_deref() != Some(t.as_path()) {
                    self.viewer_open(0, &t);
                }
                self.split_btn.set_active(true);
                match bottom.filter(|p| p.exists()) {
                    Some(b) => {
                        if self.viewer_loaded(1).as_deref() != Some(b.as_path()) {
                            self.viewer_open(1, &b);
                        }
                        self.viewer2_btn.set_active(true);
                    }
                    None => self.viewer2_btn.set_active(false),
                }
            }
            None => {
                self.viewer2_btn.set_active(false);
                self.split_btn.set_active(false);
            }
        }
        self.restoring_viewers.set(false);
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE Viewer für {}: oben {:?}, unten {:?}", file_name(&main), self.viewer_path(0), self.viewer_path(1));
        }
    }

    // ---------------------------------------------------------------- Dock

    fn build_dock(self: &Rc<Self>) {
        let tools = [Tool::Pen, Tool::Highlighter, Tool::Eraser, Tool::Select, Tool::Shape, Tool::Text, Tool::Space, Tool::Ask];
        let mut group: Option<gtk::ToggleButton> = None;
        for t in tools {
            let b = gtk::ToggleButton::new();
            b.set_icon_name(t.icon());
            b.set_tooltip_text(Some(t.label()));
            b.add_css_class("flat");
            b.add_css_class("fk-tool");
            if let Some(g) = &group {
                b.set_group(Some(g));
            } else {
                group = Some(b.clone());
            }
            // zweiter Tipp auf das aktive Werkzeug öffnet seine Optionen
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let w = Rc::downgrade(self);
            let bb = b.clone();
            click.connect_pressed(move |_, _, _, _| {
                if let Some(w) = w.upgrade() {
                    if bb.is_active() && w.canvas().st().tools.tool == t {
                        w.show_tool_options(&bb, t);
                    }
                }
            });
            b.add_controller(click);
            let w = Rc::downgrade(self);
            b.connect_toggled(move |b| {
                if b.is_active() {
                    if let Some(w) = w.upgrade() {
                        if w.canvas().st().tools.tool != t {
                            w.canvas().set_tool(t);
                        }
                    }
                }
            });
            self.dock.append(&b);
            self.tool_buttons.borrow_mut().push((t, b));
        }
        // Einfügen aus der Zwischenablage (Bilder, Text, kopierte Striche)
        let paste = gtk::Button::from_icon_name("edit-paste-symbolic");
        paste.add_css_class("flat");
        paste.set_tooltip_text(Some(crate::i18n::tl("Einfügen (Strg+V)")));
        paste.set_action_name(Some("win.paste"));
        self.dock.append(&paste);
        let claude = gtk::Button::from_icon_name("fk-spark-symbolic");
        claude.add_css_class("flat");
        claude.add_css_class("fk-claude-spark");
        claude.set_tooltip_text(Some(crate::i18n::tl("Claude fragen (Esc)")));
        let w = Rc::downgrade(self);
        claude.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.claude.toggle_touch();
            }
        });
        self.dock.append(&claude);
        self.dock.append(&gtk::Separator::new(gtk::Orientation::Vertical));

        for i in QUICK_COLORS {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.add_css_class("fk-color");
            b.set_tooltip_text(Some(crate::i18n::tl(COLOR_NAMES[i])));
            let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            sw.add_css_class("fk-swatch");
            sw.add_css_class(&format!("fk-c{i}"));
            sw.set_halign(gtk::Align::Center);
            sw.set_valign(gtk::Align::Center);
            b.set_child(Some(&sw));
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_color(PALETTE[i]);
                }
            });
            self.dock.append(&b);
            self.color_buttons.borrow_mut().push((i, b));
        }
        let more = gtk::MenuButton::new();
        more.set_icon_name("color-select-symbolic");
        more.set_tooltip_text(Some(crate::i18n::tl("Weitere Farben")));
        more.add_css_class("flat");
        more.set_direction(gtk::ArrowType::Up);
        let pop = gtk::Popover::new();
        let grid = gtk::Grid::builder().row_spacing(6).column_spacing(6).css_classes(["fk-options"]).build();
        for (i, _) in PALETTE.iter().enumerate() {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.set_tooltip_text(Some(crate::i18n::tl(COLOR_NAMES[i])));
            let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            sw.add_css_class("fk-swatch");
            sw.add_css_class(&format!("fk-c{i}"));
            b.set_child(Some(&sw));
            let w = Rc::downgrade(self);
            let pop2 = pop.clone();
            b.connect_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_color(PALETTE[i]);
                }
                pop2.popdown();
            });
            grid.attach(&b, (i % 5) as i32, (i / 5) as i32, 1, 1);
        }
        let custom = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
        let w = Rc::downgrade(self);
        custom.connect_rgba_notify(move |b| {
            if let Some(w) = w.upgrade() {
                w.set_color(crate::geom::Color::from_gdk(&b.rgba()).with_alpha(1.0));
            }
        });
        grid.attach(&custom, 0, (PALETTE.len() / 5) as i32, 5, 1);
        pop.set_child(Some(&grid));
        more.set_popover(Some(&pop));
        self.dock.append(&more);
        self.dock.append(&gtk::Separator::new(gtk::Orientation::Vertical));

        for (i, d) in [3, 5, 8, 12].into_iter().enumerate() {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.add_css_class("fk-width");
            b.set_tooltip_text(Some([crate::i18n::tl("Sehr fein"), crate::i18n::tl("Fein"), crate::i18n::tl("Mittel"), crate::i18n::tl("Dick")][i]));
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("fk-dot");
            dot.set_size_request(d, d);
            dot.set_halign(gtk::Align::Center);
            dot.set_valign(gtk::Align::Center);
            b.set_child(Some(&dot));
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.canvas().st().tools.set_width_level(i);
                    w.canvas().changed();
                }
            });
            self.dock.append(&b);
            self.width_buttons.borrow_mut().push(b);
        }
        self.dock.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        for (icon, action, tip) in [
            ("edit-undo-symbolic", "win.undo", crate::i18n::tl("Rückgängig (Strg+Z)")),
            ("edit-redo-symbolic", "win.redo", crate::i18n::tl("Wiederholen (Strg+Y)")),
        ] {
            let b = gtk::Button::from_icon_name(icon);
            b.add_css_class("flat");
            b.set_action_name(Some(action));
            b.set_tooltip_text(Some(tip));
            self.dock.append(&b);
        }
    }

    fn set_color(&self, c: crate::geom::Color) {
        if self.canvas().recolor_selection(c) {
            self.canvas().st().tools.pen.color = c;
            return;
        }
        let cv = self.canvas();
        {
            let mut st = cv.st();
            if !st.tools.tool.uses_pen() {
                st.tools.tool = Tool::Pen;
            }
            st.tools.set_color(c);
        }
        let tool = self.canvas().st().tools.tool;
        self.canvas().set_tool(tool);
    }

    fn show_tool_options(self: &Rc<Self>, anchor: &gtk::ToggleButton, t: Tool) {
        let pop = gtk::Popover::new();
        pop.set_parent(anchor);
        pop.set_position(gtk::PositionType::Top);
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
        bx.add_css_class("fk-options");
        let tools = self.canvas().st().tools.clone();
        match t {
            Tool::Pen => {
                let cfg = self.canvas().st().cfg.clone();
                for (label, active, f) in [
                    (crate::i18n::tl("Druckempfindlich"), cfg.pressure, (|c: &mut crate::settings::Settings, v| c.pressure = v) as fn(&mut crate::settings::Settings, bool)),
                    (crate::i18n::tl("Halten zum Begradigen"), cfg.hold_to_straighten, |c, v| c.hold_to_straighten = v),
                ] {
                    let c = gtk::CheckButton::with_label(label);
                    c.set_active(active);
                    let w = Rc::downgrade(self);
                    c.connect_toggled(move |c| {
                        if let Some(w) = w.upgrade() {
                            let mut cfg = w.canvas().st().cfg.clone();
                            f(&mut cfg, c.is_active());
                            w.canvas().apply_settings(cfg);
                        }
                    });
                    bx.append(&c);
                }
                bx.append(&self.auto_shapes_check());
                let more = gtk::Button::with_label(crate::i18n::tl("Alle Stift-Einstellungen…"));
                more.add_css_class("flat");
                more.set_action_name(Some("win.preferences"));
                bx.append(&more);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                row.add_css_class("linked");
                let mut first: Option<gtk::ToggleButton> = None;
                for (label, style) in
                    [("—", LineStyle::Plain), ("- -", LineStyle::Dash), ("···", LineStyle::Dot), ("-·-", LineStyle::DashDot)]
                {
                    let b = gtk::ToggleButton::with_label(label);
                    b.set_active(tools.pen.style == style);
                    if let Some(f) = &first {
                        b.set_group(Some(f));
                    } else {
                        first = Some(b.clone());
                    }
                    let w = Rc::downgrade(self);
                    b.connect_toggled(move |b| {
                        if b.is_active() {
                            if let Some(w) = w.upgrade() {
                                w.canvas().st().tools.pen.style = style;
                            }
                        }
                    });
                    row.append(&b);
                }
                bx.append(&row);
            }
            Tool::Highlighter => {
                let c = gtk::CheckButton::with_label(crate::i18n::tl("An PDF-Text ausrichten"));
                c.set_tooltip_text(Some(crate::i18n::tl("Über PDF-Text markiert der Marker exakt die Textzeilen – wie in einem PDF-Reader.")));
                c.set_active(tools.hl_snap);
                let w = Rc::downgrade(self);
                c.connect_toggled(move |c| {
                    if let Some(w) = w.upgrade() {
                        w.canvas().st().tools.hl_snap = c.is_active();
                    }
                });
                bx.append(&c);
            }
            Tool::Eraser => {
                let a = gtk::CheckButton::with_label(crate::i18n::tl("Teile von Strichen radieren"));
                let b = gtk::CheckButton::with_label(crate::i18n::tl("Ganze Striche löschen"));
                b.set_group(Some(&a));
                a.set_active(tools.eraser_mode == EraserMode::Standard);
                b.set_active(tools.eraser_mode == EraserMode::Stroke);
                let w = Rc::downgrade(self);
                a.connect_toggled(move |a| {
                    if let Some(w) = w.upgrade() {
                        w.canvas().st().tools.eraser_mode = if a.is_active() { EraserMode::Standard } else { EraserMode::Stroke };
                    }
                });
                bx.append(&a);
                bx.append(&b);
            }
            Tool::Select => {
                let mut first: Option<gtk::CheckButton> = None;
                for (label, mode) in [(crate::i18n::tl("Lasso (frei umranden)"), SelectMode::Lasso), (crate::i18n::tl("Rechteck"), SelectMode::Rect)] {
                    let c = gtk::CheckButton::with_label(label);
                    if let Some(f) = &first { c.set_group(Some(f)) } else { first = Some(c.clone()) }
                    c.set_active(tools.select_mode == mode);
                    let w = Rc::downgrade(self);
                    c.connect_toggled(move |c| {
                        if c.is_active() {
                            if let Some(w) = w.upgrade() {
                                w.canvas().st().tools.select_mode = mode;
                            }
                        }
                    });
                    bx.append(&c);
                }
                let l = gtk::Label::new(Some(crate::i18n::tl("Tippen wählt ein einzelnes Objekt.\nStifttaste 2 wählt immer per Lasso.")));
                l.add_css_class("dim-label");
                l.add_css_class("caption");
                bx.append(&l);
            }
            Tool::Shape => {
                let grid = gtk::Grid::builder().row_spacing(4).column_spacing(4).build();
                let icons = ["fk-shape-line", "fk-shape-arrow", "fk-shape-darrow", "fk-shape-rect", "fk-shape-ellipse", "fk-shape-axes"];
                let mut first: Option<gtk::ToggleButton> = None;
                for (i, kind) in ShapeKind::ALL.iter().enumerate() {
                    let b = gtk::ToggleButton::new();
                    b.set_icon_name(&format!("{}-symbolic", icons[i]));
                    b.set_tooltip_text(Some(kind.label()));
                    if let Some(f) = &first { b.set_group(Some(f)) } else { first = Some(b.clone()) }
                    b.set_active(tools.shape == *kind);
                    let w = Rc::downgrade(self);
                    let k = *kind;
                    b.connect_toggled(move |b| {
                        if b.is_active() {
                            if let Some(w) = w.upgrade() {
                                w.canvas().st().tools.shape = k;
                            }
                        }
                    });
                    grid.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
                }
                bx.append(&grid);
                let fill = gtk::CheckButton::with_label(crate::i18n::tl("Gefüllt"));
                fill.set_active(tools.pen.fill);
                let w = Rc::downgrade(self);
                fill.connect_toggled(move |c| {
                    if let Some(w) = w.upgrade() {
                        w.canvas().st().tools.pen.fill = c.is_active();
                    }
                });
                bx.append(&fill);
                bx.append(&self.auto_shapes_check());
            }
            Tool::Text => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                row.append(&gtk::Label::new(Some(crate::i18n::tl("Größe"))));
                let spin = gtk::SpinButton::with_range(6.0, 72.0, 1.0);
                spin.set_value(tools.text_size);
                let w = Rc::downgrade(self);
                spin.connect_value_changed(move |s| {
                    if let Some(w) = w.upgrade() {
                        w.canvas().st().tools.text_size = s.value();
                    }
                });
                row.append(&spin);
                bx.append(&row);
                let fd = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
                fd.set_use_size(false);
                fd.set_font_desc(&gtk::pango::FontDescription::from_string(&tools.text_font));
                let w = Rc::downgrade(self);
                fd.connect_font_desc_notify(move |b| {
                    if let (Some(w), Some(d)) = (w.upgrade(), b.font_desc()) {
                        if let Some(f) = d.family() {
                            w.canvas().st().tools.text_font = f.to_string();
                        }
                    }
                });
                bx.append(&fd);
            }
            _ => {
                bx.append(&gtk::Label::new(Some(t.label())));
            }
        }
        pop.set_child(Some(&bx));
        pop.connect_closed(|p| p.unparent());
        pop.popup();
    }

    fn auto_shapes_check(self: &Rc<Self>) -> gtk::CheckButton {
        let c = gtk::CheckButton::with_label(crate::i18n::tl("Formen beim Absetzen erkennen"));
        c.set_tooltip_text(Some(crate::i18n::tl("Kreise, Rechtecke, Dreiecke und Linien werden automatisch sauber.\nUnabhängig davon: Stift kurz still halten erkennt die Form immer.")));
        c.set_active(self.canvas().st().cfg.auto_shapes);
        let w = Rc::downgrade(self);
        c.connect_toggled(move |c| {
            if let Some(w) = w.upgrade() {
                let mut cfg = w.canvas().st().cfg.clone();
                cfg.auto_shapes = c.is_active();
                w.canvas().apply_settings(cfg);
            }
        });
        c
    }

    fn handle_request(self: &Rc<Self>, r: crate::canvas::Request) {
        match r {
            crate::canvas::Request::Paste => {
                if let Some(v) = self.view() {
                    v.paste();
                }
            }
            crate::canvas::Request::Claude => self.claude.toggle_touch(),
            crate::canvas::Request::ClaudeRegion(png, page, area) => self.claude.ask_region(&png, page, area),
        }
    }

    /// Aktualisiert Titel, Dock-Zustand usw. nach Änderungen im aktiven Canvas.
    pub fn refresh(&self) {
        if self.stack.visible_child_name().as_deref() == Some("library") && self.current.borrow().is_none() {
            self.title.set_title("Graphit");
            self.title.set_subtitle(crate::i18n::tl("Bibliothek"));
            return;
        }
        let Some(v) = self.view() else {
            self.title.set_title("Graphit");
            self.title.set_subtitle("");
            return;
        };
        let (name, dirty) = v.title();
        let canvas = v.canvas.clone();
        let st = canvas.st();
        self.title.set_title(&format!("{}{}", if dirty { "• " } else { "" }, name));
        let n = st.doc.pages.len();
        let cur = st.current_page() + 1;
        self.title.set_subtitle(&crate::tr!("Seite {cur} von {n} · {:.0} %", "Page {cur} of {n} · {:.0} %", st.zoom / ZOOM_100 * 100.0));
        let tool = st.tools.tool;
        let level = st.tools.width_level();
        let color = match tool {
            Tool::Highlighter => st.tools.highlighter.color,
            _ => st.tools.pen.color,
        };
        let can_undo = st.history.can_undo();
        let can_redo = st.history.can_redo();
        drop(st);
        for (t, b) in self.tool_buttons.borrow().iter() {
            if *t == tool && !b.is_active() {
                b.set_active(true);
            }
        }
        for (i, b) in self.color_buttons.borrow().iter() {
            let c = PALETTE[*i];
            let same = (c.r - color.r).abs() < 0.01 && (c.g - color.g).abs() < 0.01 && (c.b - color.b).abs() < 0.01;
            if same && tool.uses_pen() { b.add_css_class("active") } else { b.remove_css_class("active") }
        }
        for (i, b) in self.width_buttons.borrow().iter().enumerate() {
            if i == level { b.add_css_class("active") } else { b.remove_css_class("active") }
        }
        self.action_enabled("undo", can_undo);
        self.action_enabled("redo", can_redo);
        if self.split.shows_sidebar() {
            self.sidebar.refresh();
        }
    }

    fn action_enabled(&self, name: &str, on: bool) {
        if let Some(a) = self.window.lookup_action(name) {
            if let Some(a) = a.downcast_ref::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        }
    }

    // ---------------------------------------------------------------- Aktionen

    fn setup_actions(self: &Rc<Self>, app: &adw::Application) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<Win>)>| {
            let a = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = w.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&a);
        };
        add("new", Box::new(|w| {
            let style = w.canvas().st().cfg.paper_style.clone();
            w.add_view(Document::new_blank(&style));
        }));
        add("open", Box::new(|w| w.open_dialog()));
        add("save", Box::new(|w| w.save(false)));
        add("save-as", Box::new(|w| w.save(true)));
        add("export-pdf", Box::new(|w| w.export_pdf()));
        add("library", Box::new(|w| w.show_library()));
        add("viewer-open", Box::new(|w| w.viewer_open_dialog(0)));
        add("viewer2-open", Box::new(|w| w.viewer_open_dialog(1)));
        add("toggle-sidebar", Box::new(|w| w.sidebar_btn.set_active(!w.sidebar_btn.is_active())));
        add("toggle-split", Box::new(|w| w.split_btn.set_active(!w.split_btn.is_active())));
        add("close-tab", Box::new(|w| {
            if let Some(p) = w.tabs.selected_page() {
                w.tabs.close_page(&p);
            }
        }));
        add("preferences", Box::new(|w| crate::prefs::show(&w.window, &w.canvas())));
        add("choose-pdf", Box::new(|w| w.choose_pdf_dialog()));
        add("claude", Box::new(|w| w.claude.toggle_touch()));
        add(
            "unlock-page",
            Box::new(|w| {
                let n = w.canvas().unlock_page();
                w.toast(&match n {
                    0 => crate::i18n::tl("Auf dieser Seite ist nichts fixiert").to_string(),
                    1 => crate::i18n::tl("1 Objekt wieder auswählbar").to_string(),
                    n => crate::tr!("{n} Objekte wieder auswählbar", "{n} objects can be selected again"),
                });
            }),
        );
        add("find", Box::new(|w| {
            // Suche dort, wo der Fokus ist (Viewer oder Editor)
            let viewer = w.viewers().into_iter().find(|v| {
                v.root.is_visible()
                    && w.left.is_visible()
                    && gtk::prelude::GtkWindowExt::focus(&w.window).map(|f| f.is_ancestor(&v.root) || f == *v.canvas.upcast_ref::<gtk::Widget>()).unwrap_or(false)
            });
            match viewer.or_else(|| w.view()) {
                Some(v) => v.show_search(),
                None => {}
            }
        }));
        add("paste", Box::new(|w| {
            if let Some(v) = w.view() {
                v.paste();
            }
        }));
        // Papier der aktuellen Seite (Parameter: graph, ruled, lined, dotted, plain)
        let paper = gio::SimpleAction::new("page-paper", Some(glib::VariantTy::STRING));
        let ww = Rc::downgrade(self);
        paper.connect_activate(move |_, p| {
            let (Some(w), Some(style)) = (ww.upgrade(), p.and_then(|p| p.get::<String>())) else { return };
            let c = w.canvas();
            let i = c.st().current_page();
            c.set_page_style(i, &style);
        });
        self.window.add_action(&paper);
        add("undo", Box::new(|w| {
            let c = w.canvas();
            if c.st().text.is_none() {
                c.undo()
            }
        }));
        add("redo", Box::new(|w| {
            let c = w.canvas();
            if c.st().text.is_none() {
                c.redo()
            }
        }));
        add("zoom-in", Box::new(|w| w.canvas().zoom_by(1.2)));
        add("zoom-out", Box::new(|w| w.canvas().zoom_by(1.0 / 1.2)));
        add("zoom-fit", Box::new(|w| w.canvas().fit_width()));
        add("zoom-100", Box::new(|w| {
            let c = w.canvas();
            let z = c.st().zoom;
            c.zoom_by(ZOOM_100 / z);
        }));
        add("fullscreen", Box::new(|w| {
            if w.window.is_fullscreen() { w.window.unfullscreen() } else { w.window.fullscreen() }
        }));
        let dark_on = self.canvas().st().cfg.dark_paper;
        let dark = gio::SimpleAction::new_stateful("dark-paper", None, &dark_on.to_variant());
        let w = Rc::downgrade(self);
        dark.connect_activate(move |a, _| {
            let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            if let Some(w) = w.upgrade() {
                let views: Vec<_> = w.views.borrow().iter().cloned().chain(w.viewers()).collect();
                for v in views {
                    let mut st = v.canvas.st();
                    st.dark_paper = on;
                    st.cfg.dark_paper = on;
                    drop(st);
                    // Auswahl wird passend zum Papier umgefärbt gerendert
                    v.canvas.render_selection();
                    v.canvas.queue_draw();
                }
                // merken (auch für die anderen Tabs, die ihre Einstellungen kopieren)
                let mut cfg = crate::settings::Settings::load();
                cfg.dark_paper = on;
                cfg.save();
            }
        });
        self.window.add_action(&dark);

        app.set_accels_for_action("win.new", &["<Control>n"]);
        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("win.save", &["<Control>s"]);
        app.set_accels_for_action("win.save-as", &["<Control><Shift>s"]);
        app.set_accels_for_action("win.export-pdf", &["<Control>e"]);
        app.set_accels_for_action("win.undo", &["<Control>z"]);
        app.set_accels_for_action("win.redo", &["<Control>y", "<Control><Shift>z"]);
        app.set_accels_for_action("win.zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"]);
        app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        app.set_accels_for_action("win.zoom-100", &["<Control>0"]);
        app.set_accels_for_action("win.zoom-fit", &["<Control>9"]);
        app.set_accels_for_action("win.fullscreen", &["F11"]);
        app.set_accels_for_action("win.dark-paper", &["<Control><Shift>d"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.toggle-sidebar", &["F9"]);
        app.set_accels_for_action("win.toggle-split", &["F8"]);
        app.set_accels_for_action("win.close-tab", &["<Control>w"]);
        app.set_accels_for_action("win.library", &["<Control>l"]);
        app.set_accels_for_action("win.find", &["<Control>f"]);
    }

    fn setup_keys(self: &Rc<Self>, view: &Rc<DocView>) {
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        let vw = Rc::downgrade(view);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let (Some(w), Some(v)) = (w.upgrade(), vw.upgrade()) else { return glib::Propagation::Proceed };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let c = &v.canvas;
            if ctrl {
                match key.to_lower() {
                    gdk::Key::c => v.copy(false),
                    gdk::Key::x => v.copy(true),
                    gdk::Key::v => v.paste(),
                    gdk::Key::d => c.duplicate_selection(),
                    gdk::Key::a => c.select_all(),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            if mods.contains(gdk::ModifierType::ALT_MASK) || mods.contains(gdk::ModifierType::SUPER_MASK) {
                return glib::Propagation::Proceed;
            }
            let step = if shift { 10.0 } else { 1.0 };
            let tool = match key.to_lower() {
                gdk::Key::Delete | gdk::Key::BackSpace => {
                    c.delete_selection();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Escape => {
                    let open = c.has_selection() || c.st().text.is_some();
                    if !open {
                        return glib::Propagation::Proceed;
                    }
                    c.commit_edits();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down if c.has_selection() => {
                    let (dx, dy) = match key {
                        gdk::Key::Left => (-step, 0.0),
                        gdk::Key::Right => (step, 0.0),
                        gdk::Key::Up => (0.0, -step),
                        _ => (0.0, step),
                    };
                    c.nudge_selection(dx, dy);
                    return glib::Propagation::Stop;
                }
                gdk::Key::p => Tool::Pen,
                gdk::Key::m | gdk::Key::h => Tool::Highlighter,
                gdk::Key::e => Tool::Eraser,
                gdk::Key::s | gdk::Key::l => Tool::Select,
                gdk::Key::f | gdk::Key::r => Tool::Shape,
                gdk::Key::t => Tool::Text,
                gdk::Key::space => Tool::Hand,
                gdk::Key::Page_Down | gdk::Key::Page_Up => {
                    let i = c.st().current_page();
                    let n = if key == gdk::Key::Page_Down { i + 1 } else { i.saturating_sub(1) };
                    c.scroll_to(n);
                    return glib::Propagation::Stop;
                }
                _ => return glib::Propagation::Proceed,
            };
            c.set_tool(tool);
            let _ = &w;
            glib::Propagation::Stop
        });
        view.canvas.add_controller(keys);
    }

    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Hinweis auf eine neue Version mit Knopf zur Download-Seite.
    pub fn offer_update(&self, version: &str, url: &str) {
        let toast = adw::Toast::builder().title(crate::tr!("Graphit {version} ist verfügbar", "Graphit {version} is available")).button_label(crate::i18n::tl("Ansehen")).timeout(12).build();
        let (window, url) = (self.window.clone(), url.to_string());
        toast.connect_button_clicked(move |_| {
            gtk::UriLauncher::new(&url).launch(Some(&window), gio::Cancellable::NONE, |_| {});
        });
        self.toasts.add_toast(toast);
    }

    // ---------------------------------------------------------------- Dateien

    /// Öffnet eine Datei in einem Tab (oder wechselt zum Tab, falls schon offen).
    /// Datei von einem Online-Konto: lokale Kopie laden, danach wie eine normale Datei.
    fn with_remote(self: &Rc<Self>, file: gio::File, then: impl FnOnce(&Rc<Self>, &Path) + 'static) {
        // bleibt, bis die Datei da ist; Meldungen stünden sonst dahinter in der Schlange
        let loading = adw::Toast::builder().title(crate::i18n::tl("Lädt die Datei …")).timeout(0).build();
        self.toasts.add_toast(loading.clone());
        let (w, uri) = (self.clone(), file.uri().to_string());
        glib::spawn_future_local(async move {
            let fetched = gio::spawn_blocking(move || crate::drive::fetch(&uri)).await;
            loading.dismiss();
            match fetched {
                Ok(Ok(crate::drive::Remote::File { path, note, stale })) => {
                    then(&w, &path);
                    if stale {
                        w.toast(crate::i18n::tl("Das Original ist neuer: Graphit zeigt die vorhandene lokale Kopie"));
                    } else if note {
                        w.toast(crate::i18n::tl("Lokale Kopie: Änderungen gehen nicht ins Original zurück"));
                    }
                }
                Ok(Ok(crate::drive::Remote::Folder)) => w.open_dialog_in(Some(file)),
                Ok(Err(e)) => w.toast(&e),
                Err(_) => w.toast(crate::i18n::tl("Laden fehlgeschlagen")),
            }
        });
    }

    pub fn open_remote(self: &Rc<Self>, file: gio::File) {
        self.with_remote(file, |w, p| w.open_path(p));
    }

    pub fn open_path(self: &Rc<Self>, path: &Path) {
        if crate::drive::is_remote(path) {
            self.open_remote(gio::File::for_path(path));
            return;
        }
        let target = canonical(path);
        let existing = self.views.borrow().iter().find(|v| {
            let st = v.canvas.st();
            st.doc.path.as_deref().map(canonical) == Some(target.clone())
                || (st.doc.path.is_none() && st.doc.pdf_path.as_deref().map(canonical) == Some(target.clone()))
                || st.doc.path.as_deref().map(canonical) == Some(canonical(&path.with_extension("xopp")))
        }).cloned();
        if let Some(v) = existing {
            let page = self.tabs.page(&v.root);
            self.tabs.set_selected_page(&page);
            self.stack.set_visible_child_name("editor");
            return;
        }
        match load_any(path) {
            Ok(mut doc) => {
                let (found, missing) = crate::pdfsearch::repair(&mut doc);
                let v = match self.view().filter(|v| v.is_pristine()) {
                    Some(v) => {
                        v.canvas.set_document(doc);
                        self.stack.set_visible_child_name("editor");
                        self.on_tab_switched();
                        v
                    }
                    None => self.add_view(doc),
                };
                if let Some(f) = found {
                    // neuen Pfad gleich in die .xopp schreiben
                    self.save_now(&v);
                    self.pdf_toast(&crate::tr!("Hintergrund-PDF gefunden: {}", "Background PDF found: {}", file_name(&f)), crate::i18n::tl("Anderes wählen…"));
                }
                if let Some(m) = missing {
                    self.pdf_toast(&crate::tr!("Hintergrund-PDF „{}“ nicht gefunden", "Background PDF “{}” not found", file_name(&m)), crate::i18n::tl("PDF wählen…"));
                }
                crate::recent::add(path);
                self.apply_tab_viewers();
            }
            Err(e) => self.toast(&crate::tr!("Öffnen fehlgeschlagen: {e}", "Opening failed: {e}")),
        }
    }

    fn pdf_toast(&self, msg: &str, button: &str) {
        let t = adw::Toast::builder().title(msg).button_label(button).action_name("win.choose-pdf").timeout(10).build();
        self.toasts.add_toast(t);
    }

    /// Hintergrund-PDF der aktuellen Notiz von Hand wählen.
    fn choose_pdf_dialog(self: &Rc<Self>) {
        let Some(v) = self.view() else { return };
        let (needed, dir) = {
            let st = v.canvas.st();
            (crate::pdfsearch::pages_needed(&st.doc), st.doc.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())))
        };
        if needed == 0 {
            self.toast(crate::i18n::tl("Diese Notiz hat keinen PDF-Hintergrund"));
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PDF"));
        filter.add_pattern("*.pdf");
        filter.add_pattern("*.PDF");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(crate::i18n::tl("Hintergrund-PDF wählen")).filters(&filters).modal(true).build();
        if let Some(d) = dir.or_else(|| self.current_dir()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(d)));
        }
        let w = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Some(p) = res.ok().and_then(|f| f.path()) else { return };
            let n = render::pdf_page_sizes(&p).map(|s| s.len()).unwrap_or(0);
            if n == 0 {
                w.toast(crate::i18n::tl("Keine lesbare PDF-Datei"));
                return;
            }
            v.canvas.set_pdf_path(p.clone());
            w.save_now(&v);
            if n < needed {
                w.toast(&crate::tr!("{} hat nur {n} Seiten, die Notiz nutzt {needed}", "{} has only {n} pages, the note uses {needed}", file_name(&p)));
            } else {
                w.toast(&crate::tr!("Hintergrund-PDF: {}", "Background PDF: {}", file_name(&p)));
            }
        });
    }

    fn open_file_dialog(&self, title: &str) -> gtk::FileDialog {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(crate::i18n::tl("Notizen und PDFs")));
        filter.add_pattern("*.xopp");
        filter.add_pattern("*.xoj");
        filter.add_pattern("*.pdf");
        filter.add_pattern("*.PDF");
        // Online-Konten nennen ihre Dateien anders als angezeigt
        filter.add_mime_type("application/pdf");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(title).filters(&filters).modal(true).build();
        if let Some(dir) = self.current_dir() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        dialog
    }

    fn open_dialog(self: &Rc<Self>) {
        self.open_dialog_in(None);
    }

    fn open_dialog_in(self: &Rc<Self>, folder: Option<gio::File>) {
        let dialog = self.open_file_dialog(crate::i18n::tl("Öffnen"));
        if let Some(f) = folder {
            dialog.set_initial_folder(Some(&f));
        }
        let w = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res {
                match f.path() {
                    Some(p) => w.open_path(&p),
                    None => w.open_remote(f),
                }
            }
        });
    }

    fn current_dir(&self) -> Option<PathBuf> {
        let c = self.canvas();
        let st = c.st();
        st.doc.path.as_ref().or(st.doc.pdf_path.as_ref()).and_then(|p| p.parent().map(|p| p.to_path_buf())).or_else(|| {
            let d = library_dir();
            d.exists().then_some(d)
        })
    }

    pub fn save(self: &Rc<Self>, ask: bool) {
        let Some(v) = self.view() else { return };
        let path = v.canvas.st().doc.path.clone();
        match path {
            Some(p) if !ask => self.save_view_to(&v, p),
            _ => {
                let suggestion = {
                    let st = v.canvas.st();
                    st.doc
                        .pdf_path
                        .as_ref()
                        .map(|p| p.with_extension("xopp"))
                        .or_else(|| st.doc.path.clone())
                        .unwrap_or_else(|| PathBuf::from(crate::i18n::tl("Notiz.xopp")))
                };
                let dialog = gtk::FileDialog::builder().title(crate::i18n::tl("Speichern unter")).modal(true).build();
                if let Some(name) = suggestion.file_name() {
                    dialog.set_initial_name(Some(&name.to_string_lossy()));
                }
                if let Some(dir) = suggestion.parent().filter(|d| d.exists()).map(|d| d.to_path_buf()).or_else(|| self.current_dir()) {
                    dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
                }
                let w = self.clone();
                dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
                    if let Ok(f) = res {
                        if let Some(mut p) = f.path() {
                            if p.extension().is_none() {
                                p.set_extension("xopp");
                            }
                            w.save_view_to(&v, p);
                        }
                    }
                });
            }
        }
    }

    /// Speichert im Hintergrund-Thread; die UI blockiert nie.
    fn save_view_to(self: &Rc<Self>, v: &Rc<DocView>, path: PathBuf) {
        v.canvas.commit_edits();
        let (data, serial) = {
            let st = v.canvas.st();
            (xopp::SaveData::from_doc(&st.doc), st.history.serial)
        };
        let w = self.clone();
        let v = v.clone();
        glib::spawn_future_local(async move {
            let p2 = path.clone();
            let res = gio::spawn_blocking(move || xopp::save(&data, &p2)).await;
            match res {
                Ok(Ok(())) => {
                    {
                        let mut st = v.canvas.st();
                        st.doc.path = Some(path.clone());
                        st.history.saved_at = serial;
                    }
                    crate::recent::add(&path);
                    remove_autosave(&path);
                    v.canvas.changed();
                    w.toast(crate::i18n::tl("Gespeichert"));
                }
                Ok(Err(e)) => w.toast(&crate::tr!("Speichern fehlgeschlagen: {e}", "Saving failed: {e}")),
                Err(_) => w.toast(crate::i18n::tl("Speichern fehlgeschlagen")),
            }
        });
    }

    /// Automatisch speichern, sobald nach einer Änderung kurz Ruhe ist: Dokumente mit
    /// Dateinamen direkt in ihre Datei, unbenannte als Sicherung im Cache.
    fn autosave(&self) {
        let t0 = std::time::Instant::now();
        self.autosave_inner();
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE autosave-Takt {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn autosave_inner(&self) {
        for v in self.views.borrow().iter().cloned().chain(self.viewers()) {
            v.canvas.remember_position();
        }
        for v in self.views.borrow().iter() {
            let (data, serial, direct) = {
                let st = v.canvas.st();
                let serial = st.history.serial;
                if !st.history.dirty() || v.autosaved_serial.get() == serial || v.autosaving.get() {
                    continue;
                }
                // erst speichern, wenn sich seit dem letzten Takt nichts mehr geändert hat
                // und gerade nicht geschrieben, ausgewählt oder getippt wird
                if v.autosave_serial.replace(serial) != serial || st.is_busy() || st.sel.is_some() || st.text.is_some() {
                    continue;
                }
                let direct = st.doc.path.clone().filter(|p| is_xopp(p));
                (xopp::SaveData::from_doc(&st.doc), serial, direct)
            };
            v.autosaving.set(true);
            let v = v.clone();
            glib::spawn_future_local(async move {
                let target = direct.clone().unwrap_or_else(|| autosave_path(None));
                let res = gio::spawn_blocking(move || {
                    if let Some(dir) = target.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    xopp::save(&data, &target)
                })
                .await;
                v.autosaving.set(false);
                if !matches!(res, Ok(Ok(()))) {
                    return;
                }
                v.autosaved_serial.set(serial);
                if let Some(path) = direct {
                    v.canvas.st().history.saved_at = serial;
                    remove_autosave(&path);
                    v.canvas.changed();
                }
            });
        }
    }

    /// Sofort und synchron speichern (beim Schließen). false, wenn das Dokument
    /// noch keinen Dateinamen hat oder das Speichern fehlschlug.
    fn save_now(&self, v: &DocView) -> bool {
        v.canvas.commit_edits();
        let (data, serial, path) = {
            let st = v.canvas.st();
            let Some(p) = st.doc.path.clone().filter(|p| is_xopp(p)) else { return false };
            (xopp::SaveData::from_doc(&st.doc), st.history.serial, p)
        };
        match xopp::save(&data, &path) {
            Ok(()) => {
                v.canvas.st().history.saved_at = serial;
                remove_autosave(&path);
                true
            }
            Err(e) => {
                self.toast(&crate::tr!("Speichern fehlgeschlagen: {e}", "Saving failed: {e}"));
                false
            }
        }
    }

    fn export_pdf(self: &Rc<Self>) {
        let Some(v) = self.view() else { return };
        v.canvas.commit_edits();
        let (pages, suggestion) = {
            let st = v.canvas.st();
            let pages: Vec<_> = st.doc.pages.iter().map(|p| { let p = p.borrow(); p.snapshot(st.doc.page_pdf(&p)) }).collect();
            let base = st.doc.path.clone().or_else(|| st.doc.pdf_path.clone()).unwrap_or_else(|| PathBuf::from("Export.pdf"));
            let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Export".into());
            (pages, base.with_file_name(format!("{stem}_annotated.pdf")))
        };
        let dialog = gtk::FileDialog::builder().title(crate::i18n::tl("Als PDF exportieren")).modal(true).build();
        if let Some(n) = suggestion.file_name() {
            dialog.set_initial_name(Some(&n.to_string_lossy()));
        }
        if let Some(d) = suggestion.parent().filter(|d| d.exists()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(d)));
        }
        let w = self.clone();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Ok(f) = res else { return };
            let Some(path) = f.path() else { return };
            let w = w.clone();
            glib::spawn_future_local(async move {
                let src_pdf = pages.iter().find_map(|p| p.pdf.clone());
                if src_pdf.as_deref() == Some(path.as_path()) {
                    w.toast(crate::i18n::tl("Das Original-PDF kann nicht überschrieben werden"));
                    return;
                }
                let res = gio::spawn_blocking(move || export_pages(&pages, &path)).await;
                match res {
                    Ok(Ok(())) => w.toast(crate::i18n::tl("PDF exportiert")),
                    Ok(Err(e)) => w.toast(&crate::tr!("Export fehlgeschlagen: {e}", "Export failed: {e}")),
                    Err(_) => w.toast(crate::i18n::tl("Export fehlgeschlagen")),
                }
            });
        });
    }

    fn on_close(self: &Rc<Self>) -> glib::Propagation {
        self.claude.stop_all();
        for v in self.views.borrow().iter().cloned().chain(self.viewers()) {
            v.canvas.remember_position();
        }
        // Dokumente mit Dateinamen werden einfach gespeichert, gefragt wird nur bei unbenannten
        let views: Vec<Rc<DocView>> = self.views.borrow().clone();
        let dirty: Vec<Rc<DocView>> =
            views.into_iter().filter(|v| v.canvas.st().history.dirty() && !self.save_now(v)).collect();
        let Some(first) = dirty.first() else { return glib::Propagation::Proceed };
        // erstes ungespeichertes Dokument zeigen und nachfragen
        let page = self.tabs.page(&first.root);
        self.tabs.set_selected_page(&page);
        self.stack.set_visible_child_name("editor");
        let n = dirty.len();
        let d = adw::AlertDialog::new(
            Some(crate::i18n::tl("Änderungen speichern?")),
            Some(&if n == 1 { crate::i18n::tl("Ein Dokument hat ungespeicherte Änderungen.").to_string() } else { crate::tr!("{n} Dokumente haben ungespeicherte Änderungen.", "{n} documents have unsaved changes.") }),
        );
        d.add_responses(&[("cancel", crate::i18n::tl("Abbrechen")), ("discard", crate::i18n::tl("Alle verwerfen")), ("save", crate::i18n::tl("Speichern"))]);
        d.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let w = self.clone();
        d.choose(Some(&self.window), gio::Cancellable::NONE, move |r| match r.as_str() {
            "discard" => {
                for v in w.views.borrow().iter() {
                    v.canvas.st().history.mark_saved();
                }
                w.window.close();
            }
            "save" => w.save(false),
            _ => {}
        });
        glib::Propagation::Stop
    }
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Lädt .xopp oder PDF (bei einem PDF mit gleichnamiger .xopp daneben wird die geöffnet).
fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn load_any(path: &Path) -> Result<Document, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "pdf" {
        let xopp = path.with_extension("xopp");
        if xopp.exists() {
            return xopp::load(&xopp);
        }
        render::pdf_page_sizes(path).map(|s| {
            let mut d = Document::from_pdf(canonical(path), &s);
            d.path = None;
            d
        })
    } else {
        xopp::load(path)
    }
}

pub fn autosave_path(doc: Option<&Path>) -> PathBuf {
    let dir = glib::user_cache_dir().join("graphit").join("autosave");
    let name = doc
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::i18n::tl("Unbenannt").into());
    let hash = doc.map(|p| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        p.hash(&mut h);
        h.finish() & 0xffffff
    });
    dir.join(format!("{}-{:06x}.xopp", name, hash.unwrap_or(0)))
}

fn is_xopp(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("xopp")).unwrap_or(false)
}

fn remove_autosave(doc: &Path) {
    let _ = std::fs::remove_file(autosave_path(Some(doc)));
}

/// Vektor-Export: PDF-Hintergründe werden über Poppler als Vektoren übernommen.
pub fn export_pages(pages: &[std::sync::Arc<crate::model::PageData>], path: &Path) -> Result<(), String> {
    let first = pages.first().ok_or(crate::i18n::tl("leeres Dokument"))?;
    let tmp = path.with_extension("pdf.graphit-tmp");
    {
        let surf = cairo::PdfSurface::new(first.width, first.height, &tmp).map_err(|e| e.to_string())?;
        let cr = cairo::Context::new(&surf).map_err(|e| e.to_string())?;
        for p in pages {
            surf.set_size(p.width, p.height).map_err(|e| e.to_string())?;
            render::draw_page(&cr, p, true);
            cr.show_page().map_err(|e| e.to_string())?;
        }
        surf.finish();
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}
