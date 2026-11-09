//! Zuletzt geöffnete Dateien (einfache Textdatei in ~/.config/graphit).

use std::path::{Path, PathBuf};

fn file() -> PathBuf {
    gtk::glib::user_config_dir().join("graphit").join("recent.txt")
}

pub fn list() -> Vec<PathBuf> {
    std::fs::read_to_string(file())
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

pub fn add(p: &Path) {
    if std::env::var("GRAPHIT_SELFTEST").is_ok() || std::env::var("GRAPHIT_NO_RECENT").is_ok() {
        return;
    }
    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut l: Vec<PathBuf> = list().into_iter().filter(|x| *x != p).collect();
    l.insert(0, p);
    l.truncate(50);
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let s: Vec<String> = l.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let _ = std::fs::write(f, s.join("\n"));
}
