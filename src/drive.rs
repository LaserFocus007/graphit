//! Dateien aus GNOME-Online-Konten (Google Drive und andere gvfs-Orte).
//!
//! Drive nennt seine Dateien nach ihrer ID, ohne Endung, und liest sich über das Netz
//! langsam. Graphit arbeitet deshalb mit einer lokalen Kopie unter dem echten Namen.
//! Zurückgeschrieben wird nichts: Das Original bleibt unberührt.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use gtk::{gio, glib};
use gtk::gio::prelude::*;

const STAND: &str = ".stand";

pub enum Remote {
    Folder,
    File {
        path: PathBuf,
        /// Notiz: Änderungen landen nur in der lokalen Kopie
        note: bool,
        /// das Original ist neuer als die bearbeitete oder nicht ladbare Kopie
        stale: bool,
    },
}

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(glib::user_runtime_dir().join("gvfs"))
}

/// Dateiname der Kopie, oder der Grund, warum Graphit die Datei nicht öffnet.
fn local_name(display: &str, content_type: &str) -> Result<String, String> {
    let name = display.replace('/', "-");
    let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" | "xopp" | "xoj" => Ok(name),
        _ if content_type == "application/pdf" => Ok(format!("{name}.pdf")),
        _ if content_type == "application/x-xopp" => Ok(format!("{name}.xopp")),
        _ => Err(format!("Graphit öffnet nur PDFs und Notizen, nicht „{name}“")),
    }
}

#[derive(Debug, PartialEq)]
enum Plan {
    /// neu laden
    Load,
    /// vorhandene Kopie nehmen
    Keep,
    /// lokal bearbeitet und das Original hat sich geändert: nichts überschreiben
    KeepStale,
}

/// `saved`: Stand von Original und Kopie beim letzten Laden. `local`: Stand der Kopie jetzt.
fn plan(remote: &str, local: Option<&str>, saved: Option<(&str, &str)>) -> Plan {
    let (Some(local), Some((was_remote, was_local))) = (local, saved) else { return Plan::Load };
    match (was_remote == remote, was_local == local) {
        (true, _) => Plan::Keep,
        (false, true) => Plan::Load,
        (false, false) => Plan::KeepStale,
    }
}

fn stamp(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", m.len(), t.as_nanos()))
}

/// Läuft im Hintergrund-Thread: Netzwerkzugriffe dürfen die Oberfläche nicht anhalten.
pub fn fetch(uri: &str) -> Result<Remote, String> {
    let file = gio::File::for_uri(uri);
    let attrs = "standard::type,standard::display-name,standard::content-type,standard::size,time::modified";
    let info = file
        .query_info(attrs, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .map_err(|e| format!("Datei nicht erreichbar: {}", e.message()))?;
    if info.file_type() == gio::FileType::Directory {
        return Ok(Remote::Folder);
    }
    let kind = info.content_type().map(|c| c.to_string()).unwrap_or_default();
    let name = local_name(&info.display_name(), &kind)?;
    let mut key = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut key);
    let dir = glib::user_data_dir().join("graphit").join("drive").join(format!("{:016x}", key.finish()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(&name);
    let note = !name.to_ascii_lowercase().ends_with(".pdf");
    let remote = format!("{}:{}", info.size(), info.attribute_uint64("time::modified"));
    let text = std::fs::read_to_string(dir.join(STAND)).unwrap_or_default();
    let saved = text.split_once('\n');
    let local = stamp(&target);
    let stale = match plan(&remote, local.as_deref(), saved) {
        Plan::Keep => false,
        Plan::KeepStale => true,
        Plan::Load => {
            let part = dir.join(".laden");
            let copied = file.copy(&gio::File::for_path(&part), gio::FileCopyFlags::OVERWRITE, gio::Cancellable::NONE, None);
            match copied {
                Ok(()) => {
                    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
                    let _ = std::fs::write(dir.join(STAND), format!("{remote}\n{}", stamp(&target).unwrap_or_default()));
                    false
                }
                // offline: lieber die alte Kopie als gar nichts
                Err(_) if target.exists() => true,
                Err(e) => return Err(format!("Laden fehlgeschlagen: {}", e.message())),
            }
        }
    };
    Ok(Remote::File { path: target, note, stale })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(local_name("Jacking Force.pdf", "application/pdf").unwrap(), "Jacking Force.pdf");
        assert_eq!(local_name("Skript", "application/pdf").unwrap(), "Skript.pdf");
        assert_eq!(local_name("a/b.xopp", "application/gzip").unwrap(), "a-b.xopp");
        assert!(local_name("Daten.xlsx", "application/vnd.ms-excel").is_err());
    }

    #[test]
    fn plans() {
        assert_eq!(plan("r", None, None), Plan::Load);
        assert_eq!(plan("r", Some("l"), None), Plan::Load);
        assert_eq!(plan("r", Some("l"), Some(("r", "l"))), Plan::Keep);
        // eigene Änderungen an der Kopie bleiben
        assert_eq!(plan("r", Some("neu"), Some(("r", "l"))), Plan::Keep);
        assert_eq!(plan("r2", Some("l"), Some(("r", "l"))), Plan::Load);
        assert_eq!(plan("r2", Some("neu"), Some(("r", "l"))), Plan::KeepStale);
    }
}
