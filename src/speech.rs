//! Vorlesen: Piper mit deutscher Stimme, solange sie nicht installiert ist die Systemstimme.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gtk::glib;

fn voice_dir() -> PathBuf {
    glib::user_data_dir().join("graphit").join("piper")
}

fn piper() -> Option<(PathBuf, PathBuf)> {
    let dir = voice_dir();
    let (bin, model) = (dir.join("piper").join("piper"), dir.join("de_DE-thorsten-medium.onnx"));
    (bin.exists() && model.exists()).then_some((bin, model))
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

    /// Text aus Markdown und LaTeX vorlesen. Läuft im Hintergrund, ein neuer Aufruf löst den alten ab.
    pub fn say(&self, md: &str) {
        self.stop();
        let text = spoken(md);
        if text.is_empty() {
            return;
        }
        let Some((bin, model)) = piper() else {
            self.system.set(true);
            let _ = Command::new("spd-say").args(["-l", "de", "-r", "10"]).arg(&text).status();
            return;
        };
        let pids = self.pids.borrow().clone();
        std::thread::spawn(move || {
            // Bibliotheken liegen neben dem Programm
            let dir = bin.parent().map(PathBuf::from).unwrap_or_default();
            let Ok(mut voice) = Command::new(&bin)
                .args(["--output-raw", "--model"])
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
            let Ok(mut player) = Command::new("aplay").args(["-q", "-r", "22050", "-f", "S16_LE", "-t", "raw", "-"]).stdin(out).stderr(Stdio::null()).spawn() else {
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
fn say_tex(tex: &str) -> String {
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
                                out.push_str(&format!(" {} durch {} ", say_tex(&a), say_tex(&b)));
                                i = m;
                            }
                        }
                    }
                    "sqrt" => {
                        if let Some((a, n)) = group(&c, i) {
                            out.push_str(&format!(" Wurzel aus {} ", say_tex(&a)));
                            i = n;
                        }
                    }
                    "text" | "mathrm" | "mathbf" | "bm" | "mathit" | "operatorname" | "overline" | "bar" | "vec" | "hat" | "dot" => {
                        if let Some((a, n)) = group(&c, i) {
                            out.push_str(&say_tex(&a));
                            i = n;
                        }
                    }
                    "cdot" | "times" => out.push_str(" mal "),
                    "approx" => out.push_str(" ungefähr "),
                    "le" | "leq" => out.push_str(" kleiner gleich "),
                    "ge" | "geq" => out.push_str(" größer gleich "),
                    "partial" => out.push_str(" partiell "),
                    "sum" => out.push_str(" Summe "),
                    "int" => out.push_str(" Integral "),
                    "infty" => out.push_str(" unendlich "),
                    "to" | "rightarrow" => out.push_str(" nach "),
                    other => {
                        if let Some((_, spoken)) = GREEK.iter().find(|g| g.0 == other) {
                            out.push_str(&format!(" {spoken} "));
                        }
                    }
                }
            }
            '^' | '_' => {
                let word = if c[i] == '^' { " hoch " } else { " " };
                i += 1;
                if let Some((a, n)) = group(&c, i) {
                    out.push_str(word);
                    out.push_str(&say_tex(&a));
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
                out.push_str(" gleich ");
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
                out.push_str(" durch ");
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
pub fn spoken(md: &str) -> String {
    let mut out = String::new();
    for para in crate::latex::parse(md) {
        for seg in &para.segs {
            match seg {
                crate::latex::Seg::Text { text, .. } => out.push_str(text),
                crate::latex::Seg::Math { tex, .. } => out.push_str(&say_tex(tex)),
            }
        }
        out.push('\n');
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_become_words() {
        assert_eq!(spoken(r"Es gilt $\frac{a}{b}$."), "Es gilt a durch b .");
        assert_eq!(spoken(r"$x^2 + \sigma_i$"), "x hoch 2 plus Sigma i");
        assert_eq!(spoken(r"$\sqrt{EA} = 1$"), "Wurzel aus EA gleich 1");
        assert_eq!(spoken("**fett** und\n\nweiter"), "fett und weiter");
    }
}
