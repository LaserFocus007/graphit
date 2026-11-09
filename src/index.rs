//! Modulwissen: Der Text aller PDFs eines Moduls samt zugehöriger Altklausuren liegt
//! als durchsuchbare Textdateien bereit, der Lernstand als kleine JSON-Datei. So findet
//! Claude Stellen per grep, statt ganze PDFs zu lesen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const MANIFEST: &str = "index.json";
const LISTING: &str = "verzeichnis.txt";
const EXAMS: &str = "Old_exams";
const PROGRESS: &str = "lernstand.json";
const MAX_PAGES: i32 = 3000;
const MAX_TOPICS: usize = 100;
/// So viele wackelige Themen gehen mit jeder Frage mit
pub const TOPICS_IN_PROMPT: usize = 12;

// ------------------------------------------------------------------------ Altklausuren

const FILLER: [&str; 6] = ["and", "of", "with", "the", "und", "für"];

fn is_numeral(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| matches!(c.to_ascii_uppercase(), 'I' | 'V' | 'X')) && w.chars().all(char::is_uppercase)
}

/// Punkte: wie gut `folder` zum Kurznamen `module` passt; `None`, wenn gar nicht.
fn match_score(module: &str, folder: &str) -> Option<u32> {
    if module.eq_ignore_ascii_case(folder) {
        return Some(100);
    }
    let parts: Vec<&str> = module.split_whitespace().collect();
    let words: Vec<&str> = folder.split_whitespace().collect();
    let module_num = parts.iter().copied().find(|w| is_numeral(w));
    let folder_num = words.iter().copied().find(|w| is_numeral(w));
    // TD II gehört nicht zu Thermodynamics I
    if module_num != folder_num {
        return None;
    }
    let letters: String = parts.iter().filter(|w| !is_numeral(w)).copied().collect::<String>().to_lowercase();
    if letters.is_empty() {
        return None;
    }
    let content: Vec<String> = words.iter().filter(|w| !is_numeral(w) && !FILLER.contains(&w.to_lowercase().as_str())).map(|w| w.to_lowercase()).collect();
    let initials: String = content.iter().filter_map(|w| w.chars().next()).collect();
    if letters == initials {
        return Some(80);
    }
    // TD in Thermodynamics: gleicher Anfang, restliche Buchstaben der Reihe nach
    if content.len() == 1 {
        let mut rest = content[0].chars();
        if letters.chars().next() == content[0].chars().next() && letters.chars().all(|l| rest.any(|c| c == l)) {
            return Some(50);
        }
    }
    None
}

/// Bester Altklausur-Ordner zu einem Modul; bei Gleichstand keiner.
pub fn suggest<'a>(module: &str, folders: &'a [String]) -> Option<&'a String> {
    let mut scored: Vec<(u32, &String)> = folders.iter().filter_map(|f| Some((match_score(module, f)?, f))).collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    match &scored[..] {
        [] => None,
        [a, b, ..] if a.0 == b.0 => None,
        [a, ..] => Some(a.1),
    }
}

fn subdirs(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    v
}

/// Ordner mit den Altklausuren: Einstellung, sonst `Old_exams` im Notizordner.
fn exam_root(root: &Path) -> PathBuf {
    let chosen = crate::settings::Settings::load().exam_root;
    if chosen.is_empty() { root.join(EXAMS) } else { PathBuf::from(chosen) }
}

pub fn exam_folders(root: &Path) -> Vec<String> {
    subdirs(&exam_root(root))
}

/// Alle Module der Bibliothek: die Ordner unter `*Semester*`, die übrigen Ordner direkt.
pub fn modules(root: &Path) -> Vec<String> {
    subdirs(root)
        .iter()
        .filter(|d| d.as_str() != EXAMS)
        .flat_map(|d| if d.to_lowercase().contains("semester") { subdirs(&root.join(d)) } else { vec![d.clone()] })
        .collect()
}

/// Einstellung `claude_exams`: "TD II=Thermodynamics II;FM I=". Leer rechts: bewusst keiner.
pub fn parse_choices(text: &str) -> HashMap<String, String> {
    text.split(';').filter_map(|p| p.split_once('=')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

pub fn format_choices(map: &HashMap<String, String>) -> String {
    let mut v: Vec<String> = map.iter().map(|(a, b)| format!("{a}={b}")).collect();
    v.sort();
    v.join(";")
}

/// Altklausur-Ordner eines Moduls: Wahl des Nutzers, sonst Vorschlag.
pub fn exam_folder(root: &Path, module: &str, choices: &str) -> Option<PathBuf> {
    let name = match parse_choices(choices).get(module) {
        Some(chosen) => chosen.clone(),
        None => suggest(module, &exam_folders(root))?.clone(),
    };
    (!name.is_empty()).then(|| exam_root(root).join(name)).filter(|d| d.is_dir())
}

// ------------------------------------------------------------------------ Index

fn pdfs_in(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            pdfs_in(&p, out, depth + 1);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
            out.push(p);
        }
    }
}

fn stamp(p: &Path) -> String {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| Some(format!("{}:{}", m.len(), m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())))
        .unwrap_or_default()
}

fn extract(path: &Path, target: &Path) -> Option<i32> {
    let doc = poppler::Document::from_file(&gtk::glib::filename_to_uri(path, None).ok()?, None).ok()?;
    let pages = doc.n_pages().min(MAX_PAGES);
    let mut text = format!("# Quelle: {}\n", path.display());
    for i in 0..pages {
        if let Some(t) = doc.page(i).and_then(|p| p.text()) {
            for line in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
                text.push_str(&format!("S{}: {line}\n", i + 1));
            }
        }
    }
    std::fs::write(target, text).ok()?;
    Some(pages)
}

/// Quelle hinter einer Kennung wie D7.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub path: PathBuf,
    pub name: String,
}

/// Bringt den Index auf Stand; Unverändertes wird nicht neu gelesen. Läuft im Hintergrund.
pub fn refresh(module: &Path, exams: Option<&Path>, cache: &Path) -> HashMap<String, Source> {
    let dir = cache.join("index");
    let _ = std::fs::create_dir_all(&dir);
    let old: Value = std::fs::read_to_string(dir.join(MANIFEST)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let old = old["dokumente"].as_array().cloned().unwrap_or_default();
    let known: HashMap<String, &Value> = old.iter().filter_map(|d| Some((d["pfad"].as_str()?.to_string(), d))).collect();
    let mut next = old.iter().filter_map(|d| d["id"].as_str()?.strip_prefix('D')?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
    let mut files: Vec<(PathBuf, &str)> = Vec::new();
    let mut list = Vec::new();
    pdfs_in(module, &mut list, 0);
    list.sort();
    files.extend(list.drain(..).map(|p| (p, "Unterlage")));
    if let Some(e) = exams {
        pdfs_in(e, &mut list, 0);
        list.sort();
        files.extend(list.drain(..).map(|p| (p, "Altklausur")));
    }
    let mut docs = Vec::new();
    let mut sources = HashMap::new();
    let mut listing = String::from("# Kennung | Art | Seiten | Datei\n");
    for (path, kind) in files {
        let key = path.to_string_lossy().into_owned();
        let now = stamp(&path);
        let was = known.get(&key);
        let id = was.and_then(|d| d["id"].as_str()).map(String::from).unwrap_or_else(|| {
            next += 1;
            format!("D{}", next - 1)
        });
        let target = dir.join(format!("{id}.txt"));
        let pages = match was.filter(|d| d["stand"].as_str() == Some(now.as_str()) && target.exists()).and_then(|d| d["seiten"].as_i64()) {
            Some(p) => p as i32,
            None => match extract(&path, &target) {
                Some(p) => p,
                None => continue,
            },
        };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        listing.push_str(&format!("{id} | {kind} | {pages} | {name}\n"));
        docs.push(json!({"id": id, "pfad": key, "stand": now, "seiten": pages}));
        sources.insert(id, Source { path, name });
    }
    // Text entfernter Dateien wegräumen
    for d in &old {
        if let Some(id) = d["id"].as_str().filter(|id| !sources.contains_key(*id)) {
            let _ = std::fs::remove_file(dir.join(format!("{id}.txt")));
        }
    }
    let _ = std::fs::write(dir.join(MANIFEST), json!({"dokumente": docs}).to_string());
    let _ = std::fs::write(dir.join(LISTING), listing);
    sources
}

// ------------------------------------------------------------------------ Marken in Antworten

const OPEN: char = '⟦';
const CLOSE: char = '⟧';

#[derive(Debug, Default, PartialEq)]
pub struct Marks {
    /// Text ohne Marken
    pub clean: String,
    /// Kennung und Seite ab 1
    pub sources: Vec<(String, usize)>,
    /// Thema und ob es sitzt
    pub topics: Vec<(String, bool)>,
    /// Vorlesungsmodus: nächster Abschnitt ab dieser Seite, `Some(None)` am Ende
    pub next: Option<Option<usize>>,
}

fn parse_source(body: &str) -> Option<(String, usize)> {
    let (id, page) = body.trim().split_once(char::is_whitespace)?;
    let page: usize = page.trim().trim_start_matches(['S', 's']).trim_start_matches('.').trim().parse().ok()?;
    (id.starts_with('D') && id[1..].chars().all(|c| c.is_ascii_digit()) && id.len() > 1 && page > 0).then(|| (id.to_string(), page))
}

fn parse_topic(body: &str) -> Option<(String, bool)> {
    let (name, state) = body.trim().strip_prefix("thema:")?.split_once('|')?;
    let name = name.trim();
    match state.trim() {
        _ if name.is_empty() => None,
        "sicher" => Some((name.to_string(), true)),
        "wackelig" => Some((name.to_string(), false)),
        _ => None,
    }
}

/// Marken herausziehen. Eine noch nicht fertig gestreamte Marke am Ende bleibt unsichtbar.
pub fn marks(text: &str) -> Marks {
    let mut m = Marks::default();
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        m.clean.push_str(&rest[..i]);
        let after = &rest[i + OPEN.len_utf8()..];
        let Some(j) = after.find(CLOSE) else {
            rest = "";
            break;
        };
        let body = &after[..j];
        if let Some(s) = parse_source(body) {
            if !m.sources.contains(&s) {
                m.sources.push(s);
            }
        } else if let Some(t) = parse_topic(body) {
            m.topics.push(t);
        } else if let Some(n) = body.trim().strip_prefix("weiter:") {
            let n = n.trim();
            m.next = if n == "ende" { Some(None) } else { n.parse().ok().map(Some).or(m.next) };
        }
        rest = &after[j + CLOSE.len_utf8()..];
    }
    m.clean.push_str(rest);
    m.clean = m.clean.trim_end().to_string();
    m
}

// ------------------------------------------------------------------------ Abschriften

const TRANSCRIPTS: &str = "abschriften";
const TRANSCRIPT_DIR: &str = "abschrift";
const HASHES: &str = "stand.json";
const LECTURES: &str = "vorlesung.json";

/// Kennung des Handschrift-Inhalts einer Seite. So überlebt eine Abschrift das Einfügen
/// von Seiten, und jede Änderung an der Seite macht sie ungültig.
fn ink_hash(page: &crate::model::PageData) -> Option<String> {
    use crate::model::Item;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut strokes = 0;
    for it in page.layers.iter().flat_map(|l| l.iter()) {
        match &**it {
            Item::Stroke(s) => {
                strokes += 1;
                for p in &s.pts {
                    p.x.to_bits().hash(&mut h);
                    p.y.to_bits().hash(&mut h);
                }
            }
            Item::Text(t) => t.text.hash(&mut h),
            _ => {}
        }
    }
    (strokes > 0).then(|| format!("{:016x}", h.finish()))
}

/// Vorhandene Abschriften der Notiz bereitlegen.
pub fn offer_transcripts(data: &Path, cache: &Path, pages: &[std::sync::Arc<crate::model::PageData>]) {
    let dir = cache.join(TRANSCRIPT_DIR);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let store = data.join(TRANSCRIPTS);
    let mut map = serde_json::Map::new();
    for (i, p) in pages.iter().enumerate() {
        if let Some(hash) = ink_hash(p) {
            let _ = std::fs::copy(store.join(format!("{hash}.md")), dir.join(format!("S{}.md", i + 1)));
            map.insert((i + 1).to_string(), Value::String(hash));
        }
    }
    let _ = std::fs::write(dir.join(HASHES), Value::Object(map).to_string());
}

/// Neue Abschriften von Claude dauerhaft ablegen.
pub fn collect_transcripts(data: &Path, cache: &Path) {
    let dir = cache.join(TRANSCRIPT_DIR);
    let hashes: Value = std::fs::read_to_string(dir.join(HASHES)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let store = data.join(TRANSCRIPTS);
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(page) = name.strip_prefix('S').and_then(|n| n.strip_suffix(".md")) else { continue };
        let Some(hash) = hashes[page].as_str() else { continue };
        let target = store.join(format!("{hash}.md"));
        if !target.exists() {
            let _ = std::fs::create_dir_all(&store);
            let _ = std::fs::copy(e.path(), target);
        }
    }
}

// ------------------------------------------------------------------------ Vorlesungsmodus

pub fn lecture_next(data: &Path, note: &Path) -> Option<usize> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(data.join(LECTURES)).ok()?).ok()?;
    v[note.to_string_lossy().as_ref()].as_u64().map(|n| n as usize)
}

pub fn set_lecture_next(data: &Path, note: &Path, next: Option<usize>) {
    let mut v: Value = std::fs::read_to_string(data.join(LECTURES)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let key = note.to_string_lossy().into_owned();
    match next {
        Some(n) => v[&key] = json!(n),
        None => {
            if let Some(o) = v.as_object_mut() {
                o.remove(&key);
            }
        }
    }
    let _ = std::fs::create_dir_all(data);
    let _ = std::fs::write(data.join(LECTURES), v.to_string());
}

// ------------------------------------------------------------------------ Lernstand

#[derive(Clone, Debug, PartialEq)]
pub struct Topic {
    pub name: String,
    pub sure: bool,
}

pub fn load_progress(data: &Path) -> Vec<Topic> {
    let v: Value = std::fs::read_to_string(data.join(PROGRESS)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!([]));
    v.as_array().into_iter().flatten().filter_map(|t| Some(Topic { name: t["thema"].as_str()?.to_string(), sure: t["sicher"].as_bool()? })).collect()
}

pub fn save_progress(data: &Path, topics: &[Topic]) {
    let _ = std::fs::create_dir_all(data);
    let list: Vec<Value> = topics.iter().map(|t| json!({"thema": t.name, "sicher": t.sure})).collect();
    let _ = std::fs::write(data.join(PROGRESS), Value::Array(list).to_string());
}

/// Neue Einschätzung einarbeiten; das zuletzt Berührte steht vorn.
pub fn update_topics(topics: &mut Vec<Topic>, news: &[(String, bool)]) {
    for (name, sure) in news {
        topics.retain(|t| !t.name.eq_ignore_ascii_case(name));
        topics.insert(0, Topic { name: name.clone(), sure: *sure });
    }
    topics.truncate(MAX_TOPICS);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Vec<String> {
        ["Aerospace Structures and Elements", "Engineering Mechanics I", "Engineering Mechanics III", "Fluid Mechanics I", "Fluid Mechanics II", "Thermodynamics I", "Thermodynamics II", "Heat Transfer"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn matches_exam_folders() {
        let f = folders();
        let s = |m: &str| suggest(m, &f).map(String::as_str);
        assert_eq!(s("TD II"), Some("Thermodynamics II"));
        assert_eq!(s("FM I"), Some("Fluid Mechanics I"));
        assert_eq!(s("EM III"), Some("Engineering Mechanics III"));
        assert_eq!(s("ASE"), Some("Aerospace Structures and Elements"));
        assert_eq!(s("Heat Transfer"), Some("Heat Transfer"));
        assert_eq!(s("I2G"), None);
        assert_eq!(s("AM III"), None);
    }

    #[test]
    fn choices_round_trip() {
        let m = parse_choices("TD II=Thermodynamics II;FM I=");
        assert_eq!(m["TD II"], "Thermodynamics II");
        assert_eq!(m["FM I"], "");
        assert_eq!(parse_choices(&format_choices(&m)), m);
    }

    #[test]
    fn reads_marks() {
        let m = marks("Das gilt immer. ⟦D7 S42⟧ ⟦D2 S3⟧\n⟦thema: Enthalpie | wackelig⟧⟦thema: Gasgesetz | sicher⟧");
        assert_eq!(m.clean, "Das gilt immer.");
        assert_eq!(m.sources, [("D7".to_string(), 42), ("D2".to_string(), 3)]);
        assert_eq!(m.topics, [("Enthalpie".to_string(), false), ("Gasgesetz".to_string(), true)]);
        // unfertig beim Streamen, unbekannte Marken verschwinden still
        assert_eq!(marks("Text ⟦D7 S").clean, "Text");
        assert_eq!(marks("a ⟦unsinn⟧ b").clean, "a  b");
        assert_eq!(marks("ohne").sources, []);
        assert_eq!(marks("x ⟦weiter: 8⟧").next, Some(Some(8)));
        assert_eq!(marks("x ⟦weiter: ende⟧").next, Some(None));
        assert_eq!(marks("x").next, None);
        // doppelt zählt einmal
        assert_eq!(marks("⟦D1 S2⟧⟦D1 S2⟧").sources.len(), 1);
    }

    #[test]
    fn topics_update() {
        let mut t = vec![Topic { name: "A".into(), sure: false }];
        update_topics(&mut t, &[("a".into(), true), ("B".into(), false)]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "B");
        assert!(t[1].sure);
    }
}
