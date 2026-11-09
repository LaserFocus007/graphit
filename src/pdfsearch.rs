//! Sucht ein verschobenes oder umbenanntes Hintergrund-PDF einer .xopp: erst den
//! gleichen Dateinamen in der Nähe, dann ein PDF im selben Ordner, das vom Namen her
//! passt. Übernommen wird nur ein Treffer, der genug Seiten hat und eindeutig ist.

use std::path::{Path, PathBuf};

use crate::model::Document;

/// Höchste PDF-Seite, die das Dokument verwendet.
pub fn pages_needed(doc: &Document) -> usize {
    doc.pages
        .iter()
        .filter_map(|p| {
            let p = p.borrow();
            p.bg.is_pdf().then(|| crate::model::get_attr(&p.bg.attrs, "pageno").and_then(|n| n.parse().ok()).unwrap_or(1))
        })
        .max()
        .unwrap_or(0)
}

fn page_count(p: &Path) -> usize {
    crate::render::pdf_page_sizes(p).map(|s| s.len()).unwrap_or(0)
}

fn is_pdf(p: &Path) -> bool {
    p.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false)
}

/// Wörter eines Dateinamens (klein, ab 3 Zeichen), z. B. "The-One-Percent-Journal-A4" →
/// the, one, percent, journal.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(|w| w.to_lowercase()).collect()
}

/// Alle Dateien namens `name` unter `dir` (höchstens `depth` Ebenen tief, ohne
/// versteckte Ordner).
fn find_named(dir: &Path, name: &std::ffi::OsStr, depth: usize, budget: &mut usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let p = e.path();
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        match e.file_type() {
            Ok(t) if t.is_dir() && !hidden && depth > 0 => find_named(&p, name, depth - 1, budget, out),
            Ok(t) if t.is_file() && e.file_name() == name => out.push(p),
            _ => {}
        }
    }
}

/// Fehlt das Hintergrund-PDF einer geladenen .xopp, passendes suchen und eintragen.
/// Liefert (gefunden, weiterhin fehlend).
pub fn repair(doc: &mut Document) -> (Option<PathBuf>, Option<PathBuf>) {
    let (Some(pdf), Some(xopp)) = (doc.pdf_path.clone(), doc.path.clone()) else { return (None, None) };
    if pdf.exists() || pages_needed(doc) == 0 {
        return (None, None);
    }
    let t0 = std::time::Instant::now();
    let hit = find(&xopp, &pdf, pages_needed(doc));
    if std::env::var("GRAPHIT_PROFILE").is_ok() {
        eprintln!("PROFILE PDF-Suche {:.1} ms → {hit:?}", t0.elapsed().as_secs_f64() * 1000.0);
    }
    match hit {
        Some(f) => {
            doc.pdf_path = Some(f.clone());
            (Some(f), None)
        }
        None => (None, Some(pdf)),
    }
}

/// Passendes PDF für `missing` (Pfad aus der .xopp) suchen.
pub fn find(xopp: &Path, missing: &Path, needed: usize) -> Option<PathBuf> {
    let dir = xopp.parent()?;
    let enough = |p: &Path| page_count(p) >= needed.max(1);

    let named = |roots: Vec<(PathBuf, usize)>| -> Option<PathBuf> {
        let name = missing.file_name()?;
        let mut budget = 40_000;
        for (root, depth) in roots {
            let mut hits = Vec::new();
            find_named(&root, name, depth, &mut budget, &mut hits);
            if let Some(h) = hits.into_iter().find(|h| enough(h)) {
                return Some(h);
            }
        }
        None
    };

    // 1. gleicher Dateiname im Ordner der .xopp oder darunter
    if let Some(h) = named(vec![(dir.to_path_buf(), 3)]) {
        return Some(h);
    }
    // 2. umbenannt im selben Ordner
    if let Some(h) = renamed(xopp, dir, missing, &enough) {
        return Some(h);
    }
    // 3. gleicher Dateiname weiter weg: Ordner darüber, Dokumente, Downloads, Home
    {
        let home = gtk::glib::home_dir();
        let mut roots: Vec<(PathBuf, usize)> = Vec::new();
        if let Some(up) = dir.parent() {
            roots.push((up.to_path_buf(), 4));
        }
        for d in [gtk::glib::user_special_dir(gtk::glib::UserDirectory::Documents), gtk::glib::user_special_dir(gtk::glib::UserDirectory::Downloads)]
            .into_iter()
            .flatten()
        {
            roots.push((d, 4));
        }
        roots.push((home, 2));
        named(roots)
    }
}

/// PDF im selben Ordner, dessen Name zur .xopp oder zum alten Namen passt (eindeutig).
fn renamed(xopp: &Path, dir: &Path, missing: &Path, enough: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let stem = xopp.file_stem()?.to_string_lossy().to_lowercase();
    let old = words(&missing.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let xw = words(&stem);
    let mut scored: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_pdf(p) && p.is_file())
        .filter_map(|p| {
            let s = p.file_stem()?.to_string_lossy().to_lowercase();
            let w = words(&s);
            let mut score = 0;
            if s == stem {
                score += 3;
            }
            score += w.iter().filter(|x| old.contains(x)).count();
            score += w.iter().filter(|x| xw.contains(x)).count();
            (score > 0).then_some((score, p))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.retain(|(_, p)| enough(p));
    match scored.as_slice() {
        [(_, p)] => Some(p.clone()),
        [(a, p), (b, _), ..] if a > b => Some(p.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_of_names() {
        assert_eq!(words("The-One-Percent-Journal-A4"), ["the", "one", "percent", "journal"]);
        assert_eq!(words("Percent"), ["percent"]);
    }
}
