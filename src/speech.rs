//! Vorlesen: Piper mit einer natürlichen Stimme in der Sprache des Texts, solange keine
//! installiert ist die Systemstimme.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gtk::{gio, glib};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }

    /// Stimmen nach Güte; die erste vorhandene spricht.
    fn voices(self) -> &'static [&'static str] {
        match self {
            Lang::De => &["de_DE-thorsten-high", "de_DE-thorsten-medium"],
            Lang::En => &["en_US-lessac-high", "en_US-lessac-medium"],
        }
    }
}

fn pick(lang: Lang, de: &'static str, en: &'static str) -> &'static str {
    if lang == Lang::De { de } else { en }
}

macro_rules! say {
    ($lang:expr, $de:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $lang == Lang::De { format!($de $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}

/// Lädt Piper und die Stimmen; dieselbe Datei liegt im Paket unter /usr/share/graphit.
const INSTALL_SCRIPT: &str = include_str!("../tools/install-voice.sh");

fn voice_dir() -> PathBuf {
    glib::user_data_dir().join("graphit").join("piper")
}

fn piper_bin() -> Option<PathBuf> {
    Some(voice_dir().join("piper").join("piper")).filter(|b| b.exists())
}

fn voice_model(lang: Lang) -> Option<PathBuf> {
    let dir = voice_dir();
    lang.voices().iter().map(|v| dir.join(format!("{v}.onnx"))).find(|m| m.exists() && m.with_extension("onnx.json").exists())
}

/// Ob beide Sprachen eine natürliche Stimme in bester Güte haben.
pub fn natural_voices() -> bool {
    piper_bin().is_some() && [Lang::De, Lang::En].iter().all(|l| voice_dir().join(format!("{}.onnx", l.voices()[0])).exists())
}

/// Abtastrate aus der Beschreibung der Stimme.
fn sample_rate(model: &Path) -> u64 {
    std::fs::read_to_string(model.with_extension("onnx.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["audio"]["sample_rate"].as_u64())
        .unwrap_or(22050)
}

static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Läuft das Laden gerade?
pub fn installing() -> bool {
    INSTALLING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Einmal je Sitzung wahr, solange nur die Systemstimme da ist: Anlass für einen Hinweis.
pub fn hint_install() -> bool {
    !natural_voices() && !installing() && !HINTED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Stimmen im Hintergrund laden. `done` erfährt, ob es geklappt hat.
pub fn install(done: impl FnOnce(bool) + 'static) {
    if INSTALLING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let started = gio::Subprocess::newv(&["sh".as_ref(), "-s".as_ref(), "--".as_ref(), "all".as_ref()], gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE);
    let Ok(proc) = started else {
        INSTALLING.store(false, std::sync::atomic::Ordering::Relaxed);
        done(false);
        return;
    };
    glib::spawn_future_local(async move {
        let sent = proc.communicate_utf8_future(Some(INSTALL_SCRIPT.to_string())).await;
        INSTALLING.store(false, std::sync::atomic::Ordering::Relaxed);
        done(sent.is_ok() && proc.is_successful() && natural_voices());
    });
}

#[derive(Default)]
pub struct Speaker {
    pids: RefCell<Arc<Mutex<Vec<u32>>>>,
    system: std::cell::Cell<bool>,
}

impl Speaker {
    pub fn stop(&self) {
        let old = self.pids.replace(Arc::default());
        for pid in old.lock().unwrap().drain(..) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        if self.system.replace(false) {
            let _ = Command::new("spd-say").arg("-C").status();
        }
    }

    /// Text aus Markdown und LaTeX vorlesen, in der Stimme seiner Sprache. Läuft im
    /// Hintergrund, ein neuer Aufruf löst den alten ab.
    pub fn say(&self, md: &str) {
        self.stop();
        let lang = language_of(md);
        let text = spoken_in(md, lang);
        if text.is_empty() {
            return;
        }
        let (Some(bin), Some(model)) = (piper_bin(), voice_model(lang)) else {
            self.system.set(true);
            let _ = Command::new("spd-say").args(["-l", lang.code(), "-r", "10"]).arg(&text).status();
            return;
        };
        let rate = sample_rate(&model).to_string();
        let pids = self.pids.borrow().clone();
        std::thread::spawn(move || {
            // Bibliotheken liegen neben dem Programm
            let dir = bin.parent().map(PathBuf::from).unwrap_or_default();
            let Ok(mut voice) = Command::new(&bin)
                .args(["--output-raw", "--sentence_silence", "0.25", "--model"])
                .arg(&model)
                .env("LD_LIBRARY_PATH", &dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            let Some(out) = voice.stdout.take() else { return };
            let Ok(mut player) = Command::new("aplay").args(["-q", "-r", &rate, "-f", "S16_LE", "-t", "raw", "-"]).stdin(out).stderr(Stdio::null()).spawn() else {
                let _ = voice.kill();
                let _ = voice.wait();
                return;
            };
            pids.lock().unwrap().extend([voice.id(), player.id()]);
            if let Some(mut input) = voice.stdin.take() {
                let _ = input.write_all(text.as_bytes());
            }
            let _ = voice.wait();
            let _ = player.wait();
        });
    }
}

const GREEK: [(&str, &str); 24] = [
    ("alpha", "Alpha"), ("beta", "Beta"), ("gamma", "Gamma"), ("delta", "Delta"), ("epsilon", "Epsilon"), ("varepsilon", "Epsilon"),
    ("eta", "Eta"), ("theta", "Theta"), ("kappa", "Kappa"), ("lambda", "Lambda"), ("mu", "Mü"), ("nu", "Nü"),
    ("pi", "Pi"), ("rho", "Rho"), ("sigma", "Sigma"), ("tau", "Tau"), ("phi", "Phi"), ("varphi", "Phi"),
    ("psi", "Psi"), ("omega", "Omega"), ("Delta", "Delta"), ("Sigma", "Summe"), ("Omega", "Omega"), ("Phi", "Phi"),
];

/// Inhalt der geschweiften Klammer ab `i` (zeigt auf die öffnende); liefert Text und Ende.
fn group(c: &[char], i: usize) -> Option<(String, usize)> {
    if c.get(i) != Some(&'{') {
        return None;
    }
    let (mut depth, mut j) = (0, i);
    while j < c.len() {
        match c[j] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((c[i + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Formel in gesprochene Sprache übersetzen.
fn say_tex(lang: Lang, tex: &str) -> String {
    let c: Vec<char> = tex.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                let mut j = i + 1;
                while c.get(j).is_some_and(|x| x.is_ascii_alphabetic()) {
                    j += 1;
                }
                let name: String = c[i + 1..j].iter().collect();
                i = j.max(i + 2);
                match name.as_str() {
                    "frac" | "dfrac" | "tfrac" => {
                        if let Some((a, n)) = group(&c, i) {
                            if let Some((b, m)) = group(&c, n) {
                                out.push_str(&say!(lang, " {} durch {} ", " {} over {} ", say_tex(lang, &a), say_tex(lang, &b)));
                                i = m;
                            }
                        }
                    }
                    "sqrt" => {
                        if let Some((a, n)) = group(&c, i) {
                            out.push_str(&say!(lang, " Wurzel aus {} ", " square root of {} ", say_tex(lang, &a)));
                            i = n;
                        }
                    }
                    "text" | "mathrm" | "mathbf" | "bm" | "mathit" | "operatorname" | "overline" | "bar" | "vec" | "hat" | "dot" => {
                        if let Some((a, n)) = group(&c, i) {
                            out.push_str(&say_tex(lang, &a));
                            i = n;
                        }
                    }
                    "cdot" | "times" => out.push_str(pick(lang, " mal ", " times ")),
                    "approx" => out.push_str(pick(lang, " ungefähr ", " approximately ")),
                    "le" | "leq" => out.push_str(pick(lang, " kleiner gleich ", " less than or equal to ")),
                    "ge" | "geq" => out.push_str(pick(lang, " größer gleich ", " greater than or equal to ")),
                    "partial" => out.push_str(pick(lang, " partiell ", " partial ")),
                    "sum" => out.push_str(pick(lang, " Summe ", " sum ")),
                    "int" => out.push_str(pick(lang, " Integral ", " integral ")),
                    "infty" => out.push_str(pick(lang, " unendlich ", " infinity ")),
                    "to" | "rightarrow" => out.push_str(pick(lang, " nach ", " to ")),
                    other => {
                        if let Some((_, spoken)) = GREEK.iter().find(|g| g.0 == other) {
                            let spoken = match other {
                                "mu" => pick(lang, spoken, "mu"),
                                "nu" => pick(lang, spoken, "nu"),
                                "Sigma" => pick(lang, spoken, "sum"),
                                _ => spoken,
                            };
                            out.push_str(&format!(" {spoken} "));
                        }
                    }
                }
            }
            '^' | '_' => {
                let word = if c[i] == '^' { pick(lang, " hoch ", " to the power of ") } else { " " };
                i += 1;
                if let Some((a, n)) = group(&c, i) {
                    out.push_str(word);
                    out.push_str(&say_tex(lang, &a));
                    out.push(' ');
                    i = n;
                } else if let Some(x) = c.get(i) {
                    out.push_str(word);
                    out.push(*x);
                    out.push(' ');
                    i += 1;
                }
            }
            '=' => {
                out.push_str(pick(lang, " gleich ", " equals "));
                i += 1;
            }
            '+' => {
                out.push_str(" plus ");
                i += 1;
            }
            '-' => {
                out.push_str(" minus ");
                i += 1;
            }
            '/' => {
                out.push_str(pick(lang, " durch ", " over "));
                i += 1;
            }
            '{' | '}' | '&' => i += 1,
            x => {
                out.push(x);
                i += 1;
            }
        }
    }
    out
}

/// Markdown und Formeln in Sprechtext umwandeln; Marken wurden vorher entfernt.
#[cfg(test)]
pub fn spoken(md: &str) -> String {
    spoken_in(md, language_of(md))
}

fn spoken_in(md: &str, lang: Lang) -> String {
    let mut out = String::new();
    for para in crate::latex::parse(md) {
        for seg in &para.segs {
            match seg {
                crate::latex::Seg::Text { text, .. } => out.push_str(text),
                crate::latex::Seg::Math { tex, .. } => out.push_str(&say_tex(lang, tex)),
            }
        }
        out.push('\n');
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sprache eines Texts an häufigen kleinen Wörtern erkennen; ohne klares Ergebnis die der Oberfläche.
pub fn language_of(text: &str) -> Lang {
    const DE: [&str; 24] = ["der", "die", "das", "und", "ist", "nicht", "mit", "für", "ein", "eine", "zu", "von", "auf", "den", "dem", "sich", "auch", "wird", "wir", "es", "bei", "im", "oder", "wenn"];
    const EN: [&str; 24] = ["the", "and", "is", "of", "to", "in", "that", "for", "with", "are", "this", "it", "be", "on", "as", "by", "we", "an", "or", "if", "at", "not", "from", "which"];
    let (mut de, mut en) = (0, 0);
    for w in text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()) {
        let w = w.to_lowercase();
        de += DE.contains(&w.as_str()) as i32 + w.contains(['ä', 'ö', 'ü', 'ß']) as i32;
        en += EN.contains(&w.as_str()) as i32;
    }
    match de.cmp(&en) {
        std::cmp::Ordering::Greater => Lang::De,
        std::cmp::Ordering::Less => Lang::En,
        std::cmp::Ordering::Equal if crate::i18n::english() => Lang::En,
        std::cmp::Ordering::Equal => Lang::De,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_language() {
        assert!(language_of("Die Grenzen werden also zu 0 und 1, und das ist alles.") == Lang::De);
        assert!(language_of("The truth is exactly in the middle of the two.") == Lang::En);
        assert!(spoken(r"The ratio is $\frac{a}{b}$ and that is it.").contains("a over b"));
    }

    #[test]
    fn formulas_become_words() {
        assert_eq!(spoken(r"Es gilt $\frac{a}{b}$."), "Es gilt a durch b .");
        assert_eq!(spoken(r"$x^2 + \sigma_i$"), "x hoch 2 plus Sigma i");
        assert_eq!(spoken(r"$\sqrt{EA} = 1$"), "Wurzel aus EA gleich 1");
        assert_eq!(spoken("**fett** und\n\nweiter"), "fett und weiter");
    }
}
