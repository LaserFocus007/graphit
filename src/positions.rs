//! Leseposition pro Dokument (Seite, Versatz, Zoom), damit eine Datei dort weitergeht,
//! wo man aufgehört hat. Liegt in ~/.config/graphit/positions.txt, nicht in der .xopp:
//! Scrollen soll die Datei nicht verändern.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Wo man im Dokument war.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub page: usize,
    /// Abstand vom oberen Seitenrand bis zur Oberkante der Ansicht (pt)
    pub offset: f64,
    /// None = Seitenbreite (automatisch angepasst), sonst Zoom und horizontale Lage
    pub zoom: Option<(f64, f64)>,
}

const MAX: usize = 500;

fn file() -> PathBuf {
    gtk::glib::user_config_dir().join("graphit").join("positions.txt")
}

fn key(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Zeilen: Seite \t Versatz \t Zoom (0 = Seitenbreite) \t x \t Pfad; neueste zuerst.
fn load() -> Vec<(PathBuf, Position)> {
    std::fs::read_to_string(file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(5, '\t');
            let page = f.next()?.parse().ok()?;
            let offset = f.next()?.parse().ok()?;
            let zoom: f64 = f.next()?.parse().ok()?;
            let x: f64 = f.next()?.parse().ok()?;
            let path = PathBuf::from(f.next()?);
            Some((path, Position { page, offset, zoom: (zoom > 0.0).then_some((zoom, x)) }))
        })
        .collect()
}

pub fn get(p: &Path) -> Option<Position> {
    let k = key(p);
    load().into_iter().find(|(q, _)| *q == k).map(|e| e.1)
}

pub fn set(p: &Path, pos: Position) {
    if std::env::var("GRAPHIT_SELFTEST").is_ok() {
        return;
    }
    let k = key(p);
    let mut all = load();
    if all.first().map(|(q, old)| *q == k && *old == pos).unwrap_or(false) {
        return;
    }
    all.retain(|(q, _)| *q != k);
    all.insert(0, (k, pos));
    all.truncate(MAX);
    // Duplikate durch Umbenennen o. Ä. stören nicht; die erste Zeile gewinnt
    let mut seen = HashMap::new();
    let text: String = all
        .iter()
        .filter(|(q, _)| seen.insert(q.clone(), ()).is_none())
        .map(|(q, p)| {
            let (z, x) = p.zoom.unwrap_or((0.0, 0.0));
            format!("{}\t{:.1}\t{:.4}\t{:.1}\t{}\n", p.page, p.offset, z, x, q.to_string_lossy())
        })
        .collect();
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, text);
}

// ---------------------------------------------------------------- Lese-Viewer

/// Welche Dokumente zuletzt neben einem Hauptdokument in den Lese-Viewern offen waren
/// (oben, unten). Liegt in ~/.config/graphit/viewers.txt.
pub type Viewers = [Option<PathBuf>; 2];

fn viewers_file() -> PathBuf {
    gtk::glib::user_config_dir().join("graphit").join("viewers.txt")
}

/// Zeilen: Hauptdokument \t oben \t unten (leer = nichts offen); neueste zuerst.
fn load_viewers() -> Vec<(PathBuf, Viewers)> {
    let opt = |s: &str| (!s.is_empty()).then(|| PathBuf::from(s));
    std::fs::read_to_string(viewers_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let main = PathBuf::from(f.next().filter(|s| !s.is_empty())?);
            Some((main, [opt(f.next()?), opt(f.next()?)]))
        })
        .collect()
}

pub fn viewers(main: &Path) -> Viewers {
    let k = key(main);
    load_viewers().into_iter().find(|(q, _)| *q == k).map(|e| e.1).unwrap_or_default()
}

pub fn set_viewers(main: &Path, v: Viewers) {
    if std::env::var("GRAPHIT_SELFTEST").is_ok() {
        return;
    }
    let k = key(main);
    let v = v.map(|p| p.map(|p| key(&p)));
    let mut all = load_viewers();
    if all.iter().find(|(q, _)| *q == k).map(|e| e.1 == v).unwrap_or(v == Viewers::default()) {
        return;
    }
    all.retain(|(q, _)| *q != k);
    if v != Viewers::default() {
        all.insert(0, (k, v));
    }
    all.truncate(MAX);
    let s = |p: &Option<PathBuf>| p.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let text: String = all.iter().map(|(q, v)| format!("{}\t{}\t{}\n", q.to_string_lossy(), s(&v[0]), s(&v[1]))).collect();
    let f = viewers_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, text);
}
