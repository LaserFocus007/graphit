//! Eigene symbolische Icons (werden von GTK automatisch in Theme-Farbe eingefärbt).

const ICONS: &[(&str, &str)] = &[
    ("fk-ask", r##"<path fill-rule="evenodd" d="M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1zm0 10.2a1 1 0 1 0 0 2 1 1 0 0 0 0-2zm.1-7.9c-1.5 0-2.7.9-2.9 2.4l1.5.2c.1-.7.6-1.2 1.4-1.2.8 0 1.3.5 1.3 1.1 0 .6-.4.9-.9 1.3-.7.5-1.3 1-1.3 2.1v.5h1.5v-.4c0-.5.3-.8.8-1.1.7-.5 1.4-1.1 1.4-2.4 0-1.5-1.3-2.5-2.8-2.5z"/>"##),
    ("fk-spark", r##"<path d="M7 1l1.6 4.4L13 7 8.6 8.6 7 13 5.4 8.6 1 7l4.4-1.6z"/><path d="M12.5 10.5l.8 1.9 1.9.8-1.9.8-.8 1.9-.8-1.9-1.9-.8 1.9-.8z" opacity=".7"/>"##),
    ("fk-pen", r##"<path d="M11.6 1.2a1 1 0 0 1 1.4 0l1.8 1.8a1 1 0 0 1 0 1.4L5.2 14 1 15l1-4.2z"/>"##),
    ("fk-highlighter", r##"<path d="M10.2 1.3a1 1 0 0 1 1.4 0l3.1 3.1a1 1 0 0 1 0 1.4L8.4 12.1H5.6L3.9 10.4V7.6z"/><path d="M3.2 11.2l1.6 1.6L3.6 14H1z"/><rect x="7" y="14" width="8" height="1.6" rx=".8" opacity=".5"/>"##),
    ("fk-eraser", r##"<path d="M9.4 1.6a1.2 1.2 0 0 1 1.7 0l3.3 3.3a1.2 1.2 0 0 1 0 1.7l-5.3 5.3-5-5z"/><path d="M3.4 7.6l5 5L7.1 14H4.3l-2.6-2.6a1.2 1.2 0 0 1 0-1.7z" opacity=".55"/><rect x="8" y="14" width="7" height="1.4" rx=".7"/>"##),
    ("fk-lasso", r##"<path fill-rule="evenodd" d="M8 1.5c3.9 0 7 2.2 7 5s-3.1 5-7 5c-.7 0-1.4-.1-2-.2l-.4-1.5c.7.2 1.5.3 2.4.3 3.2 0 5.5-1.7 5.5-3.6S11.2 2.9 8 2.9 2.5 4.6 2.5 6.5c0 .9.5 1.7 1.3 2.4l-.6 1.4C1.8 9.3 1 8 1 6.5c0-2.8 3.1-5 7-5z"/><circle cx="4.6" cy="11.6" r="1.6"/><path d="M4 13h1.4c0 1-.6 2-1.6 2.5l-.6-1C3.7 14.2 4 13.6 4 13z"/>"##),
    ("fk-shapes", r##"<path fill-rule="evenodd" d="M1 1h8v8H1zm1.5 1.5v5h5v-5z"/><path fill-rule="evenodd" d="M11 7a4 4 0 1 1 0 8 4 4 0 0 1 0-8zm0 1.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z"/>"##),
    ("fk-text", r##"<path d="M2 2h12v3h-1.6l-.4-1.4H9v9.2l1.5.4V15h-5v-1.8l1.5-.4V3.6H4l-.4 1.4H2z"/>"##),
    ("fk-image", r##"<path fill-rule="evenodd" d="M1 2.5h14v11H1zm1.5 1.5v6.4l3-3 3.2 3.2 1.8-1.8 3 3V4z"/><circle cx="10.5" cy="6.3" r="1.3"/>"##),
    ("fk-hand", r##"<path d="M6.2 1.5c.6 0 1 .4 1 1V7h.5V2c0-.6.4-1 1-1s1 .4 1 1v5h.5V3c0-.6.4-1 1-1s1 .4 1 1v4.5h.4V5c0-.6.4-1 1-1s1 .4 1 1v5c0 3-2.2 5.5-5.2 5.5-2 0-3.2-.8-4.3-2.3L1.6 9.6c-.4-.6-.2-1.3.3-1.6.5-.3 1.1-.2 1.5.3l1.8 2.1V2.5c0-.6.4-1 1-1z"/>"##),
    ("fk-space", r##"<rect x="1" y="1" width="14" height="1.6" rx=".8"/><rect x="1" y="13.4" width="14" height="1.6" rx=".8"/><path d="M8 3.6l2.8 2.8H8.8v3.2h2L8 12.4 5.2 9.6h2V6.4h-2z"/>"##),
    ("fk-ruler", r##"<path fill-rule="evenodd" d="M11.3.7l4 4L4.7 15.3l-4-4zm0 2.1L2.8 11.3l1.9 1.9 1-1-1-1 .9-.9 1 1 1-1-1.5-1.5.9-.9 1.5 1.5 1-1-1-1 .9-.9 1 1 1-1-1.5-1.5.9-.9 1.5 1.5.6-.6z"/>"##),
    ("fk-pressure", r##"<path d="M1 13c3-.2 4.5-2.2 6-5s3.5-5 8-5v2.2c-3.4 0-4.6 1.7-6 4.3-1.6 3-3.6 5.6-8 5.8z"/>"##),
    ("fk-shape-line", r##"<path d="M13.3 1.6l1.1 1.1L2.7 14.4l-1.1-1.1z"/>"##),
    ("fk-shape-arrow", r##"<path d="M15 1v6.5l-2.3-2.3-10 10-1.1-1.1 10-10L9.2 1.8z"/>"##),
    ("fk-shape-darrow", r##"<path d="M15 1v5.5l-2-2-8.5 8.5 2 2H1v-5.5l2 2L11.5 3l-2-2z"/>"##),
    ("fk-shape-rect", r##"<path fill-rule="evenodd" d="M1 3h14v10H1zm1.5 1.5v7h11v-7z"/>"##),
    ("fk-shape-ellipse", r##"<path fill-rule="evenodd" d="M8 2.5c3.9 0 7 2.5 7 5.5s-3.1 5.5-7 5.5S1 11 1 8s3.1-5.5 7-5.5zm0 1.5C4.9 4 2.5 5.8 2.5 8s2.4 4 5.5 4 5.5-1.8 5.5-4S11.1 4 8 4z"/>"##),
    ("fk-shape-axes", r##"<path d="M3 1l2 2.5H3.8v9h9V11l2.5 2-2.5 2v-1.2H2.5V3.5H1.2z"/>"##),
    ("fk-split", r##"<path fill-rule="evenodd" d="M1 2h14v12H1zm1.5 1.5v9h4.75v-9zm6.25 0v9h4.75v-9z"/>"##),
    ("fk-split-below", r##"<path fill-rule="evenodd" d="M1 2h14v12H1zm1.5 1.5v3.75h11V3.5zm0 5.25v3.75h11V8.75z"/>"##),
    ("fk-pages", r##"<path fill-rule="evenodd" d="M3 1h7l3 3v8H3zm1.5 1.5v8h7V4.6L9.4 2.5z"/><path d="M5 13.5h8.5V5H15v10H5z" opacity=".55"/>"##),
];

/// Schreibt die Icons in ein Cache-Verzeichnis und registriert es beim Icon-Theme.
pub fn install() {
    let dir = gtk::glib::user_cache_dir().join("graphit").join("icons");
    let actions = dir.join("hicolor").join("scalable").join("actions");
    let _ = std::fs::create_dir_all(&actions);
    for (name, body) in ICONS {
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><g fill="#2e3436">{body}</g></svg>"##
        );
        let path = actions.join(format!("{name}-symbolic.svg"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(svg.as_str()) {
            let _ = std::fs::write(&path, svg);
        }
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}
