//! Claude im Fenster: Esc öffnet eine Leiste über dem Dock, die Antwort wächst darüber
//! auf. Pro Modul (Überordner) läuft eine eigene Claude-Code-Session im Modulordner.
//! Graphit legt die aktuelle Seite und die ganze Notiz als PDF ab; Claude liest sie nur,
//! wenn die Frage es braucht. Mit einer Führung zeigt Claude Schritt für Schritt auf
//! Stellen in der Notiz oder den Lese-Viewern; dazu erscheint eine Lernkarte daneben.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use serde_json::{json, Value};

use crate::canvas::{ink_lines, snap_to_ink, Mark, MarkKind};
use crate::docview::DocView;
use crate::geom::{Pt, Rect};
use crate::index;
use crate::place;
use crate::plot::{self, Plot};
use crate::speech::Speaker;
use crate::latex::{self, Engine, Rich};

const SEITE_PDF: &str = "aktuelle-seite.pdf";
const NOTIZ_PDF: &str = "notiz.pdf";
const TOUR_FILE: &str = "fuehrung.json";
const INK_FILE: &str = "striche.json";
const MAX_HISTORY: usize = 300;
const MEMORY_FILE: &str = "gedaechtnis.md";
const CONTEXT_FILE: &str = "kontext";
/// Ab dieser Kontextgröße wird das Gespräch gleich nach der Antwort zusammengefasst.
/// Rund 20.000 Tokens davon sind Claude Codes fester Sockel.
/// So weit muss sich die Spitze bewegen, bis die Lernkarte mitgeht.
const CARD_DRAG_START: f64 = 7.0;
const COMPACT_AT: u32 = 100_000;
/// Ab hier nach einer Pause: kurz bevor der Cache abläuft, ist das Zusammenfassen billig,
/// danach müsste die nächste Frage das ganze Gespräch neu einlesen.
const COMPACT_IDLE_AT: u32 = 70_000;
const COMPACT_IDLE: Duration = Duration::from_secs(240);
const COMPACT_PROMPT: &str = "Graphit räumt dieses Gespräch auf, damit es klein bleibt. Schreibe eine Zusammenfassung für dich selbst, mit der du in einem neuen Gespräch nahtlos weitermachen kannst: Thema und Stand, was schon erklärt wurde, offene Fragen und Aufgaben, Vorlieben des Nutzers, wichtige Dateien und Seiten des Moduls. Lass Regeln und Pfade weg, die Graphit ohnehin zu jeder Frage nennt. Höchstens 200 Wörter in Stichpunkten, nur die Zusammenfassung selbst. Benutze keine Werkzeuge und schreibe keine Führung.";

/// Anzeigename und Kürzel für `claude --model`.
pub const MODELS: [(&str, &str); 4] = [("Fable", "fable"), ("Opus", "opus"), ("Sonnet", "sonnet"), ("Haiku", "haiku")];
/// Anzeigename und Wert für `claude --effort`; leer = Vorgabe des Modells.
pub const EFFORTS: [(&str, &str); 6] =
    [("Standard", ""), ("Niedrig", "low"), ("Mittel", "medium"), ("Hoch", "high"), ("Sehr hoch", "xhigh"), ("Maximal", "max")];

// ------------------------------------------------------------------------ Modul

/// Ordner der Session: `<Semester>/<Modul>`, sonst der oberste Ordner unter der
/// Bibliothek. Dateien außerhalb der Bibliothek nutzen ihren eigenen Ordner.
pub fn module_dir(note: Option<&Path>, root: &Path) -> PathBuf {
    let Some(parent) = note.and_then(|n| n.parent()) else { return root.to_path_buf() };
    let Ok(rel) = parent.strip_prefix(root) else { return parent.to_path_buf() };
    let parts: Vec<_> = rel.components().collect();
    let depth = match parts.first() {
        None => 0,
        Some(c) if c.as_os_str().to_string_lossy().to_lowercase().contains("semester") => parts.len().min(2),
        Some(_) => 1,
    };
    parts.iter().take(depth).fold(root.to_path_buf(), |p, c| p.join(c))
}

fn store_key(dir: &Path) -> String {
    dir.to_string_lossy().trim_matches('/').replace('/', "_")
}

fn data_dir(dir: &Path) -> PathBuf {
    glib::user_data_dir().join("graphit").join("claude").join(store_key(dir))
}

fn cache_dir(dir: &Path) -> PathBuf {
    glib::user_cache_dir().join("graphit").join("claude").join(store_key(dir))
}

// ------------------------------------------------------------------------ Ereignisse

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Session(String),
    /// neuer Textabschnitt der Antwort
    TextStart,
    Text(String),
    /// Werkzeug läuft; `lasting` bleibt im Verlauf stehen (Änderungen, Befehle)
    Tool { text: String, lasting: bool },
    /// Tokens, die der letzte Aufruf mitgeschickt hat
    Context(u32),
    Finished { error: Option<String> },
}

fn short_path(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

fn tool_event(name: &str, input: &Value) -> Event {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let (text, lasting) = match name {
        "Read" => (crate::tr!("Liest {}", "Reading {}", short_path(s("file_path"))), false),
        "Write" => (crate::tr!("Schreibt {}", "Writing {}", short_path(s("file_path"))), true),
        "Edit" | "MultiEdit" | "NotebookEdit" => (crate::tr!("Ändert {}", "Editing {}", short_path(s("file_path"))), true),
        "Bash" => {
            let d = if s("description").is_empty() { s("command") } else { s("description") };
            let line = d.lines().next().unwrap_or("");
            let short: String = line.chars().take(70).collect();
            (crate::tr!("Führt aus: {short}{}", "Running: {short}{}", if short.len() < line.len() { " …" } else { "" }), true)
        }
        "Grep" | "Glob" => (crate::tr!("Sucht {}", "Searching {}", s("pattern")), false),
        other => (other.to_string(), false),
    };
    Event::Tool { text, lasting }
}

/// Eine Zeile von `--output-format stream-json` in Ereignisse übersetzen.
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(d) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let nested = d.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
    match d["type"].as_str().unwrap_or("") {
        "system" if d["subtype"] == "init" => d["session_id"].as_str().map(|s| vec![Event::Session(s.into())]).unwrap_or_default(),
        "stream_event" if !nested => {
            let e = &d["event"];
            match e["type"].as_str().unwrap_or("") {
                "content_block_start" if e["content_block"]["type"] == "text" => vec![Event::TextStart],
                "content_block_delta" if e["delta"]["type"] == "text_delta" => {
                    e["delta"]["text"].as_str().map(|t| vec![Event::Text(t.into())]).unwrap_or_default()
                }
                _ => Vec::new(),
            }
        }
        "assistant" if !nested => {
            let u = &d["message"]["usage"];
            let used: u64 = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].iter().filter_map(|k| u[k].as_u64()).sum();
            let tools = d["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use");
            let tools = tools.map(|b| tool_event(b["name"].as_str().unwrap_or(crate::i18n::tl("Werkzeug")), &b["input"]));
            (used > 0).then_some(Event::Context(used as u32)).into_iter().chain(tools).collect()
        }
        "result" => {
            // ohne Text steht die Ursache in stderr; die ergänzt `launch`
            let error = d["is_error"].as_bool().unwrap_or(false).then(|| d["result"].as_str().unwrap_or("").to_string());
            vec![Event::Finished { error }]
        }
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------------------ Claude Code starten

fn claude_binary() -> PathBuf {
    glib::find_program_in_path("claude").unwrap_or_else(|| glib::home_dir().join(".local/bin/claude"))
}

const SYSTEM_PROMPT: &str = r#"Du läufst eingebettet in Graphit, einer App für handschriftliche Notizen. Der Nutzer studiert, dein Arbeitsordner ist ein Studienmodul mit seinen Notizen und Unterlagen. Deine Antwort erscheint in einem kleinen Dialog.

Grundregeln:
- Antworte knapp und immer in der Sprache der eigentlichen Nutzerfrage, auch in Führungen, Quiz und Einträgen. Dieser Kontext und die Sprache der Notizen zählen dabei nicht.
- Schreibe Formeln als LaTeX: $…$ im Text, $$…$$ abgesetzt. Graphit setzt sie mit pdflatex (amsmath, mathtools, bm), auch in Tabellen, Führungen und Einfüge-Texten. Mehrzeiliges mit aligned oder cases.
- Zu jeder Anfrage nennt Graphit die aktuelle Seite und die ganze Notiz als PDF sowie die Lese-Viewer. Handschrift ist nur in diesen PDFs lesbar.
- Bearbeite .xopp-Dateien nie direkt: Graphit hat sie offen und würde deine Änderung überschreiben.
- Soll etwas dauerhaft in die Notiz (ausfüllen, eintragen, ergänzen, beschriften, markieren, skizzieren, Lösung aufschreiben), dann tu es wirklich dort, an der richtigen Stelle, statt es nur im Dialog zu schreiben. Du kannst Text, Formeln, Bilder, Linien und Marker frei auf jeder Seite platzieren. Wie das geht, steht in CACHE/einfuegen-anleitung.md: einmal lesen, sobald du es brauchst. Tu das nur, wenn der Nutzer es möchte.

Unterlagen: CACHE/index/verzeichnis.txt listet alle PDFs des Moduls und seine Altklausuren (D1, D2, … mit Art). CACHE/index/D1.txt enthält den Text, jede Zeile beginnt mit S<Seite>:. Suche dort mit grep, statt PDFs zu öffnen. Stützt du dich auf so eine Stelle, setze ans Ende der Antwort oder Lernkarte ⟦D7 S42⟧, höchstens drei; das wird ein Knopf zur Quelle. Nenne die Kennungen nie im Fließtext.
Lernstand: Zeigt sich klar, etwa an Quizantworten oder Rückfragen, dass ein Thema sitzt oder wackelt, ergänze ⟦thema: Name | sicher⟧ oder ⟦thema: Name | wackelig⟧. Kurze, wiederverwendbare Namen, selten.

Handschrift: Liegt CACHE/abschrift/S<Seite>.md vor, lies die Abschrift statt des PDFs. Musstest du eine Seite mit Handschrift aus dem PDF lesen, schreibe vor dem Ende ihre Abschrift (Text mit LaTeX) dorthin; Graphit merkt sie sich.

Zeigen ist der Normalfall:
Bezieht sich deine Antwort auf etwas Sichtbares (Aufgabe, Formel, Rechnung, Skizze, Tabelle, Textstelle in Notiz oder Viewer) oder erklärst du etwas in mehreren Schritten, dann erstelle ungefragt eine Führung. Der Nutzer muss nie darum bitten. Wörter wie "das", "hier", "diese Aufgabe", "die nächste Seite" meinen die Seite: öffne sie und führe darüber. Keine Führung nur bei reinen Kurz-, Wissens- oder Organisationsfragen ohne Bezug zu einer Seite. Mit Führung ist deine Antwort im Dialog nur ein Satz, die Erklärung steht in den Schritten.

Eine Führung ist die Datei CACHE/fuehrung.json, die du vor dem Ende deiner Antwort schreibst:
{"schritte":[{
  "text":"Ein bis zwei kurze Sätze, erscheint als Lernkarte neben der Stelle.",
  "ziel":"notiz",            // notiz, viewer1 (Lese-Viewer oben), viewer2 (unten)
  "seite":1,                 // ab 1
  "marken":[                 // beliebig viele Zeichen auf dieser Seite
    {"zeichen":"kreis","suche":"wörtlicher PDF-Text","text":"kurzes Schild"},
    {"zeichen":"rahmen","bereich":[0.1,0.2,0.5,0.3]}
  ],
  "verbinde":[[0,1]],        // Linie von marken[0] zu marken[1]: zeigt Zusammenhänge
  "skizze":[[[0.2,0.5],[0.4,0.5],[0.4,0.3]]],  // Linienzüge, zeichnen sich sichtbar
  "tipp":"Hilfe, die erst auf Knopfdruck erscheint",
  "quiz":{"optionen":["A","B","C"],"richtig":1,"erklaerung":"Warum B stimmt"},
  "aufgabe":"Was der Nutzer selbst auf der Seite rechnen soll",
  "terme":[{"bereich":[0.2,0.3,0.28,0.36],"name":"$\\frac{EA}{L}$","erklaerung":"Steifigkeit des Stabs"}],
  "diagramm":{"titel":"Spannung","x":{"name":"Dehnung","min":0,"max":0.01},"y":{"name":"σ in MPa"},
    "kurven":[{"name":"Stahl","formel":"E*x"}],
    "regler":[{"symbol":"E","name":"E-Modul","einheit":"MPa","min":70000,"max":250000,"wert":210000}]}
}],
"vorschlaege":["Rechne mir ein Beispiel vor","Warum gilt das nur für ideale Gase?"]}

Alles außer text ist freiwillig. Die Datei darf auch nur vorschlaege enthalten.
- zeichen: kreis, markierung, unterstreichen, pfeil, rahmen, fokus (dunkelt alles andere ab, für den einen Punkt, auf den es ankommt).
- suche trifft exakt und ist immer vorzuziehen; hole den Wortlaut bei PDFs mit pdftotext. bereich nur für Handschrift: [x0,y0,x1,y1] als Anteile der Seite von 0 bis 1; CACHE/striche.json nennt die Schreibzeilen je Seite, daran rastet Graphit ein.
- skizze in Seitenanteilen, für Freikörperbilder, Diagramme, Kurvenverläufe, fehlende Hilfslinien. Der Nutzer kann sie übernehmen.
- quiz nach einem Kerngedanken, damit der Nutzer aktiv nachdenkt, statt nur zu lesen. Falsche Optionen sind typische Denkfehler.
- aufgabe, wenn Selbermachen mehr bringt als Zuschauen. Der Nutzer rechnet auf der Seite und tippt auf Prüfen; dann bekommst du die aktuelle Seite und gibst Rückmeldung, bei Fehlern mit Führung genau auf seine Stelle.
- tipp statt der Lösung, wenn der Nutzer selbst darauf kommen kann.
- terme schlüsseln eine Formel auf der Seite auf: Jeder Term bekommt einen farbigen Rahmen und einen Knopf auf der Karte, der seine erklaerung zeigt. Ein grober bereich reicht, Graphit rastet am Inhalt ein; suche geht auch. Eine Herleitung läuft über mehrere Schritte, je Schritt eine Zeile und ihre terme.
- diagramm, wenn ein Zusammenhang durchs Ausprobieren klarer wird. formel nutzt x, die symbol der regler, + - * / ^, sin cos tan exp ln log sqrt abs, pi, e. y ohne min und max skaliert sich selbst.
- verbinde, wenn zwei Stellen derselben Seite zusammengehören (Formel und Größe, Annahme und Folge).
Nimm 2 bis 6 Schritte. Mische die Mittel nach Nutzen, höchstens ein Quiz und eine Aufgabe je Führung, außer der Nutzer will üben. vorschlaege: zwei bis drei sinnvolle nächste Fragen in Ich-Form des Nutzers, gib sie fast immer mit."#;

fn system_prompt(cache: &Path) -> String {
    SYSTEM_PROMPT.replace("CACHE", &cache.display().to_string())
}

struct Launched {
    events: async_channel::Receiver<Event>,
    pid: u32,
}

fn launch(dir: &Path, cache: &Path, prompt: &str, session: Option<&str>, model: &str, effort: &str) -> std::io::Result<Launched> {
    let mut cmd = Command::new(claude_binary());
    cmd.current_dir(dir)
        .arg("-p")
        .arg(prompt)
        .args(["--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--permission-mode", "acceptEdits", "--allowedTools", "Bash"])
        // nur was Graphit braucht: übrige Werkzeuge, Skills und MCP kosten bei jedem Aufruf
        // rund 14.000 Tokens und kamen hier nie zum Einsatz. Die Nutzereinstellungen bleiben,
        // sie kosten nichts und enthalten seine Sperrregeln für Schlüssel und .env-Dateien.
        .args(["--tools", "Bash,Read,Write,Edit", "--strict-mcp-config", "--disable-slash-commands"])
        .args(["--model", model])
        .arg("--add-dir")
        .arg(cache)
        .arg("--append-system-prompt")
        .arg(system_prompt(cache));
    if !effort.is_empty() {
        cmd.args(["--effort", effort]);
    }
    if let Some(s) = session {
        cmd.args(["--resume", s]);
    }
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = async_channel::unbounded();
    let err_text = Arc::new(Mutex::new(String::new()));
    let err_sink = err_text.clone();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        *err_sink.lock().unwrap() = s;
    });
    std::thread::spawn(move || {
        let mut finished = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            for ev in parse_line(&line) {
                if ev == (Event::Finished { error: Some(String::new()) }) {
                    continue;
                }
                finished |= matches!(ev, Event::Finished { .. });
                let _ = tx.send_blocking(ev);
            }
        }
        let status = child.wait();
        let _ = err_thread.join();
        if !finished {
            let err = err_text.lock().unwrap().trim().to_string();
            let error = match status {
                Ok(s) if s.success() && err.is_empty() => None,
                _ if err.is_empty() => Some(crate::i18n::tl("Abgebrochen").to_string()),
                _ => Some(err),
            };
            let _ = tx.send_blocking(Event::Finished { error });
        }
    });
    Ok(Launched { events: rx, pid })
}

// ------------------------------------------------------------------------ Gespräch

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    User,
    Claude,
    Tool,
    Error,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Claude => "claude",
            Kind::Tool => "tool",
            Kind::Error => "error",
        }
    }
    fn parse(s: &str) -> Kind {
        match s {
            "user" => Kind::User,
            "tool" => Kind::Tool,
            "error" => Kind::Error,
            _ => Kind::Claude,
        }
    }
}

struct Msg {
    kind: Kind,
    text: String,
}

/// Ein Gespräch pro Modul; läuft weiter, auch wenn der Dialog zu ist.
struct Conv {
    dir: PathBuf,
    name: String,
    msgs: RefCell<Vec<Msg>>,
    session: RefCell<Option<String>>,
    pid: Cell<Option<u32>>,
    /// vom Nutzer angehalten
    stopped: Cell<bool>,
    status: RefCell<String>,
    /// fertige Antwort, die noch niemand gesehen hat
    unread: Cell<bool>,
    /// (Dokument, Stand), für den die PDFs aktuell sind
    exported: RefCell<Option<(Option<PathBuf>, u64)>>,
    exporting: Rc<Cell<u32>>,
    /// läuft gerade ein Vorlesungsdurchgang, der eine Führung liefern muss
    lecture: Cell<bool>,
    /// zweiter Versuch, nachdem die Führung gefehlt hat
    repairing: Cell<bool>,
    /// neue Schritte kommen hinter diesen Schritt der laufenden Führung
    insert_at: Cell<Option<usize>>,
    /// Führung, die wartet, bis ihr Tab wieder offen ist
    pending_tour: RefCell<(Vec<Step>, Vec<String>)>,
    /// Tokens, die der letzte Aufruf mitgeschickt hat
    context: Cell<u32>,
    /// Zusammenfassung früherer Gespräche; geht mit der ersten Frage einer neuen Session mit
    memory: RefCell<Option<String>>,
    /// zählt die Fragen, damit eine neue Frage die Pause beendet
    serial: Cell<u32>,
    /// Modell und Effort der letzten Antwort: nur damit trifft das Zusammenfassen den Cache
    last_run: Cell<Option<(&'static str, &'static str)>>,
    compacting: Cell<bool>,
    /// Quellen hinter den Kennungen D1, D2, …
    sources: RefCell<HashMap<String, index::Source>>,
    indexed: Cell<bool>,
    has_exams: Cell<bool>,
    /// Index in STYLES, pro Modul gemerkt
    style: Cell<usize>,
    /// Quizantworten seit der letzten Frage
    quiz_log: RefCell<Vec<String>>,
    /// Serials der Einfügungen, damit Claude genau die zurücknehmen kann
    last_batch: RefCell<Vec<u64>>,
    /// während des Zusammenfassens abgeschickt: danach senden
    send_after: Cell<bool>,
}

impl Conv {
    fn load(dir: &Path, root: &Path) -> Rc<Conv> {
        let data = data_dir(dir);
        let session = std::fs::read_to_string(data.join("session")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let msgs = std::fs::read_to_string(data.join("verlauf.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| Msg { kind: Kind::parse(v["kind"].as_str().unwrap_or("")), text: v["text"].as_str().unwrap_or("").to_string() })
            .collect();
        let name = if dir == root { crate::i18n::tl("Alle Notizen").to_string() } else { short_path(&dir.to_string_lossy()) };
        Rc::new(Conv {
            dir: dir.to_path_buf(),
            name,
            msgs: RefCell::new(msgs),
            session: RefCell::new(session),
            pid: Cell::new(None),
            stopped: Cell::new(false),
            status: RefCell::new(String::new()),
            unread: Cell::new(false),
            exported: RefCell::new(None),
            exporting: Rc::new(Cell::new(0)),
            lecture: Cell::new(false),
            repairing: Cell::new(false),
            insert_at: Cell::new(None),
            pending_tour: RefCell::new(Default::default()),
            context: Cell::new(std::fs::read_to_string(data.join(CONTEXT_FILE)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)),
            memory: RefCell::new(std::fs::read_to_string(data.join(MEMORY_FILE)).ok().filter(|m| !m.trim().is_empty())),
            serial: Cell::new(0),
            last_run: Cell::new(None),
            compacting: Cell::new(false),
            sources: RefCell::new(HashMap::new()),
            indexed: Cell::new(false),
            has_exams: Cell::new(false),
            style: Cell::new(std::fs::read_to_string(data.join(STYLE_FILE)).ok().and_then(|s| s.trim().parse().ok()).filter(|i| *i < STYLES.len()).unwrap_or(0)),
            quiz_log: RefCell::new(Vec::new()),
            last_batch: RefCell::new(Vec::new()),
            send_after: Cell::new(false),
        })
    }

    fn set_memory(&self, text: Option<String>) {
        let file = data_dir(&self.dir).join(MEMORY_FILE);
        match &text {
            Some(t) => {
                let _ = std::fs::write(file, t);
            }
            None => {
                let _ = std::fs::remove_file(file);
            }
        }
        self.memory.replace(text);
    }

    fn save(&self) {
        let data = data_dir(&self.dir);
        let _ = std::fs::create_dir_all(&data);
        let _ = std::fs::write(data.join("session"), self.session.borrow().clone().unwrap_or_default());
        let _ = std::fs::write(data.join(CONTEXT_FILE), self.context.get().to_string());
        let mut msgs = self.msgs.borrow_mut();
        let extra = msgs.len().saturating_sub(MAX_HISTORY);
        msgs.drain(..extra);
        let lines: Vec<String> = msgs.iter().map(|m| json!({"kind": m.kind.key(), "text": m.text}).to_string()).collect();
        let _ = std::fs::write(data.join("verlauf.jsonl"), lines.join("\n"));
    }

    fn push(&self, kind: Kind, text: impl Into<String>) {
        self.msgs.borrow_mut().push(Msg { kind, text: text.into() });
    }

    fn stop(&self) {
        if let Some(pid) = self.pid.get().filter(|p| *p != 0) {
            self.stopped.set(true);
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
}

/// Gespeicherte Führung: Beschriftung und Inhalt von fuehrung.json.
fn saved_tours(dir: &Path, note: &Path, page: usize) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(data_dir(dir).join(TOURS_FILE)).unwrap_or_default();
    let note = note.to_string_lossy();
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for v in text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (Some(title), Some(raw)) = (v["titel"].as_str(), v["fuehrung"].as_str()) else { continue };
        if v["notiz"].as_str() == Some(note.as_ref()) && v["seite"].as_u64() == Some(page as u64) && !seen.contains(&title.to_string()) {
            seen.push(title.to_string());
            out.push((title.to_string(), raw.to_string()));
            if out.len() == TOURS_SHOWN {
                break;
            }
        }
    }
    out
}

fn save_tour(dir: &Path, note: &Path, page: usize, title: &str, raw: &str) {
    let data = data_dir(dir);
    let _ = std::fs::create_dir_all(&data);
    let file = data.join(TOURS_FILE);
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    let mut lines: Vec<String> = old.lines().map(String::from).collect();
    lines.push(json!({"notiz": note.to_string_lossy(), "seite": page, "titel": title, "fuehrung": raw}).to_string());
    let extra = lines.len().saturating_sub(MAX_TOURS);
    let _ = std::fs::write(file, lines[extra..].join("\n"));
}

/// Schild auf der Seite: dort gibt es kein LaTeX.
fn strip_math(s: &str) -> String {
    s.replace('$', "").replace("\\", "")
}

const LECTURE_GUIDE_FILE: &str = "vorlesung-anleitung.md";
const LECTURE_KIND_FILE: &str = "vorlesung-art";

/// Varianten des Vorlesungsdurchgangs: Name, Beschreibung, Anweisung an Claude.
const LECTURE_KINDS: [(&str, &str, &str); 4] = [
    ("Grundverständnis", "Intuition und roter Faden, jede Idee an einem Beispiel", "Variante Grundverständnis: Intuition und roter Faden vor Formalismus. Jede Idee an einem kleinen konkreten Beispiel, Formeln nur soweit nötig, keine Beweise."),
    ("Tiefe Theorie", "Herleitungen, Annahmen und warum etwas gilt", "Variante Tiefe Theorie: Herleitungen Schritt für Schritt, Annahmen, Beweisideen, warum etwas gilt und wann es scheitert. Nimm jede zentrale Formel mit terme auseinander."),
    ("Prüfungsvorbereitung", "Rechenschemata, typische Aufgaben und Fehler", "Variante Prüfungsvorbereitung: Rechenschemata, typische Aufgabentypen, häufige Fehler. Nutze Altklausuren aus dem Index mit Quelle, wenn es welche gibt. Viele aufgabe und quiz."),
    ("Schnellüberblick", "Die wichtigsten Aussagen in zwei bis drei Minuten", "Variante Schnellüberblick: Nach der Landkarte nur 3 bis 5 Schritte mit den wichtigsten Aussagen, keine aufgabe."),
];

/// Liegt als Datei im Cache und wird nur im Vorlesungsmodus gelesen, das spart Kontext.
const LECTURE_GUIDE: &str = r#"# Interaktiver Vorlesungsdurchgang

Das Ergebnis ist ausschließlich die Datei CACHE/fuehrung.json. Der Nutzer will keinen Fließtext: Erkläre nichts im Chat. Deine Antwort besteht aus höchstens einem Satz und der Marke ⟦weiter: N⟧ (erste Seite des nächsten Abschnitts) oder ⟦weiter: ende⟧.

Lies die Seiten mit pdftotext, damit suche wörtlich trifft. Aufbau wie im Systemprompt beschrieben, mit diesen Ergänzungen:

## Ablauf (8 bis 14 Schritte, außer die Variante sagt anderes)
1. Der erste Schritt hat immer eine landkarte. Danach geht es Idee für Idee durch das Skript.
2. Jeder Schritt zeigt auf genau eine Stelle (marken mit suche) und hat höchstens zwei kurze Sätze in text. Eine Idee pro Schritt.
3. Mindestens die Hälfte der Schritte hat denken: Der Nutzer überlegt zuerst, text erscheint erst danach. Frage nach dem nächsten logischen Schritt, einer Vorhersage oder dem Warum, nie nach reinem Abschreiben.
4. Zentrale Formeln bekommen terme: jeder Term einzeln mit Bedeutung und Herkunft, statt die Formel im Fließtext zu erklären.
5. Hängt etwas von einem Parameter ab oder läuft etwas schrittweise oder über die Zeit ab, gehört ein diagramm dazu, mit regler, möglichst mit folge oder animiere.
6. Der letzte Schritt hat quiz oder aufgabe.

## Neue Felder eines Schritts
"denken": {"frage":"Was passiert mit dem Fehler, wenn h halbiert wird?","optionen":["Halbiert sich","Viertelt sich"],"richtig":0}
optionen und richtig sind freiwillig; ohne optionen deckt ein Knopf die Erklärung auf. Text und Diagramm erscheinen erst danach, also nimm die Antwort in frage und optionen nicht vorweg.

"landkarte": {"ideen":[{"id":"a","titel":"Zeitgitter","text":"Ein Satz zur Idee.","seite":1,"suche":"wörtlicher PDF-Text"}],"verbindungen":[["a","b","liefert"]]}
Höchstens 8 Ideen, titel höchstens drei Wörter. Die verbindungen sagen, wie die Ideen aufeinander aufbauen, mit einem kurzen Wort. Tippt der Nutzer eine Idee an, springt Graphit zu ihrer Stelle.

## Animierte Diagramme
Im diagramm zusätzlich:
"animiere":"h"  ein Reglersymbol: Der Abspielen-Knopf fährt diesen Regler einmal durch.
"folge":{"name":"Euler","start":[0,1],"x":"x+h","y":"y+h*y","schritte":"1/h"}  Punkte, die sich Schritt für Schritt aufbauen, für Zeitschrittverfahren, Iterationen und Rekursionen. In x, y und schritte stehen die Formelsprache, die Reglersymbole, x und y (aktueller Punkt) und k (Schrittnummer ab 0). Passt die exakte Lösung, gehört sie als kurven dazu, damit der Unterschied sichtbar wird. kurven darf bei einer folge fehlen.
"#;

fn short_title(q: &str) -> String {
    let line = q.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(44).collect();
    if cut.len() < line.len() { format!("{cut} …") } else { cut }
}

/// Woher eine Anfrage kam: dort landet, was Claude in die Notiz einfügen soll.
#[derive(Clone)]
struct Origin {
    view: Option<Weak<DocView>>,
    page: usize,
}

// ------------------------------------------------------------------------ Führung

#[derive(Clone, Debug, PartialEq)]
struct MarkSpec {
    kind: MarkKind,
    find: Option<String>,
    area: Option<[f64; 4]>,
    label: Option<String>,
}

/// Teil einer Formel, auf der Seite farbig eingerahmt.
#[derive(Clone, Debug, PartialEq)]
struct Term {
    find: Option<String>,
    area: Option<[f64; 4]>,
    name: String,
    why: Option<String>,
}

/// Farben der Terme, passend zu den Punkten `.fk-term-N` auf der Karte.
const TERM_COLORS: [(f32, f32, f32); 6] = [(0.85, 0.17, 0.23), (0.12, 0.37, 0.82), (0.12, 0.60, 0.31), (0.88, 0.49, 0.05), (0.49, 0.27, 0.78), (0.08, 0.64, 0.63)];

#[derive(Clone, Debug, PartialEq)]
struct Quiz {
    options: Vec<String>,
    right: usize,
    why: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    text: String,
    target: String,
    /// Seite ab 0
    page: Option<usize>,
    marks: Vec<MarkSpec>,
    links: Vec<(usize, usize)>,
    sketch: Vec<Vec<[f64; 2]>>,
    tip: Option<String>,
    quiz: Option<Quiz>,
    task: Option<String>,
    terms: Vec<Term>,
    plot: Option<Plot>,
    think: Option<Think>,
    map: Option<IdeaMap>,
}

/// Erst überlegen, dann erscheint der Text des Schritts.
#[derive(Clone, Debug, PartialEq)]
struct Think {
    question: String,
    options: Vec<String>,
    right: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Idea {
    id: String,
    title: String,
    text: String,
    page: Option<usize>,
    mark: Option<MarkSpec>,
}

/// Landkarte eines Abschnitts: Ideen und wie sie aufeinander aufbauen.
#[derive(Clone, Debug, PartialEq)]
struct IdeaMap {
    ideas: Vec<Idea>,
    /// Von, nach (Index in ideas) und Beschriftung
    links: Vec<(usize, usize, String)>,
}

fn parse_think(v: &Value) -> Option<Think> {
    let options: Vec<String> = v["optionen"].as_array().map(|o| o.iter().filter_map(non_empty).collect()).unwrap_or_default();
    Some(Think { question: non_empty(&v["frage"])?, right: v["richtig"].as_u64().map(|r| r as usize).filter(|r| *r < options.len()), options })
}

fn parse_map(v: &Value) -> Option<IdeaMap> {
    let ideas: Vec<Idea> = v["ideen"]
        .as_array()?
        .iter()
        .take(MAX_IDEAS)
        .filter_map(|i| {
            Some(Idea {
                id: non_empty(&i["id"])?,
                title: non_empty(&i["titel"])?,
                text: non_empty(&i["text"]).unwrap_or_default(),
                page: i["seite"].as_u64().map(|p| (p as usize).saturating_sub(1)),
                mark: parse_mark(&json!({"zeichen": "rahmen", "suche": i["suche"], "bereich": i["bereich"]})),
            })
        })
        .collect();
    if ideas.is_empty() {
        return None;
    }
    let at = |v: &Value| v.as_str().and_then(|id| ideas.iter().position(|i| i.id == id));
    let links = v["verbindungen"].as_array().into_iter().flatten().filter_map(|l| Some((at(&l[0])?, at(&l[1])?, l[2].as_str().unwrap_or("").to_string()))).filter(|l| l.0 != l.1).collect();
    Some(IdeaMap { ideas, links })
}

/// Ordnet Ideen in Zeilen: eine Idee steht unter allen, auf denen sie aufbaut.
fn idea_rows(map: &IdeaMap) -> Vec<Vec<usize>> {
    let mut depth = vec![0usize; map.ideas.len()];
    // begrenzte Durchläufe: Kreise in den Verbindungen dürfen nicht hängen bleiben
    for _ in 0..map.ideas.len() {
        for (a, b, _) in &map.links {
            if depth[*b] <= depth[*a] && depth[*a] + 1 < map.ideas.len() {
                depth[*b] = depth[*a] + 1;
            }
        }
    }
    let mut rows = Vec::new();
    for level in 0..=depth.iter().copied().max().unwrap_or(0) {
        let row: Vec<usize> = (0..map.ideas.len()).filter(|i| depth[*i] == level).collect();
        rows.extend(row.chunks(IDEAS_PER_ROW).map(<[usize]>::to_vec));
    }
    rows.retain(|r| !r.is_empty());
    rows
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().filter(|t| !t.trim().is_empty()).map(str::to_string)
}

fn parse_mark(m: &Value) -> Option<MarkSpec> {
    let area = m["bereich"].as_array().filter(|a| a.len() == 4).and_then(|a| {
        let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
        <[f64; 4]>::try_from(v).ok()
    });
    let find = non_empty(&m["suche"]);
    (find.is_some() || area.is_some()).then(|| MarkSpec {
        kind: MarkKind::parse(m["zeichen"].as_str().unwrap_or("kreis")),
        find,
        area,
        label: non_empty(&m["text"]),
    })
}

/// `fuehrung.json` lesen: Schritte und Vorschläge; unbrauchbare Schritte fallen weg.
pub fn parse_tour(json: &str) -> (Vec<Step>, Vec<String>) {
    let Ok(d) = serde_json::from_str::<Value>(json) else { return Default::default() };
    let hints = d["vorschlaege"].as_array().map(|a| a.iter().filter_map(non_empty).take(4).collect()).unwrap_or_default();
    let steps = d["schritte"].as_array().map(Vec::as_slice).unwrap_or_default();
    let steps = steps
        .iter()
        .filter_map(|s| {
            let text = non_empty(&s["text"])?;
            // ein einzelnes Zeichen darf direkt im Schritt stehen
            let mut marks: Vec<MarkSpec> = s["marken"].as_array().map(|a| a.iter().filter_map(parse_mark).collect()).unwrap_or_default();
            if marks.is_empty() {
                let single = json!({"zeichen": s["zeichen"], "suche": s["suche"], "bereich": s["bereich"]});
                marks.extend(parse_mark(&single));
            }
            let pair = |v: &Value| Some((v[0].as_u64()? as usize, v[1].as_u64()? as usize));
            let line = |l: &Value| l.as_array().map(|p| p.iter().filter_map(|q| Some([q[0].as_f64()?, q[1].as_f64()?])).collect::<Vec<_>>());
            let quiz = s["quiz"]["optionen"].as_array().filter(|o| o.len() >= 2).map(|o| Quiz {
                options: o.iter().filter_map(non_empty).collect(),
                right: s["quiz"]["richtig"].as_u64().unwrap_or(0) as usize,
                why: non_empty(&s["quiz"]["erklaerung"]),
            });
            Some(Step {
                text,
                target: s["ziel"].as_str().unwrap_or("notiz").to_string(),
                page: s["seite"].as_u64().map(|p| (p as usize).saturating_sub(1)),
                marks,
                links: s["verbinde"].as_array().map(|a| a.iter().filter_map(pair).collect()).unwrap_or_default(),
                sketch: s["skizze"].as_array().map(|a| a.iter().filter_map(line).filter(|l| l.len() >= 2).collect()).unwrap_or_default(),
                tip: non_empty(&s["tipp"]),
                quiz,
                task: non_empty(&s["aufgabe"]),
                terms: s["terme"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(TERM_COLORS.len())
                    .filter_map(|t| {
                        let spec = parse_mark(t)?;
                        Some(Term { find: spec.find, area: spec.area, name: non_empty(&t["name"])?, why: non_empty(&t["erklaerung"]) })
                    })
                    .collect(),
                plot: plot::from_json(&s["diagramm"]),
                think: parse_think(&s["denken"]),
                map: parse_map(&s["landkarte"]),
            })
        })
        .collect();
    (steps, hints)
}

/// Lernkarte ohne Stelle auf der Seite: unten mittig über dem Dock.
fn free_card_position(card: (f64, f64), view: (f64, f64), bottom: f64) -> (f64, f64) {
    fit_card(((view.0 - card.0) / 2.0, view.1 - card.1 - bottom - 60.0), card, view, bottom)
}

/// Karte vollständig ins Fenster und über das Dock schieben.
fn fit_card(at: (f64, f64), card: (f64, f64), view: (f64, f64), bottom: f64) -> (f64, f64) {
    (at.0.clamp(CARD_PAD, (view.0 - card.0 - CARD_PAD).max(CARD_PAD)), at.1.clamp(CARD_PAD, (view.1 - card.1 - bottom).max(CARD_PAD)))
}

const CARD_PAD: f64 = 12.0;
/// So viel Luft bleibt um jedes Zeichen.
const CARD_CLEARANCE: f64 = 10.0;
/// Platz des Schilds über einem beschrifteten Zeichen.
const LABEL_SPACE: f64 = 38.0;

/// Abstand zweier Rechtecke; 0, wenn sie sich berühren.
fn rect_gap(a: &Rect, b: &Rect) -> f64 {
    let dx = (b.x0 - a.x1).max(a.x0 - b.x1).max(0.0);
    let dy = (b.y0 - a.y1).max(a.y0 - b.y1).max(0.0);
    dx.hypot(dy)
}

fn overlap(a: &Rect, b: &Rect) -> f64 {
    let w = a.x1.min(b.x1) - a.x0.max(b.x0);
    let h = a.y1.min(b.y1) - a.y0.max(b.y0);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

/// Die Stelle in `area`, an der die Karte nah an den Zeichen steht und wenig Inhalt
/// verdeckt. Zeichen selbst bleiben frei, solange das irgendwie geht. Kosten in px²:
/// verdeckter Inhalt zählt voll, Entfernung leicht und mit Abstand immer stärker.
/// `near` hält die Karte bei einer Neuberechnung in der Nähe ihrer bisherigen Lage.
fn best_card_spot(area: Rect, card: (f64, f64), marks: &[Rect], links: &[Pt], ink: impl Fn(&Rect) -> f64, near: Option<(f64, f64)>) -> (f64, f64) {
    const GRID: f64 = 12.0;
    let (cw, ch) = card;
    let cost = |x: f64, y: f64| {
        let r = Rect::new(x, y, x + cw, y + ch);
        let blocked: f64 = marks.iter().map(|m| overlap(&r, &m.expand(CARD_CLEARANCE))).sum();
        let dist = marks.iter().map(|m| rect_gap(&r, m)).fold(f64::INFINITY, f64::min);
        let dist = if dist.is_finite() { dist } else { 0.0 };
        let crossed = links.iter().filter(|p| r.contains(**p)).count() as f64;
        let moved = near.map_or(0.0, |(nx, ny)| (x - nx).hypot(y - ny));
        (if blocked > 0.0 { 1e7 + blocked } else { 0.0 }) + ink(&r) + 15.0 * dist + 0.05 * dist * dist + 1500.0 * crossed + 8.0 * moved
    };
    let span = |a: f64, b: f64| ((b - a).max(0.0) / GRID).ceil() as usize;
    let (xmax, ymax) = (area.x1 - cw, area.y1 - ch);
    let axis = |lo: f64, hi: f64| (0..=span(lo, hi)).map(move |k| (lo + k as f64 * GRID).min(hi.max(lo)));
    axis(area.x0, xmax)
        .flat_map(|x| axis(area.y0, ymax).map(move |y| (x, y)))
        .map(|(x, y)| ((x, y), cost(x, y)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or((area.x0, area.y0), |s| s.0)
}

/// Berechnete Lage der Karte, als Abstand zur Stelle.
#[derive(Clone, Copy)]
struct CardSpot {
    offset: (f64, f64),
    card: (f64, f64),
    view: (f64, f64),
    zoom: f64,
}

// ------------------------------------------------------------------------ Dialog

const MAX_IDEAS: usize = 8;
const IDEAS_PER_ROW: usize = 3;
const DOCK_SPACE: i32 = 84;
const MIN_ANSWER: i32 = 90;
/// Breite des schwebenden Fensters
const CARD_WIDTH: i32 = 620;
/// Grenzen der angehefteten Spalte
const COLUMN_WIDTH: (i32, i32) = (360, 640);
/// Platz, den das Dock neben der Spalte braucht
const DOCK_WIDTH: i32 = 880;
/// Standardort: mittig, oberes Drittel. Anteile des freien Raums bzw. der Höhe.
const CARD_HOME: (f64, f64) = (0.5, 0.14);

/// Erklärstile: Name im Knopf und Anweisung, die mit jeder Frage mitgeht.
const STYLES: [(&str, &str); 4] = [
    ("Erklären", ""),
    ("Nur Tipps", "[Stil: Nur Tipps] Verrate die Lösung nicht. Führe den Nutzer mit Tipps und Gegenfragen Schritt für Schritt selbst dorthin.\n"),
    ("Abfragen", "[Stil: Abfragen] Erkläre nicht von dir aus, sondern frage den Nutzer zum Stoff ab: eine Frage oder ein Quiz nach dem anderen, mit Rückmeldung zu seiner Antwort.\n"),
    ("Klausur", "[Stil: Klausur] Richte alles auf die Prüfung aus: typische Aufgaben, Rechenschema, Fehler, die Punkte kosten. Nutze Altklausuren im Modulordner, wenn es welche gibt.\n"),
];
const STYLE_FILE: &str = "stil";
const TOURS_FILE: &str = "fuehrungen.jsonl";
const MAX_TOURS: usize = 150;
/// So viele gespeicherte Führungen stehen pro Seite in den Schnellaktionen
const TOURS_SHOWN: usize = 2;

/// Was eine Zeile der Schnellaktionen tut.
enum QuickAction {
    Tour(String),
    Ask(String),
    Progress,
    /// Varianten für den Durchgang ab dieser Seite anbieten
    PickLecture(usize),
    Lecture(usize, usize),
    BackToQuick,
}

/// Schnellaktionen unter dem leeren Eingabefeld: Symbol, Beschriftung, Frage an Claude.
const QUICK: [(&str, &str, &str); 4] = [
    ("text-x-generic-symbolic", "Diese Seite erklären", "Erkläre mir diese Seite Schritt für Schritt."),
    ("object-select-symbolic", "Meine Rechnung prüfen", "Prüfe meine Rechnung auf dieser Seite und zeig mir, wo Fehler sind."),
    ("dialog-question-symbolic", "Mich abfragen", "Frag mich zum Stoff dieser Seite ab, mit Quizfragen."),
    ("view-list-bullet-symbolic", "Seite zusammenfassen", "Fasse diese Seite kurz zusammen."),
];

pub struct ClaudePanel {
    pub clamp: adw::Clamp,
    /// angeheftete Spalte rechts neben dem Inhalt
    pub column: gtk::Box,
    pub guide: gtk::Box,
    overlay: gtk::Overlay,
    card: gtk::Box,
    /// oberer Rand des schwebenden Fensters, zum Verschieben
    handle: gtk::Box,
    /// Kopf der Spalte mit Modulname und Schließen
    pin_head: gtk::Box,
    pin_title: gtk::Label,
    pin_btn: gtk::ToggleButton,
    /// Kontextanzeige, fasst auf Klick zusammen
    context_btn: gtk::Button,
    style: gtk::DropDown,
    pen_btn: gtk::ToggleButton,
    pad_revealer: gtk::Revealer,
    pad: gtk::DrawingArea,
    /// Striche der handgeschriebenen Frage, in Pixeln des Schreibfelds
    strokes: Rc<RefCell<Vec<Vec<(f64, f64)>>>>,
    speaker: Speaker,
    /// Lernkarte liest jeden Schritt vor
    guide_speak: gtk::ToggleButton,
    /// Vorlesungsmodus: Seite des nächsten Abschnitts nach dieser Führung
    tour_next: Cell<Option<usize>>,
    /// eingekreiste Stelle, geht mit der nächsten Frage mit
    region: RefCell<Option<String>>,
    explain_btn: gtk::Button,
    /// Bezug einer Nachfrage von der Lernkarte, geht mit der nächsten Frage mit
    followup: RefCell<Option<String>>,
    /// Aktionen der Schnellaktionen und ob sie auch über einem Gespräch stehen
    quick_actions: RefCell<Vec<(QuickAction, bool)>>,
    open_source: Rc<dyn Fn(&Path, usize)>,
    /// Seite, für die die Schnellaktionen aufgebaut sind
    quick_page: Cell<Option<usize>>,
    bar: gtk::Box,
    spark: gtk::Image,
    esc_hint: gtk::Label,
    quick: gtk::ListBox,
    foot: gtk::Box,
    grip: gtk::Box,
    body: gtk::Box,
    thumbs: gtk::Box,
    entry: gtk::Entry,
    model: gtk::DropDown,
    effort: gtk::DropDown,
    history_btn: gtk::ToggleButton,
    new_btn: gtk::Button,
    attach_btn: gtk::Button,
    action_btn: gtk::Button,
    revealer: gtk::Revealer,
    scroller: gtk::ScrolledWindow,
    list: gtk::Box,
    status_row: gtk::Box,
    status: gtk::Label,
    guide_count: gtk::Label,
    guide_kind: gtk::Label,
    /// Fortschrittsbalken der Führung
    guide_steps: gtk::Box,
    guide_text: Rich,
    engine: Rc<Engine>,
    /// Antworten im Dialog, damit fertig gesetzte Formeln nachgezogen werden
    rich_rows: RefCell<Vec<Rich>>,
    guide_prev: gtk::Button,
    guide_next: gtk::Button,
    /// Quiz, Tipp, Prüfen und Skizze des aktuellen Schritts
    guide_extra: gtk::Box,
    /// Texte darin, damit fertig gesetzte Formeln nachgezogen werden
    extra_rich: RefCell<Vec<Rich>>,
    /// Vorschläge für die nächste Frage
    chips: gtk::FlowBox,
    tour_hints: RefCell<Vec<String>>,
    root: PathBuf,
    /// "notiz", "viewer1" oder "viewer2"
    view_for: Rc<dyn Fn(&str) -> Option<Rc<DocView>>>,
    toast: Rc<dyn Fn(&str)>,
    convs: RefCell<HashMap<PathBuf, Rc<Conv>>>,
    conv: RefCell<Option<Rc<Conv>>>,
    /// erste und nächste anzuzeigende Nachricht
    first: Cell<usize>,
    rendered: Cell<usize>,
    streaming: RefCell<Option<(Rich, gtk::Box, gtk::Box)>>,
    stick: Cell<bool>,
    /// Modell wurde im Dialog gewechselt und gilt statt des Standards
    model_chosen: Cell<bool>,
    effort_chosen: Cell<bool>,
    model_setting: Cell<bool>,
    attachments: RefCell<Vec<PathBuf>>,
    attach_serial: Cell<u32>,
    tour: RefCell<Vec<Step>>,
    tour_idx: Cell<usize>,
    tour_view: RefCell<Option<Rc<DocView>>>,
    guide_placed: Cell<bool>,
    /// vom Nutzer gezogener Abstand zur Standardposition, je Schritt: die Karte
    /// bleibt so an ihrer Stelle im Dokument verankert
    guide_offsets: RefCell<Vec<Option<(f64, f64)>>>,
    /// Bisherige Seite der Karte, damit sie beim Scrollen nicht hin und her wechselt
    guide_spot: Cell<Option<CardSpot>>,
    /// Inhalt der Seiten fürs Platzieren, mit dem Stand der Notiz
    guide_ink: RefCell<HashMap<usize, (u64, Rc<place::InkMap>)>>,
    /// Führung passt sich an, wenn der Nutzer nicht weiterkommt
    tour_adaptive: Cell<bool>,
    /// Text des aktuellen Denkschritts ist aufgedeckt
    guide_revealed: Cell<bool>,
    guide_stuck: gtk::Button,
    guide_note: gtk::Label,
    /// Stelle dieses Schritts auf der Seite. Fest, damit spätere Zeichen die Karte nicht versetzen.
    guide_anchor: Cell<Option<(usize, Rect)>>,
    /// laufendes Ziehen: Startpunkt im Fenster und Abstand beim Start
    guide_drag: Cell<Option<(f64, f64, f64, f64)>>,
    /// als Spalte angeheftet statt schwebend
    pinned: Cell<bool>,
    /// Ort des schwebenden Fensters, siehe CARD_HOME
    pos: Cell<(f64, f64)>,
    /// laufendes Verschieben: Startpunkt im Fenster und Ort der Karte beim Start
    move_from: Cell<Option<(f64, f64, f64, f64)>>,
    /// Graphit hat die Bildschirmtastatur eingeschaltet und schaltet sie wieder aus
    keyboard_on: Cell<bool>,
}

const A11Y_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const A11Y_KEYBOARD: &str = "screen-keyboard-enabled";

fn a11y_settings() -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(A11Y_SCHEMA, true)?;
    Some(gio::Settings::new(A11Y_SCHEMA))
}

fn keyboard_marker() -> PathBuf {
    glib::user_cache_dir().join("graphit").join("bildschirmtastatur-an")
}

/// Ist Graphit abgestürzt, während die Bildschirmtastatur an war: wieder ausschalten.
pub fn restore_keyboard() {
    if keyboard_marker().exists() {
        if let Some(s) = a11y_settings() {
            let _ = s.set_boolean(A11Y_KEYBOARD, false);
            gio::Settings::sync();
        }
        let _ = std::fs::remove_file(keyboard_marker());
    }
}

impl ClaudePanel {
    pub fn new(root: PathBuf, overlay: &gtk::Overlay, view_for: Rc<dyn Fn(&str) -> Option<Rc<DocView>>>, toast: Rc<dyn Fn(&str)>, open_source: Rc<dyn Fn(&Path, usize)>) -> Rc<ClaudePanel> {
        let round = |b: &gtk::Button, tip: &str| {
            b.set_tooltip_text(Some(tip));
            b.add_css_class("flat");
            b.add_css_class("circular");
        };
        let spark = gtk::Image::from_icon_name("fk-spark-symbolic");
        spark.add_css_class("fk-claude-spark");
        let thumbs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let entry = gtk::Entry::new();
        entry.set_hexpand(true);
        let names: Vec<&str> = MODELS.iter().map(|m| m.0).collect();
        let model = gtk::DropDown::from_strings(&names);
        model.add_css_class("flat");
        model.add_css_class("fk-claude-model");
        model.set_tooltip_text(Some(crate::i18n::tl("Modell für die nächste Frage")));
        let names: Vec<&str> = EFFORTS.iter().map(|e| crate::i18n::tl(e.0)).collect();
        let effort = gtk::DropDown::from_strings(&names);
        effort.add_css_class("flat");
        effort.add_css_class("fk-claude-model");
        effort.set_tooltip_text(Some(crate::i18n::tl("Wie gründlich Claude nachdenkt: höher ist genauer, aber langsamer")));
        let attach_btn = gtk::Button::from_icon_name("mail-attachment-symbolic");
        round(&attach_btn, crate::i18n::tl("Bild anhängen (auch Strg+V oder hineinziehen)"));
        let history_btn = gtk::ToggleButton::new();
        history_btn.set_icon_name("document-open-recent-symbolic");
        history_btn.set_tooltip_text(Some(crate::i18n::tl("Bisheriges Gespräch dieses Moduls")));
        history_btn.add_css_class("flat");
        history_btn.add_css_class("circular");
        let new_btn = gtk::Button::from_icon_name("list-add-symbolic");
        round(&new_btn, crate::i18n::tl("Neues Gespräch für dieses Modul"));
        let pin_btn = gtk::ToggleButton::new();
        pin_btn.set_icon_name("view-pin-symbolic");
        pin_btn.add_css_class("flat");
        pin_btn.add_css_class("circular");
        let action_btn = gtk::Button::from_icon_name("go-up-symbolic");
        action_btn.add_css_class("circular");
        action_btn.add_css_class("fk-claude-send");
        let pen_btn = gtk::ToggleButton::new();
        pen_btn.set_icon_name("fk-pen-symbolic");
        pen_btn.set_tooltip_text(Some(crate::i18n::tl("Frage mit dem Stift schreiben")));
        pen_btn.add_css_class("flat");
        pen_btn.add_css_class("circular");
        pen_btn.set_valign(gtk::Align::Center);
        let explain_btn = gtk::Button::with_label(crate::i18n::tl("Erklär das"));
        explain_btn.add_css_class("fk-claude-next");
        explain_btn.set_valign(gtk::Align::Center);
        explain_btn.set_visible(false);
        let esc_hint = gtk::Label::new(Some("Esc"));
        esc_hint.add_css_class("fk-claude-key");
        esc_hint.set_valign(gtk::Align::Center);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("fk-claude-bar");
        for w in [spark.upcast_ref::<gtk::Widget>(), thumbs.upcast_ref(), entry.upcast_ref(), explain_btn.upcast_ref(), pen_btn.upcast_ref(), esc_hint.upcast_ref(), action_btn.upcast_ref()] {
            bar.append(w);
        }

        let quick = gtk::ListBox::new();
        quick.add_css_class("fk-claude-quick");

        // Schreibfeld für Fragen mit dem Stift
        let pad = gtk::DrawingArea::new();
        pad.set_content_height(150);
        pad.set_hexpand(true);
        pad.add_css_class("fk-claude-pad");
        pad.set_cursor_from_name(Some("crosshair"));
        let pad_clear = gtk::Button::from_icon_name("edit-clear-symbolic");
        round(&pad_clear, crate::i18n::tl("Schreibfeld leeren"));
        pad_clear.set_halign(gtk::Align::End);
        pad_clear.set_valign(gtk::Align::Start);
        let pad_box = gtk::Overlay::new();
        pad_box.add_css_class("fk-claude-padbox");
        pad_box.set_child(Some(&pad));
        pad_box.add_overlay(&pad_clear);
        let pad_revealer = gtk::Revealer::builder().child(&pad_box).transition_type(gtk::RevealerTransitionType::SlideDown).transition_duration(160).build();

        let context_btn = gtk::Button::new();
        context_btn.add_css_class("flat");
        context_btn.add_css_class("fk-claude-context");
        context_btn.set_visible(false);
        context_btn.set_tooltip_text(Some(&crate::tr!(
            "So viel Gespräch schickt Claude bei jedem Schritt mit. Ab {}k fasst Graphit es selbst zusammen, ein Klick tut das sofort.", "This much conversation is sent with every step. From {}k, Graphit summarizes it automatically; a click does so right away.",
            COMPACT_AT / 1000
        )));
        let names: Vec<&str> = STYLES.iter().map(|s| crate::i18n::tl(s.0)).collect();
        let style = gtk::DropDown::from_strings(&names);
        style.add_css_class("flat");
        style.add_css_class("fk-claude-model");
        style.set_tooltip_text(Some(crate::i18n::tl("Wie Claude dir in diesem Modul hilft")));
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        foot.add_css_class("fk-claude-foot");
        let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fill.set_hexpand(true);
        for w in [style.upcast_ref::<gtk::Widget>(), model.upcast_ref(), effort.upcast_ref(), context_btn.upcast_ref(), fill.upcast_ref(), attach_btn.upcast_ref(), history_btn.upcast_ref(), new_btn.upcast_ref(), pin_btn.upcast_ref()] {
            foot.append(w);
        }

        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        list.add_css_class("fk-claude-list");
        let scroller = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).build();
        let status = gtk::Label::new(None);
        status.add_css_class("fk-claude-thinking");
        status.add_css_class("caption");
        status.set_ellipsize(pango::EllipsizeMode::End);
        status.set_xalign(0.0);
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_row.add_css_class("fk-claude-status");
        status_row.append(&status);
        status_row.set_visible(false);
        // Griff unter der Antwort: damit bestimmt der Nutzer, wie hoch sie werden darf
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("fk-claude-pill");
        pill.set_halign(gtk::Align::Center);
        let grip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grip.add_css_class("fk-claude-grip");
        grip.set_cursor_from_name(Some("ns-resize"));
        grip.set_tooltip_text(Some(crate::i18n::tl("Ziehen ändert die Höhe")));
        grip.append(&pill);
        let chips = gtk::FlowBox::new();
        chips.set_selection_mode(gtk::SelectionMode::None);
        chips.set_homogeneous(false);
        chips.set_max_children_per_line(4);
        chips.add_css_class("fk-claude-chips");
        chips.set_visible(false);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("fk-claude-body");
        body.append(&scroller);
        body.append(&chips);
        body.append(&status_row);
        body.append(&grip);
        let revealer = gtk::Revealer::builder().child(&body).transition_type(gtk::RevealerTransitionType::SlideDown).transition_duration(180).build();

        // oberer Rand zum Verschieben, in der Spalte stattdessen ein Kopf
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("fk-claude-pill");
        pill.set_halign(gtk::Align::Center);
        let handle = gtk::Box::new(gtk::Orientation::Vertical, 0);
        handle.add_css_class("fk-claude-handle");
        handle.set_cursor_from_name(Some("grab"));
        handle.set_tooltip_text(Some(crate::i18n::tl("Ziehen verschiebt das Fenster, Doppelklick holt es zurück in die Mitte")));
        handle.append(&pill);
        let pin_title = gtk::Label::new(None);
        pin_title.add_css_class("fk-claude-title");
        pin_title.set_ellipsize(pango::EllipsizeMode::End);
        pin_title.set_xalign(0.0);
        pin_title.set_hexpand(true);
        let pin_close = gtk::Button::from_icon_name("window-close-symbolic");
        round(&pin_close, crate::i18n::tl("Schließen (Esc)"));
        let pin_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pin_head.add_css_class("fk-claude-pinhead");
        pin_head.append(&pin_title);
        pin_head.append(&pin_close);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("fk-claude");
        for w in [handle.upcast_ref::<gtk::Widget>(), pin_head.upcast_ref(), bar.upcast_ref(), pad_revealer.upcast_ref(), revealer.upcast_ref(), quick.upcast_ref(), foot.upcast_ref()] {
            card.append(w);
        }
        let clamp = adw::Clamp::builder().maximum_size(CARD_WIDTH).tightening_threshold(CARD_WIDTH).valign(gtk::Align::Start).build();
        clamp.set_visible(false);

        // Spalte: linker Rand ändert die Breite
        let cfg = crate::settings::Settings::load();
        let edge = gtk::Box::new(gtk::Orientation::Vertical, 0);
        edge.add_css_class("fk-claude-edge");
        edge.set_cursor_from_name(Some("ew-resize"));
        let column = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        column.add_css_class("fk-claude-column");
        column.set_size_request(cfg.claude_col_w.clamp(COLUMN_WIDTH.0, COLUMN_WIDTH.1), -1);
        // feste Breite: der Inhalt daneben bekommt den ganzen Rest
        column.set_hexpand(false);
        column.append(&edge);
        column.set_visible(false);

        // Lernkarte
        let guide_kind = gtk::Label::new(None);
        guide_kind.add_css_class("fk-claude-kind");
        let guide_count = gtk::Label::new(None);
        guide_count.add_css_class("fk-claude-count");
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        round(&close, crate::i18n::tl("Erklärung beenden (Esc)"));
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let head_spark = gtk::Image::from_icon_name("fk-spark-symbolic");
        head_spark.add_css_class("fk-claude-spark");
        head.append(&head_spark);
        head.append(&guide_kind);
        let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fill.set_hexpand(true);
        head.append(&fill);
        head.append(&guide_count);
        let guide_speak = gtk::ToggleButton::new();
        guide_speak.set_icon_name("audio-volume-high-symbolic");
        guide_speak.set_tooltip_text(Some(crate::i18n::tl("Schritte vorlesen")));
        guide_speak.add_css_class("flat");
        guide_speak.add_css_class("circular");
        head.append(&guide_speak);
        head.append(&close);
        let guide_steps = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        let guide_text = Rich::new(330.0);
        let guide_prev = gtk::Button::from_icon_name("go-previous-symbolic");
        round(&guide_prev, crate::i18n::tl("Zurück"));
        let ask = gtk::Button::with_label(crate::i18n::tl("Nachfragen"));
        ask.add_css_class("flat");
        let guide_next = gtk::Button::with_label(crate::i18n::tl("Weiter"));
        guide_next.add_css_class("fk-claude-next");
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fill.set_hexpand(true);
        foot.append(&guide_prev);
        foot.append(&ask);
        let guide_stuck = gtk::Button::with_label(crate::i18n::tl("Versteh ich nicht"));
        guide_stuck.add_css_class("flat");
        guide_stuck.set_visible(false);
        guide_stuck.set_tooltip_text(Some(crate::i18n::tl("Claude baut hier einen einfacheren Zwischenschritt ein")));
        foot.append(&guide_stuck);
        foot.append(&fill);
        foot.append(&guide_next);
        let guide = gtk::Box::new(gtk::Orientation::Vertical, 8);
        guide.add_css_class("fk-claude-guide");
        guide.set_halign(gtk::Align::Start);
        guide.set_valign(gtk::Align::Start);
        guide.set_size_request(350, -1);
        guide.set_cursor_from_name(Some("grab"));
        guide.append(&head);
        guide.append(&guide_steps);
        guide.append(&guide_text.root);
        let guide_extra = gtk::Box::new(gtk::Orientation::Vertical, 6);
        guide.append(&guide_extra);
        let guide_note = gtk::Label::new(None);
        guide_note.set_xalign(0.0);
        guide_note.set_wrap(true);
        guide_note.add_css_class("fk-claude-kind-detail");
        guide_note.set_visible(false);
        guide.append(&guide_note);
        guide.append(&foot);
        guide.set_visible(false);

        let panel = Rc::new(ClaudePanel {
            clamp,
            column,
            guide,
            overlay: overlay.clone(),
            card,
            handle,
            pin_head,
            pin_title,
            pin_btn,
            context_btn,
            style,
            pen_btn,
            pad_revealer,
            pad,
            strokes: Rc::default(),
            speaker: Speaker::default(),
            guide_speak,
            tour_next: Cell::new(None),
            region: RefCell::new(None),
            explain_btn,
            followup: RefCell::new(None),
            quick_actions: RefCell::new(Vec::new()),
            open_source,
            quick_page: Cell::new(None),
            bar,
            spark,
            esc_hint,
            quick,
            foot,
            grip: grip.clone(),
            body,
            thumbs,
            entry,
            model,
            effort,
            history_btn,
            new_btn,
            attach_btn,
            action_btn,
            revealer,
            scroller,
            list,
            status_row,
            status,
            guide_count,
            guide_kind,
            guide_steps,
            guide_text,
            engine: Rc::new(Engine::default()),
            rich_rows: RefCell::new(Vec::new()),
            guide_prev,
            guide_next,
            guide_extra,
            extra_rich: RefCell::new(Vec::new()),
            chips,
            tour_hints: RefCell::new(Vec::new()),
            root,
            view_for,
            toast,
            convs: RefCell::new(HashMap::new()),
            conv: RefCell::new(None),
            first: Cell::new(0),
            rendered: Cell::new(0),
            streaming: RefCell::new(None),
            stick: Cell::new(true),
            model_chosen: Cell::new(false),
            effort_chosen: Cell::new(false),
            model_setting: Cell::new(false),
            attachments: RefCell::new(Vec::new()),
            attach_serial: Cell::new(0),
            tour: RefCell::new(Vec::new()),
            tour_idx: Cell::new(0),
            tour_view: RefCell::new(None),
            guide_placed: Cell::new(false),
            guide_offsets: RefCell::new(Vec::new()),
            guide_spot: Cell::new(None),
            guide_ink: RefCell::default(),
            tour_adaptive: Cell::new(false),
            guide_revealed: Cell::new(false),
            guide_stuck,
            guide_note,
            guide_anchor: Cell::new(None),
            guide_drag: Cell::new(None),
            pinned: Cell::new(cfg.claude_pinned),
            pos: Cell::new(if cfg.claude_x >= 0.0 { (cfg.claude_x, cfg.claude_y) } else { CARD_HOME }),
            move_from: Cell::new(None),
            keyboard_on: Cell::new(false),
        });
        panel.connect();
        panel.connect_grip(&grip);
        panel.connect_card_drag();
        panel.connect_edge(&edge);
        panel.connect_pad(&pad_clear);
        let (handle, spark, foot) = (panel.handle.clone(), panel.spark.clone(), panel.foot.clone());
        panel.connect_move(&handle);
        panel.connect_move(&spark);
        panel.connect_move(&foot);
        panel.mount();
        let w = Rc::downgrade(&panel);
        pin_close.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.close();
            }
        });
        let w = Rc::downgrade(&panel);
        panel.engine.connect_ready(Rc::new(move || {
            if let Some(p) = w.upgrade() {
                let extra = p.extra_rich.borrow();
                for r in p.rich_rows.borrow().iter().chain(extra.iter()).chain(std::iter::once(&p.guide_text)) {
                    r.set(&r.source(), &p.engine);
                }
            }
        }));
        let w = Rc::downgrade(&panel);
        close.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.end_tour();
            }
        });
        let w = Rc::downgrade(&panel);
        ask.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                let step = p.tour.borrow().get(p.tour_idx.get()).map(|s| (s.text.clone(), p.tour_idx.get() + 1));
                let total = p.tour.borrow().len();
                p.open();
                if let Some((text, i)) = step {
                    p.followup.replace(Some(format!("[Nachfrage zu Schritt {i} von {total} der laufenden Führung. Text der Karte: {text}]\n")));
                    p.entry.set_placeholder_text(Some(&crate::tr!("Nachfrage zu Schritt {i} …", "Follow up on step {i} …")));
                }
            }
        });
        panel
    }

    fn note(&self) -> Option<Rc<DocView>> {
        (self.view_for)("notiz")
    }

    fn connect(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        self.entry.connect_activate(move |e| {
            if let Some(p) = w.upgrade() {
                // mit den Pfeiltasten gewählte Schnellaktion
                match p.quick.selected_row().filter(|_| e.text().is_empty() && p.quick.is_visible()) {
                    Some(row) => p.run_quick(row.index()),
                    None => p.send(),
                }
            }
        });
        let w = Rc::downgrade(self);
        self.entry.connect_changed(move |_| {
            if let Some(p) = w.upgrade() {
                p.update_bar();
            }
        });
        let w = Rc::downgrade(self);
        self.quick.connect_row_activated(move |_, row| {
            if let Some(p) = w.upgrade() {
                p.run_quick(row.index());
            }
        });
        self.connect_quick_pen();
        let w = Rc::downgrade(self);
        self.style.connect_selected_notify(move |d| {
            if let Some(p) = w.upgrade().filter(|p| !p.model_setting.get()) {
                let conv = p.conv.borrow().clone();
                if let Some(c) = conv {
                    c.style.set((d.selected() as usize).min(STYLES.len() - 1));
                    let data = data_dir(&c.dir);
                    let _ = std::fs::create_dir_all(&data);
                    let _ = std::fs::write(data.join(STYLE_FILE), c.style.get().to_string());
                }
            }
        });
        let w = Rc::downgrade(self);
        self.guide_speak.connect_toggled(move |b| {
            let Some(p) = w.upgrade() else { return };
            if b.is_active() {
                p.read_step();
            } else {
                p.speaker.stop();
            }
        });
        let w = Rc::downgrade(self);
        self.explain_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.ask(crate::i18n::tl("Erkläre mir diesen Ausschnitt."));
            }
        });
        let w = Rc::downgrade(self);
        self.pen_btn.connect_toggled(move |b| {
            if let Some(p) = w.upgrade() {
                p.pad_revealer.set_reveal_child(b.is_active());
                if b.is_active() {
                    // die Bildschirmtastatur würde das Schreibfeld verdecken
                    p.hide_keyboard();
                }
            }
        });
        // Beim Blättern die gespeicherten Führungen der neuen Seite anbieten
        let w = Rc::downgrade(self);
        glib::timeout_add_seconds_local(1, move || {
            let Some(p) = w.upgrade() else { return glib::ControlFlow::Break };
            if p.is_open() && p.quick_page.get() != p.note().map(|v| v.canvas.st().current_page()) {
                p.rebuild_quick();
                p.update_bar();
            }
            glib::ControlFlow::Continue
        });
        let w = Rc::downgrade(self);
        self.context_btn.connect_clicked(move |_| {
            let Some(p) = w.upgrade() else { return };
            let conv = p.conv.borrow().clone();
            if let Some(c) = conv {
                glib::spawn_future_local(async move { p.compact(c).await });
            }
        });
        let w = Rc::downgrade(self);
        self.pin_btn.connect_toggled(move |b| {
            if let Some(p) = w.upgrade().filter(|p| p.pinned.get() != b.is_active()) {
                p.set_pinned(b.is_active());
            }
        });
        let w = Rc::downgrade(self);
        self.action_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                let running = p.conv.borrow().clone().filter(|c| c.pid.get().is_some());
                match running {
                    Some(c) => c.stop(),
                    None => p.send(),
                }
            }
        });
        let w = Rc::downgrade(self);
        self.history_btn.connect_toggled(move |b| {
            if let Some(p) = w.upgrade() {
                p.show_history(b.is_active());
            }
        });
        let w = Rc::downgrade(self);
        self.new_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.new_conversation();
            }
        });
        let w = Rc::downgrade(self);
        self.attach_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.choose_image();
            }
        });
        let w = Rc::downgrade(self);
        self.model.connect_selected_notify(move |_| {
            if let Some(p) = w.upgrade().filter(|p| !p.model_setting.get()) {
                p.model_chosen.set(true);
            }
        });
        let w = Rc::downgrade(self);
        self.effort.connect_selected_notify(move |_| {
            if let Some(p) = w.upgrade().filter(|p| !p.model_setting.get()) {
                p.effort_chosen.set(true);
            }
        });
        let w = Rc::downgrade(self);
        self.guide_next.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.step(1);
            }
        });
        let w = Rc::downgrade(self);
        self.guide_stuck.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.simplify_step("Der Nutzer sagt, dass er es nicht versteht");
            }
        });
        let w = Rc::downgrade(self);
        self.guide_prev.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.step(-1);
            }
        });
        let w = Rc::downgrade(self);
        self.guide.add_tick_callback(move |_, _| match w.upgrade() {
            Some(p) => {
                p.place_guide();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        // Strg+V: Bild aus der Zwischenablage anhängen, Text normal einfügen
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let is_paste = mods.contains(gdk::ModifierType::CONTROL_MASK) && key.to_lower() == gdk::Key::v;
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => 0,
            };
            match w.upgrade() {
                Some(p) if is_paste && p.paste_image() => glib::Propagation::Stop,
                Some(p) if step != 0 && p.quick.is_visible() => {
                    p.move_quick(step);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.entry.add_controller(keys);
        // Bilder hineinziehen
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let w = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(p), Ok(files)) = (w.upgrade(), value.get::<gdk::FileList>()) else { return false };
            for f in files.files().iter().filter_map(|f| f.path()) {
                p.attach_file(&f);
            }
            true
        });
        self.card.add_controller(drop);
        // Der Dialog belegt nur seine eigene Fläche: daneben bleibt Graphit voll bedienbar
        let w = Rc::downgrade(self);
        self.overlay.connect_get_child_position(move |_, child| {
            let p = w.upgrade()?;
            if child != p.clamp.upcast_ref::<gtk::Widget>() {
                return None;
            }
            let (x, y, width, height) = p.card_rect();
            Some(gdk::Rectangle::new(x, y, width, height))
        });
        // unten bleiben, solange der Nutzer nicht hochgescrollt hat
        let adj = self.scroller.vadjustment();
        let w = Rc::downgrade(self);
        adj.connect_value_changed(move |a| {
            if let Some(p) = w.upgrade() {
                p.stick.set(a.value() + a.page_size() >= a.upper() - 24.0);
            }
        });
        let w = Rc::downgrade(self);
        adj.connect_upper_notify(move |a| {
            if w.upgrade().is_some_and(|p| p.stick.get()) {
                a.set_value(a.upper() - a.page_size());
            }
        });
    }

    pub fn is_open(&self) -> bool {
        self.clamp.is_visible() || self.column.is_visible()
    }

    /// Ort und Größe des schwebenden Fensters im aktuellen Graphit-Fenster.
    fn card_rect(&self) -> (i32, i32, i32, i32) {
        let (ow, oh) = (self.overlay.width(), self.overlay.height());
        let width = ow.min(CARD_WIDTH + 32) - 32;
        let (_, natural, _, _) = self.clamp.measure(gtk::Orientation::Vertical, width);
        let height = natural.min(oh - 16);
        let (fx, fy) = self.pos.get();
        let x = (fx * (ow - width) as f64).round() as i32;
        // wächst die Antwort über den Rand, weicht das Fenster nach oben aus
        let y = ((fy * oh as f64).round() as i32).min(oh - height - 8).max(8);
        (x, y, width, height)
    }

    /// Eingabe, Absenden und Schnellaktionen passend zum Zustand zeigen.
    fn update_bar(&self) {
        let running = self.conv.borrow().as_ref().is_some_and(|c| c.pid.get().is_some());
        let typed = !self.entry.text().is_empty() || !self.attachments.borrow().is_empty() || !self.strokes.borrow().is_empty();
        let pinned = self.pinned.get();
        let used = self.conv.borrow().as_ref().map_or(0, |c| c.context.get());
        self.context_btn.set_visible(used > 0);
        self.context_btn.set_label(&crate::tr!("Kontext {}k", "Context {}k", (used + 500) / 1000));
        if used >= COMPACT_IDLE_AT {
            self.context_btn.add_css_class("fk-claude-context-high");
        } else {
            self.context_btn.remove_css_class("fk-claude-context-high");
        }
        self.action_btn.set_visible(running || typed);
        self.explain_btn.set_visible(!running && self.region.borrow().is_some());
        self.esc_hint.set_visible(!running && !typed && !pinned);
        // die Spalte zeigt ihr Gespräch und bietet darüber nur gespeicherte Führungen an
        let always: Vec<bool> = self.quick_actions.borrow().iter().map(|a| a.1).collect();
        let empty = self.list.first_child().is_none();
        let free = if pinned { empty || always.contains(&true) } else { !self.revealer.reveals_child() };
        let show = free && !typed && !running && self.note().is_some();
        let mut i = 0;
        while let Some(row) = self.quick.row_at_index(i) {
            row.set_visible(always.get(i as usize).copied().unwrap_or(false) || !pinned || empty);
            i += 1;
        }
        if !show {
            self.quick.unselect_all();
        }
        self.quick.set_visible(show);
        if running {
            self.spark.add_css_class("fk-claude-busy");
        } else {
            self.spark.remove_css_class("fk-claude-busy");
        }
    }

    fn run_quick(self: &Rc<Self>, row: i32) {
        enum Run {
            Tour(String),
            Ask(String),
            Progress,
            Pick(usize),
            Lecture(usize, usize),
            Back,
        }
        let run = match self.quick_actions.borrow().get(row as usize) {
            Some((QuickAction::Tour(r), _)) => Run::Tour(r.clone()),
            Some((QuickAction::Ask(q), _)) => Run::Ask(q.clone()),
            Some((QuickAction::Progress, _)) => Run::Progress,
            Some((QuickAction::PickLecture(p), _)) => Run::Pick(*p),
            Some((QuickAction::Lecture(p, k), _)) => Run::Lecture(*p, *k),
            Some((QuickAction::BackToQuick, _)) => Run::Back,
            None => return,
        };
        match run {
            Run::Tour(raw) => {
                let (steps, hints) = parse_tour(&raw);
                self.step_aside();
                self.tour_next.set(None);
                self.tour_adaptive.set(false);
                self.start_tour(steps, hints);
            }
            Run::Ask(q) => self.ask(&q),
            Run::Progress => self.show_progress(),
            Run::Pick(start) => self.show_lecture_kinds(start),
            Run::Lecture(start, kind) => self.start_lecture(start, kind),
            Run::Back => {
                self.rebuild_quick();
                self.update_bar();
            }
        }
    }

    /// Fragt vor dem Durchgang, wie tief er gehen soll.
    fn show_lecture_kinds(&self, start: usize) {
        self.quick.remove_all();
        let add = |icon: &str, title: &str, detail: Option<&str>| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Image::from_icon_name(icon));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
            let name = gtk::Label::new(Some(title));
            name.set_xalign(0.0);
            text.append(&name);
            if let Some(d) = detail {
                let more = gtk::Label::new(Some(d));
                more.set_xalign(0.0);
                more.add_css_class("fk-claude-kind-detail");
                text.append(&more);
            }
            row.append(&text);
            self.quick.append(&row);
        };
        let mut actions = Vec::new();
        for (k, (name, detail, _)) in LECTURE_KINDS.iter().enumerate() {
            add(["weather-clear-symbolic", "view-more-symbolic", "emblem-ok-symbolic", "media-seek-forward-symbolic"][k], crate::i18n::tl(name), Some(crate::i18n::tl(detail)));
            actions.push((QuickAction::Lecture(start, k), true));
        }
        add("go-previous-symbolic", crate::i18n::tl("Zurück"), None);
        actions.push((QuickAction::BackToQuick, true));
        self.quick_actions.replace(actions);
        self.quick.unselect_all();
    }

    /// Startet den Durchgang. Sichtbar bleibt eine kurze Zeile, die Anweisung geht verdeckt mit.
    fn start_lecture(self: &Rc<Self>, start: usize, kind: usize) {
        let Some(conv) = self.conv.borrow().clone().or_else(|| {
            self.open();
            self.conv.borrow().clone()
        }) else {
            return;
        };
        let kind = kind.min(LECTURE_KINDS.len() - 1);
        let _ = std::fs::write(data_dir(&conv.dir).join(LECTURE_KIND_FILE), kind.to_string());
        let (name, _, how) = LECTURE_KINDS[kind];
        let language = if crate::i18n::english() { "Englisch" } else { "Deutsch" };
        let guide = cache_dir(&conv.dir).join(LECTURE_GUIDE_FILE);
        self.followup.replace(Some(format!(
            "[Vorlesungsmodus. {how} Lies zuerst {} und halte dich genau daran. Nimm ab Seite {start} den nächsten zusammengehörenden Abschnitt von 3 bis 5 Seiten. Schreibe alle Texte auf {language}, unabhängig vom bisherigen Gespräch.]\n",
            guide.display()
        )));
        conv.lecture.set(true);
        let name = crate::i18n::tl(name);
        self.ask(&crate::tr!("Vorlesung ab Seite {start}: {name}", "Lecture from page {start}: {name}"));
    }

    /// Variante des letzten Durchgangs in diesem Modul.
    fn last_lecture_kind(&self) -> usize {
        self.conv.borrow().as_ref().and_then(|c| std::fs::read_to_string(data_dir(&c.dir).join(LECTURE_KIND_FILE)).ok()).and_then(|t| t.trim().parse().ok()).unwrap_or(0)
    }

    /// Schnellaktionen aufbauen: Gespeichertes und Persönliches zuerst, dann die festen.
    fn rebuild_quick(&self) {
        let view = self.note();
        let page = view.as_ref().map(|v| v.canvas.st().current_page());
        let note = view.as_ref().and_then(|v| v.canvas.st().doc_key());
        let conv = self.conv.borrow().clone();
        let tours = match (&conv, &note, page) {
            (Some(c), Some(n), Some(p)) => saved_tours(&c.dir, n, p),
            _ => Vec::new(),
        };
        let topics = conv.as_ref().map(|c| index::load_progress(&data_dir(&c.dir))).unwrap_or_default();
        let shaky: Vec<String> = topics.iter().filter(|t| !t.sure).map(|t| t.name.clone()).collect();
        self.quick_page.set(page);
        self.quick.remove_all();
        let add = |icon: &str, text: &str| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Image::from_icon_name(icon));
            let label = gtk::Label::new(Some(text));
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_xalign(0.0);
            row.append(&label);
            self.quick.append(&row);
        };
        let mut actions = Vec::new();
        for (title, raw) in tours {
            add("media-playback-start-symbolic", &crate::tr!("Führung nochmal ansehen: {title}", "Replay walkthrough: {title}"));
            actions.push((QuickAction::Tour(raw), true));
        }
        if let (Some(c), Some(n), Some(p)) = (&conv, &note, page) {
            match index::lecture_next(&data_dir(&c.dir), n) {
                Some(next) => {
                    add("media-seek-forward-symbolic", &crate::tr!("Vorlesung fortsetzen ab Seite {next}", "Continue lecture from page {next}"));
                    actions.push((QuickAction::PickLecture(next), true));
                }
                None => {
                    add("media-seek-forward-symbolic", crate::i18n::tl("Vorlesung ab hier durchgehen"));
                    actions.push((QuickAction::PickLecture(p + 1), false));
                }
            }
        }
        if !shaky.is_empty() {
            add("view-refresh-symbolic", &crate::tr!("Wiederholung starten ({} wackelig)", "Start review ({} shaky)", shaky.len()));
            let list = shaky.iter().take(index::TOPICS_IN_PROMPT).cloned().collect::<Vec<_>>().join(", ");
            actions.push((QuickAction::Ask(crate::tr!("Starte eine Wiederholung zu diesen wackeligen Themen: {list}. Frag mich ab, mit Quizfragen und, wenn vorhanden, Aufgaben aus Altklausuren.", "Start a review of these shaky topics: {list}. Quiz me, using past exam questions where available.")), true));
        }
        if !topics.is_empty() {
            add("emblem-ok-symbolic", crate::i18n::tl("Lernstand ansehen"));
            actions.push((QuickAction::Progress, true));
        }
        if conv.as_ref().is_some_and(|c| c.has_exams.get()) {
            add("edit-find-symbolic", crate::i18n::tl("Passende Altklausur-Aufgaben finden"));
            actions.push((QuickAction::Ask(crate::i18n::tl("Suche in den Altklausuren Aufgaben zum Stoff dieser Seite und nenne sie mit Quelle.").into()), false));
        }
        for (icon, label, question) in QUICK {
            let (label, question) = (crate::i18n::tl(label), crate::i18n::tl(question));
            add(icon, label);
            actions.push((QuickAction::Ask(question.to_string()), false));
        }
        self.quick_actions.replace(actions);
    }

    /// Übersicht des Lernstands: Einstufung umschalten, Einträge löschen.
    fn show_progress(self: &Rc<Self>) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        let data = data_dir(&conv.dir);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list.add_css_class("fk-claude-progress");
        for t in index::load_progress(&data) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("fk-claude-progress-row");
            let name = gtk::Label::new(Some(&t.name));
            name.set_hexpand(true);
            name.set_xalign(0.0);
            name.set_wrap(true);
            let state = gtk::Button::with_label(if t.sure { "sicher" } else { "wackelig" });
            state.add_css_class("flat");
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some(crate::i18n::tl("Thema entfernen")));
            let (d, topic) = (data.clone(), t.name.clone());
            state.connect_clicked(move |b| {
                let mut all = index::load_progress(&d);
                if let Some(x) = all.iter_mut().find(|x| x.name == topic) {
                    x.sure = !x.sure;
                    b.set_label(if x.sure { "sicher" } else { "wackelig" });
                }
                index::save_progress(&d, &all);
            });
            let (d, topic, r) = (data.clone(), t.name.clone(), row.clone());
            remove.connect_clicked(move |_| {
                let mut all = index::load_progress(&d);
                all.retain(|x| x.name != topic);
                index::save_progress(&d, &all);
                r.set_visible(false);
            });
            row.append(&name);
            row.append(&state);
            row.append(&remove);
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(360).min_content_width(340).build();
        let pop = gtk::Popover::new();
        pop.set_child(Some(&scroller));
        pop.set_parent(&self.foot);
        pop.set_position(gtk::PositionType::Top);
        let w = Rc::downgrade(self);
        pop.connect_closed(move |p| {
            p.unparent();
            if let Some(panel) = w.upgrade() {
                panel.quick_page.set(None);
            }
        });
        pop.popup();
    }

    /// Modulwissen im Hintergrund aufbauen; beim ersten Öffnen des Moduls in dieser Sitzung.
    fn index_module(self: &Rc<Self>, conv: &Rc<Conv>) {
        if conv.indexed.replace(true) {
            return;
        }
        let name = short_path(&conv.dir.to_string_lossy());
        let exams = index::exam_folder(&self.root, &name, &crate::settings::Settings::load().claude_exams);
        conv.has_exams.set(exams.is_some());
        let (module, cache) = (conv.dir.clone(), cache_dir(&conv.dir));
        let (p, c) = (self.clone(), conv.clone());
        glib::spawn_future_local(async move {
            if let Ok(found) = gio::spawn_blocking(move || index::refresh(&module, exams.as_deref(), &cache)).await {
                c.sources.replace(found);
                p.quick_page.set(None);
            }
        });
    }

    /// Knöpfe zu den Quellen einer Antwort oder Lernkarte.
    fn source_buttons(self: &Rc<Self>, conv: &Rc<Conv>, found: &[(String, usize)], into: &gtk::Box) {
        for (id, page) in found {
            let Some(src) = conv.sources.borrow().get(id).cloned() else { continue };
            let label: String = src.name.chars().take(28).collect();
            let b = gtk::Button::with_label(&crate::tr!("{label} · S. {page}", "{label} · p. {page}"));
            b.add_css_class("fk-claude-hint");
            b.add_css_class("fk-claude-source");
            b.set_tooltip_text(Some(&src.path.to_string_lossy()));
            let (w, page) = (Rc::downgrade(self), page.saturating_sub(1));
            b.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    (p.open_source)(&src.path, page);
                }
            });
            into.append(&b);
        }
    }

    /// Schreibfeld: Stift und Maus schreiben, Finger nicht (die Hand liegt oft auf).
    fn connect_pad(self: &Rc<Self>, clear: &gtk::Button) {
        let strokes = self.strokes.clone();
        self.pad.set_draw_func(move |_, cr, _, _| {
            cr.set_source_rgb(0.945, 0.941, 0.929);
            draw_strokes(cr, &strokes.borrow(), 2.2);
        });
        // Direkt statt über eine Geste: Die Geste merkt sich das erste Gerät und ignoriert den
        // Stift, solange die Hand auf dem Feld liegt.
        let input = gtk::EventControllerLegacy::new();
        let w = Rc::downgrade(self);
        let writing = Cell::new(false);
        input.connect_event(move |_, ev| {
            use gdk::EventType as E;
            let Some(p) = w.upgrade() else { return glib::Propagation::Proceed };
            let et = ev.event_type();
            if matches!(et, E::TouchBegin | E::TouchUpdate | E::TouchEnd | E::TouchCancel) {
                return glib::Propagation::Stop;
            }
            if !matches!(et, E::ButtonPress | E::MotionNotify | E::ButtonRelease) {
                return glib::Propagation::Proceed;
            }
            if ev.is_pointer_emulated() || ev.device().is_some_and(|d| d.source() == gdk::InputSource::Touchscreen) {
                return glib::Propagation::Stop;
            }
            match et {
                E::ButtonPress if ev.downcast_ref::<gdk::ButtonEvent>().is_some_and(|b| b.button() == gdk::BUTTON_PRIMARY) => {
                    let Some(at) = widget_point(&p.pad, ev) else { return glib::Propagation::Stop };
                    writing.set(true);
                    p.strokes.borrow_mut().push(vec![at]);
                    p.pad.queue_draw();
                    p.update_bar();
                }
                E::MotionNotify if writing.get() => {
                    // das Loslassen kann verloren gehen, etwa wenn ein Dialog aufgeht
                    if !ev.modifier_state().contains(gdk::ModifierType::BUTTON1_MASK) {
                        writing.set(false);
                    } else if let Some(at) = widget_point(&p.pad, ev) {
                        if let Some(s) = p.strokes.borrow_mut().last_mut() {
                            s.push(at);
                        }
                        p.pad.queue_draw();
                    }
                }
                E::ButtonRelease => writing.set(false),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.pad.add_controller(input);
        let w = Rc::downgrade(self);
        clear.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.clear_pad();
            }
        });
    }

    /// Stifttipps wertet die Liste selbst aus: Die Klick-Geste bricht bei leicht verrutschter
    /// Spitze ab oder zählt schnelle Tipps als Doppeltipp, dann wird die Zeile nur markiert.
    fn connect_quick_pen(self: &Rc<Self>) {
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        let every_pointer = std::env::var("GRAPHIT_TAP_ALL").is_ok();
        let w = Rc::downgrade(self);
        let pressed = Cell::new(None::<i32>);
        input.connect_event(move |_, ev| {
            use gdk::EventType as E;
            let et = ev.event_type();
            let stylus = ev.device_tool().is_some() || ev.device().is_some_and(|d| d.source() == gdk::InputSource::Pen);
            let primary = ev.downcast_ref::<gdk::ButtonEvent>().is_some_and(|b| b.button() == gdk::BUTTON_PRIMARY);
            let Some(p) = w.upgrade().filter(|_| matches!(et, E::ButtonPress | E::ButtonRelease) && primary && (stylus || every_pointer)) else {
                return glib::Propagation::Proceed;
            };
            let row = widget_point(&p.quick, ev).and_then(|(_, y)| p.quick.row_at_y(y as i32));
            if et == E::ButtonPress {
                pressed.set(row.map(|r| r.index()));
            } else if let Some(row) = row.filter(|r| pressed.take() == Some(r.index())) {
                // erst nach dem Ereignis: die Aktion baut die Liste um
                glib::idle_add_local_once(move || {
                    row.activate();
                });
            }
            glib::Propagation::Stop
        });
        self.quick.add_controller(input);
    }

    /// Testhilfe: öffnet das Schreibfeld, nennt seine Lage und später die Striche darin.
    pub fn test_pen(self: &Rc<Self>) {
        self.pen_btn.set_active(true);
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            let Some(p) = w.upgrade() else { return };
            let Some(root) = p.pad.root() else { return };
            if let Some(at) = p.pad.compute_point(&root, &gtk::graphene::Point::new(0.0, 0.0)) {
                let (sx, sy) = p.pad.native().map_or((0.0, 0.0), |n| n.surface_transform());
                eprintln!("PEN Feld {} {} {} {}", (at.x() as f64 + sx) as i32, (at.y() as f64 + sy) as i32, p.pad.width(), p.pad.height());
                let last = p.quick.last_child().and_then(|r| r.compute_point(&root, &gtk::graphene::Point::new(40.0, 12.0)));
                eprintln!("PEN Zeile {:?} sichtbar {}", last.map(|a| ((a.x() as f64 + sx) as i32, (a.y() as f64 + sy) as i32)), p.quick.is_visible());
            }
        });
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(9), move || {
            if let Some(p) = w.upgrade() {
                let strokes = p.strokes.borrow();
                let pts: Vec<_> = strokes.iter().flatten().collect();
                let inside = pts.iter().filter(|a| a.0 >= 0.0 && a.1 >= 0.0 && a.0 <= p.pad.width() as f64 && a.1 <= p.pad.height() as f64).count();
                eprintln!("PEN Striche {} Punkte {} davon im Feld {} {:?}", strokes.len(), pts.len(), inside, strokes.iter().map(|s| (s.first().copied(), s.last().copied())).collect::<Vec<_>>());
            }
        });
    }

    /// Testhilfe: öffnet eine Quelle wie ihr Knopf.
    pub fn test_source(&self, path: &Path, page: usize) {
        (self.open_source)(path, page.saturating_sub(1));
    }

    /// Testhilfe: schreibt "2+2" ins Schreibfeld.
    pub fn test_lecture(self: &Rc<Self>, start: usize, kind: usize) {
        self.open();
        self.start_lecture(start, kind);
    }

    /// Testhilfe: öffnet die Variantenauswahl.
    pub fn test_lecture_kinds(self: &Rc<Self>) {
        self.open();
        self.show_lecture_kinds(1);
        self.update_bar();
    }

    /// Testhilfe: läuft wie ein Vorlesungsdurchgang, mit Anpassung.
    pub fn test_adaptive(self: &Rc<Self>) {
        self.tour_adaptive.set(true);
        self.guide_stuck.set_visible(true);
    }

    pub fn test_pad(&self) {
        self.pen_btn.set_active(true);
        let glyph = |x: f64| vec![(x, 40.0), (x + 18.0, 28.0), (x + 34.0, 40.0), (x + 30.0, 60.0), (x, 100.0), (x + 38.0, 100.0)];
        let plus = vec![vec![(100.0, 65.0), (140.0, 65.0)], vec![(120.0, 45.0), (120.0, 85.0)]];
        let mut all = vec![glyph(40.0), glyph(170.0)];
        all.extend(plus);
        self.strokes.replace(all);
        self.pad.queue_draw();
        self.update_bar();
    }

    fn clear_pad(&self) {
        self.strokes.borrow_mut().clear();
        self.pad.queue_draw();
        self.update_bar();
    }

    /// Handschrift als Bild, schwarz auf Weiß und auf ihren Inhalt zugeschnitten.
    fn pad_png(&self) -> Option<Vec<u8>> {
        let strokes = self.strokes.borrow();
        let pts = || strokes.iter().flatten();
        let (x0, y0) = pts().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (x1, y1) = pts().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        if x0 > x1 {
            return None;
        }
        const SCALE: f64 = 2.0;
        const MARGIN: f64 = 12.0;
        let (w, h) = ((x1 - x0 + 2.0 * MARGIN) * SCALE, (y1 - y0 + 2.0 * MARGIN) * SCALE);
        let s = cairo::ImageSurface::create(cairo::Format::Rgb24, w.ceil() as i32, h.ceil() as i32).ok()?;
        {
            let cr = cairo::Context::new(&s).ok()?;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().ok()?;
            cr.scale(SCALE, SCALE);
            cr.translate(MARGIN - x0, MARGIN - y0);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            draw_strokes(&cr, &strokes, 2.2);
        }
        let mut png = Vec::new();
        s.write_to_png(&mut png).ok()?;
        Some(png)
    }

    /// Pfeiltasten im Eingabefeld: durch die Schnellaktionen gehen, oben zurück ins Feld.
    fn move_quick(&self, step: i32) {
        let now = self.quick.selected_row().map_or(-1, |r| r.index());
        let next = now + step;
        if next < 0 {
            self.quick.unselect_all();
        } else if let Some(row) = self.quick.row_at_index(next) {
            self.quick.select_row(Some(&row));
        }
    }

    /// Einstellung ändern und dauerhaft ablegen.
    fn store(&self, change: impl FnOnce(&mut crate::settings::Settings)) {
        match self.note() {
            Some(v) => {
                let mut cfg = v.canvas.st().cfg.clone();
                change(&mut cfg);
                v.canvas.apply_settings(cfg);
            }
            None => {
                let mut cfg = crate::settings::Settings::load();
                change(&mut cfg);
                cfg.save();
            }
        }
    }

    /// Karte in die Spalte oder ins schwebende Fenster hängen und die Teile umordnen.
    fn mount(&self) {
        let on = self.pinned.get();
        if on {
            self.clamp.set_child(None::<&gtk::Widget>);
            if self.card.parent().is_none() {
                self.column.append(&self.card);
            }
            self.card.reorder_child_after(&self.bar, Some(&self.quick));
            self.card.add_css_class("fk-claude-pinned");
            // In der schmalen Spalte ist die Fußleiste voll: Stil in den Kopf
            self.foot.remove(&self.style);
            self.pin_head.insert_child_after(&self.style, Some(&self.pin_title));
            self.scroller.set_max_content_height(-1);
        } else {
            if self.card.parent().is_some() {
                self.column.remove(&self.card);
            }
            self.clamp.set_child(Some(&self.card));
            self.card.reorder_child_after(&self.bar, Some(&self.pin_head));
            self.card.remove_css_class("fk-claude-pinned");
            if self.style.parent().is_some_and(|w| w == *self.pin_head.upcast_ref::<gtk::Widget>()) {
                self.pin_head.remove(&self.style);
                self.foot.prepend(&self.style);
            }
        }
        self.card.reorder_child_after(&self.pad_revealer, Some(&self.bar));
        self.card.set_hexpand(on);
        self.scroller.set_propagate_natural_height(!on);
        for w in [self.scroller.upcast_ref::<gtk::Widget>(), self.body.upcast_ref(), self.revealer.upcast_ref()] {
            w.set_vexpand(on);
        }
        self.handle.set_visible(!on);
        self.grip.set_visible(!on);
        self.pin_head.set_visible(on);
        self.history_btn.set_visible(!on);
        self.pin_btn.set_active(on);
        self.pin_btn.set_tooltip_text(Some(if on { crate::i18n::tl("Wieder als schwebendes Fenster") } else { crate::i18n::tl("Als Spalte rechts anheften") }));
    }

    /// Umschalten, während das Fenster offen ist: das Gespräch bleibt stehen.
    fn set_pinned(self: &Rc<Self>, on: bool) {
        self.history_btn.set_active(false);
        self.pinned.set(on);
        self.clamp.set_visible(false);
        self.column.set_visible(false);
        self.mount();
        self.store(|c| c.claude_pinned = on);
        self.reveal();
        // beim Lösen bleibt die letzte Antwort stehen
        let conv = self.conv.borrow().clone();
        if let Some(c) = conv.as_ref().filter(|c| !on && !c.msgs.borrow().is_empty()) {
            c.unread.set(true);
        }
        self.show_conversation();
        if let Some(c) = conv {
            c.unread.set(false);
        }
        self.entry.grab_focus();
    }

    pub fn toggle_pin(&self) {
        self.pin_btn.set_active(!self.pinned.get());
    }

    /// Das zur Anheftung passende Fenster zeigen.
    fn reveal(&self) {
        if self.pinned.get() {
            self.column.set_visible(true);
        } else {
            self.card.remove_css_class("fk-claude-in");
            self.clamp.set_visible(true);
            self.card.add_css_class("fk-claude-in");
        }
    }

    /// Spalte: ganzes Gespräch. Schwebend: nur Offenes, sonst die schlanke Eingabe.
    fn show_conversation(self: &Rc<Self>) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        let n = conv.msgs.borrow().len();
        if self.pinned.get() {
            self.reset_list(0);
            self.expand();
        } else if conv.pid.get().is_some() || conv.unread.get() {
            let last_question = conv.msgs.borrow().iter().rposition(|m| m.kind == Kind::User).unwrap_or(0);
            self.reset_list(last_question);
            self.expand();
        } else {
            self.reset_list(n);
            self.collapse();
        }
        self.sync();
    }

    /// Schwebendes Fenster mit der Maus ziehen; Knöpfe behalten ihre eigenen Gesten.
    fn connect_move(self: &Rc<Self>, target: &impl IsA<gtk::Widget>) {
        let target = target.clone().upcast::<gtk::Widget>();
        let g = gtk::GestureDrag::new();
        g.set_button(0);
        let (w, t) = (Rc::downgrade(self), target.downgrade());
        g.connect_drag_begin(move |_, x, y| {
            let (Some(p), Some(t)) = (w.upgrade(), t.upgrade()) else { return };
            if p.pinned.get() {
                return;
            }
            let (cx, cy, _, _) = p.card_rect();
            let at = t.compute_point(&p.overlay, &gtk::graphene::Point::new(x as f32, y as f32));
            p.move_from.set(at.map(|a| (a.x() as f64, a.y() as f64, cx as f64, cy as f64)));
        });
        let (w, t) = (Rc::downgrade(self), target.downgrade());
        let started = Rc::new(Cell::new(false));
        let reset = started.clone();
        g.connect_drag_update(move |g, dx, dy| {
            let (Some(p), Some(t), Some((sx, sy))) = (w.upgrade(), t.upgrade(), g.start_point()) else { return };
            let Some((ax, ay, cx, cy)) = p.move_from.get() else { return };
            let Some(now) = t.compute_point(&p.overlay, &gtk::graphene::Point::new((sx + dx) as f32, (sy + dy) as f32)) else { return };
            let (mx, my) = (now.x() as f64 - ax, now.y() as f64 - ay);
            // erst ab etwas Weg: ein Tipp verschiebt nichts
            if mx.hypot(my) < 6.0 && !started.get() {
                return;
            }
            started.set(true);
            g.set_state(gtk::EventSequenceState::Claimed);
            let (ow, oh) = (p.overlay.width() as f64, p.overlay.height().max(1) as f64);
            let room = (ow - (ow.min(CARD_WIDTH as f64 + 32.0) - 32.0)).max(1.0);
            p.pos.set((((cx + mx) / room).clamp(0.0, 1.0), ((cy + my) / oh).clamp(0.0, 1.0)));
            p.overlay.queue_allocate();
        });
        let w = Rc::downgrade(self);
        g.connect_drag_end(move |_, _, _| {
            let moved = reset.replace(false);
            let Some(p) = w.upgrade() else { return };
            p.move_from.set(None);
            if moved {
                // gemerkt wird, wo das Fenster wirklich steht
                let (_, y, _, _) = p.card_rect();
                let (fx, _) = p.pos.get();
                let fy = y as f64 / p.overlay.height().max(1) as f64;
                p.pos.set((fx, fy));
                p.store(|c| (c.claude_x, c.claude_y) = (fx, fy));
            }
        });
        target.add_controller(g);
        if target == *self.handle.upcast_ref::<gtk::Widget>() {
            let twice = gtk::GestureClick::new();
            let w = Rc::downgrade(self);
            twice.connect_pressed(move |_, n, _, _| {
                if let Some(p) = w.upgrade().filter(|_| n == 2) {
                    p.pos.set(CARD_HOME);
                    p.overlay.queue_allocate();
                    p.store(|c| (c.claude_x, c.claude_y) = (-1.0, -1.0));
                }
            });
            target.add_controller(twice);
        }
    }

    /// Linken Rand der Spalte ziehen ändert ihre Breite.
    fn connect_edge(self: &Rc<Self>, edge: &gtk::Box) {
        let g = gtk::GestureDrag::new();
        g.set_button(0);
        let (w, handle) = (Rc::downgrade(self), edge.downgrade());
        g.connect_drag_update(move |g, dx, dy| {
            let (Some(p), Some(edge), Some((sx, sy))) = (w.upgrade(), handle.upgrade(), g.start_point()) else { return };
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(at) = edge.compute_point(&p.overlay, &gtk::graphene::Point::new((sx + dx) as f32, (sy + dy) as f32)) else { return };
            // das Dock muss daneben noch ganz hinpassen
            let most = COLUMN_WIDTH.1.min(p.overlay.width() - DOCK_WIDTH).max(COLUMN_WIDTH.0);
            p.column.set_size_request((p.overlay.width() - at.x() as i32).clamp(COLUMN_WIDTH.0, most), -1);
        });
        let w = Rc::downgrade(self);
        g.connect_drag_end(move |_, _, _| {
            if let Some(p) = w.upgrade() {
                let (width, _) = p.column.size_request();
                p.store(|c| c.claude_col_w = width);
            }
        });
        edge.add_controller(g);
    }

    fn tour_active(&self) -> bool {
        !self.tour.borrow().is_empty()
    }

    /// Höchste erlaubte Antworthöhe im aktuellen Fenster.
    fn height_limit(&self) -> i32 {
        let oh = self.overlay.height().max(300);
        let top = (self.pos.get().1 * oh as f64) as i32;
        (oh - top - DOCK_SPACE - 150).max(240).min(oh - 170).max(MIN_ANSWER)
    }

    fn expand(&self) {
        if !self.pinned.get() {
            // Vorgabe lässt die Notiz sichtbar; gezogene Höhe wird gemerkt
            let saved = crate::settings::Settings::load().claude_height;
            let h = if saved > 0 { saved } else { (self.overlay.height() as f64 * 0.42) as i32 };
            self.scroller.set_max_content_height(h.clamp(MIN_ANSWER, self.height_limit()));
        }
        self.revealer.set_reveal_child(true);
        self.update_bar();
    }

    /// Schlanke Eingabe; die Spalte zeigt immer das Gespräch.
    fn collapse(&self) {
        self.revealer.set_reveal_child(self.pinned.get());
        self.update_bar();
    }

    /// Die ganze Lernkarte ist Griff. Die Geste kommt vor den Kindern an: Ein Tipp erreicht
    /// noch die Knöpfe, erst ab etwas Weg gehört die Bewegung der Karte.
    fn connect_card_drag(self: &Rc<Self>) {
        let g = gtk::GestureDrag::new();
        g.set_button(gdk::BUTTON_PRIMARY);
        g.set_propagation_phase(gtk::PropagationPhase::Capture);
        let press = Rc::new(Cell::new(None::<(f64, f64)>));
        let (w, start) = (Rc::downgrade(self), press.clone());
        g.connect_drag_begin(move |g, x, y| {
            let Some(p) = w.upgrade() else { return };
            p.guide_drag.set(None);
            // Regler und Co. ziehen selbst, die Geste bleibt dann untätig
            start.set(if p.card_child_drags(g, x, y) { None } else { gesture_point(g, &p.overlay) });
        });
        let w = Rc::downgrade(self);
        g.connect_drag_update(move |g, _, _| {
            let (Some(p), Some(start)) = (w.upgrade(), press.get()) else { return };
            let Some(now) = gesture_point(g, &p.overlay) else { return };
            let base = p.guide_drag.get().or_else(|| {
                if (now.0 - start.0).hypot(now.1 - start.1) < CARD_DRAG_START {
                    return None;
                }
                g.set_state(gtk::EventSequenceState::Claimed);
                // ab hier zählt der Weg, die Karte macht beim Losgehen keinen Satz
                let base = (now.0, now.1, p.guide.margin_start() as f64, p.guide.margin_top() as f64);
                p.guide_drag.set(Some(base));
                Some(base)
            });
            if let Some((px, py, cx, cy)) = base {
                p.drag_card_to(cx + now.0 - px, cy + now.1 - py);
            }
        });
        let w = Rc::downgrade(self);
        g.connect_drag_end(move |_, _, _| {
            if let Some(p) = w.upgrade() {
                if p.guide_drag.take().is_some() && std::env::var("GRAPHIT_PROFILE").is_ok() {
                    eprintln!("PROFILE Karte bei {} {} Schritt {}", p.guide.margin_start(), p.guide.margin_top(), p.tour_idx.get() + 1);
                }
            }
        });
        self.guide.add_controller(g);
    }

    /// Regler und Eingabefelder brauchen das Ziehen selbst, mit der Maus auch der Text.
    fn card_child_drags(&self, g: &gtk::GestureDrag, x: f64, y: f64) -> bool {
        let mouse = g.device().is_some_and(|d| !matches!(d.source(), gdk::InputSource::Pen | gdk::InputSource::Touchscreen));
        let guide = self.guide.upcast_ref::<gtk::Widget>();
        let mut at = self.guide.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(w) = at.filter(|w| w != guide) {
            if w.is::<gtk::Scale>() || w.is::<gtk::Entry>() || w.is::<gtk::Text>() || (mouse && w.is::<gtk::TextView>()) {
                return true;
            }
            at = w.parent();
        }
        false
    }

    fn step_offset(&self) -> Option<(f64, f64)> {
        self.guide_offsets.borrow().get(self.tour_idx.get()).copied().flatten()
    }

    /// Freie Fläche für die Karte; die angeheftete Spalte bleibt frei.
    fn guide_view(&self) -> (f64, f64) {
        let side = if self.column.is_visible() { self.column.width() } else { 0 };
        ((self.overlay.width() - side) as f64, self.overlay.height() as f64)
    }

    fn guide_size(&self) -> (f64, f64) {
        (self.guide.width().max(310) as f64, self.guide.height().max(120) as f64)
    }

    fn guide_mark(&self) -> Option<Rect> {
        let view = self.tour_view.borrow().clone()?;
        let (page, area) = self.guide_anchor.get()?;
        self.page_on_overlay(&view, page, area)
    }

    /// Rechteck einer Seite im Fenster, so wie es nach dem laufenden Gleiten steht.
    fn page_on_overlay(&self, view: &DocView, page: usize, area: Rect) -> Option<Rect> {
        let r = view.canvas.page_rect_on_screen(page, area);
        let (dx, dy) = view.canvas.guide_rest_shift();
        let pt = |x: f64, y: f64| view.canvas.compute_point(&self.overlay, &gtk::graphene::Point::new((x + dx) as f32, (y + dy) as f32));
        let (a, b) = (pt(r.x0, r.y0)?, pt(r.x1, r.y1)?);
        Some(Rect::new(a.x() as f64, a.y() as f64, b.x() as f64, b.y() as f64))
    }

    /// Inhalt einer Seite, einmal je Stand der Notiz gerastert.
    fn page_ink_map(&self, view: &DocView, page: usize) -> Option<Rc<place::InkMap>> {
        let serial = view.canvas.history_serial();
        if let Some((_, m)) = self.guide_ink.borrow().get(&page).filter(|e| e.0 == serial) {
            return Some(m.clone());
        }
        let map = Rc::new(place::ink_map(&*view.canvas.page_data(page)?)?);
        self.guide_ink.borrow_mut().insert(page, (serial, map.clone()));
        Some(map)
    }

    /// Beste Lage der Karte für die aktuelle Ansicht, als Abstand zu `anchor`.
    fn solve_card_spot(&self, view: &DocView, page: usize, anchor: Rect, card: (f64, f64), near: Option<(f64, f64)>) -> (f64, f64) {
        let c = &view.canvas;
        let (w, h) = self.guide_view();
        let origin = c.compute_point(&self.overlay, &gtk::graphene::Point::new(0.0, 0.0));
        let (ox, oy) = origin.map_or((0.0, 0.0), |p| (p.x() as f64, p.y() as f64));
        let area = Rect::new(ox.max(0.0) + CARD_PAD, oy.max(0.0) + CARD_PAD, (ox + c.width() as f64).min(w) - CARD_PAD, (oy + c.height() as f64).min(h - DOCK_SPACE as f64));
        // ganze Seiten im Fenster mit ihrem Inhalt
        let sheets: Vec<(Rect, Rc<place::InkMap>)> = (page.saturating_sub(1)..=page + 1)
            .filter_map(|p| {
                let (pw, ph) = c.page_size(p)?;
                let on_screen = self.page_on_overlay(view, p, Rect::new(0.0, 0.0, pw, ph))?;
                let visible = on_screen.intersects(&area);
                visible.then(|| self.page_ink_map(view, p).map(|m| (on_screen, m))).flatten()
            })
            .collect();
        let ink = |r: &Rect| -> f64 {
            sheets
                .iter()
                .filter(|(s, _)| s.intersects(r))
                .map(|(s, m)| {
                    let z = s.width() / m.width();
                    m.covered(&Rect::new((r.x0 - s.x0) / z, (r.y0 - s.y0) / z, (r.x1 - s.x0) / z, (r.y1 - s.y0) / z)) * z * z
                })
                .sum()
        };
        let (shapes, link_points) = c.guide_shapes();
        let mut marks: Vec<Rect> = shapes
            .iter()
            .filter_map(|(r, labeled)| {
                let s = self.page_on_overlay(view, page, *r)?;
                Some(if *labeled { Rect::new(s.x0, s.y0 - LABEL_SPACE, s.x1, s.y1) } else { s })
            })
            .collect();
        if marks.is_empty() {
            marks.push(anchor);
        }
        let links: Vec<Pt> = link_points
            .iter()
            .filter_map(|q| self.page_on_overlay(view, page, Rect::new(q.x, q.y, q.x, q.y)).map(|s| Pt::new(s.x0, s.y0)))
            .collect();
        let (x, y) = best_card_spot(area, card, &marks, &links, ink, near);
        (x - anchor.x0, y - anchor.y0)
    }

    /// Lage der Karte zur Stelle. Neu gerechnet wird nur, wenn sich etwas Wesentliches
    /// ändert: Größe der Karte, Fenster, Zoom oder die Stelle wird weggescrollt.
    fn card_spot(&self, anchor: Rect) -> (f64, f64) {
        let (card, view_size, bottom) = (self.guide_size(), self.guide_view(), DOCK_SPACE as f64);
        let Some(view) = self.tour_view.borrow().clone() else { return (0.0, 0.0) };
        let Some((page, _)) = self.guide_anchor.get() else { return (0.0, 0.0) };
        let zoom = view.canvas.zoom();
        let placed = self.guide_spot.get();
        let fresh = placed.is_some_and(|s| {
            let at = (anchor.x0 + s.offset.0, anchor.y0 + s.offset.1);
            let fit = fit_card(at, card, view_size, bottom);
            let on_screen = anchor.intersects(&Rect::new(0.0, 0.0, view_size.0, view_size.1));
            let pushed = view.canvas.guide_settled() && on_screen && (at.0 - fit.0).abs().max((at.1 - fit.1).abs()) > 40.0;
            (s.card.0 - card.0).abs() <= 16.0 && (s.card.1 - card.1).abs() <= 16.0 && s.view == view_size && (s.zoom - zoom).abs() < 1e-3 && !pushed
        });
        if fresh {
            return placed.map_or((0.0, 0.0), |s| s.offset);
        }
        let near = placed.map(|s| (anchor.x0 + s.offset.0, anchor.y0 + s.offset.1));
        let offset = self.solve_card_spot(&view, page, anchor, card, near);
        self.guide_spot.set(Some(CardSpot { offset, card, view: view_size, zoom }));
        offset
    }

    /// Ganz aus dem Fenster kann die Karte nie.
    fn clamp_card(&self, x: f64, y: f64) -> (f64, f64) {
        let ((w, h), (cw, ch)) = (self.guide_view(), self.guide_size());
        (x.clamp(8.0, (w - cw - 8.0).max(8.0)), y.clamp(8.0, (h - ch - 8.0).max(8.0)))
    }

    /// Gezogene Lage merken, als Abstand zur Stelle oder ohne Stelle zum Fenster.
    fn drag_card_to(&self, x: f64, y: f64) {
        let (x, y) = self.clamp_card(x, y);
        let (ax, ay) = self.guide_mark().map_or((0.0, 0.0), |r| (r.x0, r.y0));
        if let Some(o) = self.guide_offsets.borrow_mut().get_mut(self.tour_idx.get()) {
            *o = Some((x - ax, y - ay));
        }
    }

    /// Griff ziehen: Oberkante bleibt, die Unterkante folgt dem Stift.
    fn connect_grip(self: &Rc<Self>, grip: &gtk::Box) {
        let g = gtk::GestureDrag::new();
        g.set_button(0);
        let (w, handle) = (Rc::downgrade(self), grip.downgrade());
        g.connect_drag_update(move |g, dx, dy| {
            let (Some(p), Some(grip), Some((sx, sy))) = (w.upgrade(), handle.upgrade(), g.start_point()) else { return };
            g.set_state(gtk::EventSequenceState::Claimed);
            let pt = |wd: &gtk::Widget, x: f64, y: f64| wd.compute_point(&p.overlay, &gtk::graphene::Point::new(x as f32, y as f32));
            let (Some(at), Some(top)) = (pt(grip.upcast_ref(), sx + dx, sy + dy), pt(p.scroller.upcast_ref(), 0.0, 0.0)) else {
                return;
            };
            let h = ((at.y() - top.y()) as i32 - 10).clamp(MIN_ANSWER, p.height_limit());
            p.scroller.set_max_content_height(h);
        });
        let w = Rc::downgrade(self);
        g.connect_drag_end(move |_, _, _| {
            if let Some(p) = w.upgrade() {
                let h = p.scroller.max_content_height();
                p.store(|c| c.claude_height = h);
            }
        });
        grip.add_controller(g);
    }

    pub fn close(&self) {
        self.hide_keyboard();
        self.speaker.stop();
        self.followup.replace(None);
        // ein nicht gefragter Ausschnitt soll nicht bei der nächsten Frage auftauchen
        if self.region.replace(None).is_some() {
            self.attachments.borrow_mut().clear();
            while let Some(c) = self.thumbs.first_child() {
                self.thumbs.remove(&c);
            }
        }
        self.clamp.set_visible(false);
        self.column.set_visible(false);
        if let Some(v) = self.note() {
            v.canvas.grab_focus();
        }
    }

    /// Nach einer Aktion Platz machen. Die Spalte verdeckt nichts und bleibt.
    fn step_aside(&self) {
        if !self.pinned.get() {
            self.close();
        }
    }

    /// Esc: Dialog zu, sonst Erklärung beenden, sonst Dialog auf.
    pub fn key_escape(self: &Rc<Self>, key: gdk::Key) -> bool {
        if key != gdk::Key::Escape {
            return false;
        }
        if self.is_open() {
            self.close();
        } else if self.tour_active() {
            self.end_tour();
        } else {
            self.open();
        }
        true
    }

    /// Pfeil links/rechts blättert durch die Karten. Texteingaben behalten ihre Pfeile,
    /// und auf der letzten Karte ist Schluss, damit gehaltene Tasten die Führung nicht beenden.
    pub fn key_arrow(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        let d = match key {
            gdk::Key::Left => -1,
            gdk::Key::Right => 1,
            _ => return false,
        };
        let held = gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK;
        if mods.intersects(held) || !self.tour_active() || !self.guide.is_visible() {
            return false;
        }
        let focus = self.guide.root().and_then(|r| gtk::prelude::RootExt::focus(&r));
        if focus.is_some_and(|f| f.is::<gtk::Editable>() || f.is::<gtk::TextView>()) {
            return false;
        }
        if d < 0 || self.tour_idx.get() + 1 < self.tour.borrow().len() {
            self.step(d);
        }
        true
    }

    fn model_alias(&self) -> &'static str {
        MODELS[(self.model.selected() as usize).min(MODELS.len() - 1)].1
    }

    fn effort_value(&self) -> &'static str {
        EFFORTS[(self.effort.selected() as usize).min(EFFORTS.len() - 1)].1
    }

    /// Öffnen per Knopf oder Radialmenü: Stift oder Finger, also meist ohne Tastatur.
    pub fn toggle_touch(self: &Rc<Self>) {
        if self.is_open() {
            self.close();
        } else {
            self.open();
            self.show_keyboard();
        }
    }

    /// GNOME zeigt seine Bildschirmtastatur beim Stift nur, wenn die Bedienungshilfe an ist.
    fn show_keyboard(self: &Rc<Self>) {
        // aus der Datei, damit der Schalter auch ohne offene Notiz (Bibliothek) gilt
        if !crate::settings::Settings::load().claude_osk {
            return;
        }
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE Bildschirmtastatur einschalten (im Testlauf ausgelassen)");
            return;
        }
        let Some(s) = a11y_settings() else { return };
        if !s.boolean(A11Y_KEYBOARD) {
            let _ = std::fs::write(keyboard_marker(), "");
            if s.set_boolean(A11Y_KEYBOARD, true).is_ok() {
                self.keyboard_on.set(true);
            }
        }
        // Die Shell legt die Tastatur erst nach der Änderung an, danach kann das Feld sie anfordern
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            if let Some(p) = w.upgrade().filter(|p| p.is_open()) {
                p.entry.grab_focus();
                p.request_keyboard();
            }
        });
    }

    /// GTK fordert die Tastatur selbst nur bei einem Klick ins Feld an. Dasselbe von Hand:
    /// über den Eingabekontext des Textfelds, den dessen Tasten-Controller verrät.
    fn request_keyboard(&self) {
        let text = self.entry.delegate();
        let controllers = text.as_ref().map(|t| t.observe_controllers());
        let contexts: Vec<gtk::IMContext> = controllers
            .iter()
            .flat_map(|list| (0..list.n_items()).filter_map(|i| list.item(i)))
            .filter_map(|c| c.downcast::<gtk::EventControllerKey>().ok())
            .filter_map(|k| k.im_context())
            .collect();
        for c in contexts {
            c.activate_osk(None::<gdk::Event>);
        }
    }

    fn hide_keyboard(&self) {
        if self.keyboard_on.replace(false) {
            if let Some(s) = a11y_settings() {
                let _ = s.set_boolean(A11Y_KEYBOARD, false);
            }
            let _ = std::fs::remove_file(keyboard_marker());
        }
    }

    /// Eingekreiste Stelle: Fenster mit Ausschnitt, „Erklär das“ und Schreibfeld.
    pub fn ask_region(self: &Rc<Self>, png: &[u8], page: usize, area: [f64; 4]) {
        self.open();
        self.attach_bytes(png);
        self.region.replace(Some(format!(
            "[Der Nutzer hat auf Seite {} diesen Bereich eingekreist: [{:.3},{:.3},{:.3},{:.3}]. Das angehängte Bild zeigt ihn.]\n",
            page + 1,
            area[0],
            area[1],
            area[2],
            area[3]
        )));
        self.pen_btn.set_active(true);
        self.update_bar();
    }

    /// Anderer Tab, während der Dialog offen ist: das Gespräch des neuen Moduls zeigen.
    pub fn tab_changed(self: &Rc<Self>) {
        if self.is_open() {
            self.open();
        }
    }

    pub fn open(self: &Rc<Self>) {
        let view = self.note();
        if let Some(v) = &view {
            // Ausgewählte Stelle geht als Bild mit
            if let Some(png) = v.canvas.selection_png() {
                self.attach_bytes(&png);
            }
            v.canvas.commit_edits();
        }
        // Standard aus den Einstellungen, solange im Dialog nichts gewählt wurde; auch ohne offene Notiz
        let (model, effort) = match &view {
            Some(v) => {
                let st = v.canvas.st();
                (st.cfg.claude_model.clone(), st.cfg.claude_effort.clone())
            }
            None => {
                let cfg = crate::settings::Settings::load();
                (cfg.claude_model, cfg.claude_effort)
            }
        };
        self.model_setting.set(true);
        if !self.model_chosen.get() {
            self.model.set_selected(MODELS.iter().position(|m| m.1 == model).unwrap_or(2) as u32);
        }
        if !self.effort_chosen.get() {
            self.effort.set_selected(EFFORTS.iter().position(|e| e.1 == effort).unwrap_or(0) as u32);
        }
        self.model_setting.set(false);
        let note = view.as_ref().and_then(|v| v.canvas.st().doc_key());
        let dir = module_dir(note.as_deref(), &self.root);
        let conv = self.convs.borrow_mut().entry(dir.clone()).or_insert_with(|| Conv::load(&dir, &self.root)).clone();
        self.pin_title.set_text(&conv.name);
        self.model_setting.set(true);
        self.style.set_selected(conv.style.get() as u32);
        self.model_setting.set(false);
        self.entry.set_placeholder_text(Some(&crate::tr!("Frag Claude zu {} …", "Ask Claude · {} …", conv.name)));
        self.conv.replace(Some(conv.clone()));
        self.index_module(&conv);
        self.rebuild_quick();
        if !self.is_open() {
            self.reveal();
        }
        self.show_conversation();
        let n = conv.msgs.borrow().len();
        conv.unread.set(false);
        self.history_btn.set_active(false);
        self.history_btn.set_sensitive(n > 0);
        self.quick.unselect_all();
        self.entry.grab_focus();
        if let Some(v) = view {
            self.export(&v, &conv);
        }
        let (waiting, hints) = conv.pending_tour.take();
        if !waiting.is_empty() {
            self.step_aside();
            self.start_tour(waiting, hints);
        }
    }

    fn reset_list(&self, first: usize) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        self.first.set(first);
        self.rendered.set(first);
        self.streaming.replace(None);
        self.rich_rows.borrow_mut().clear();
        self.stick.set(true);
    }

    fn show_history(self: &Rc<Self>, on: bool) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        let n = conv.msgs.borrow().len();
        if on {
            self.reset_list(0);
            self.expand();
        } else if conv.pid.get().is_none() {
            self.reset_list(n);
            self.collapse();
        }
        self.sync();
    }

    fn new_conversation(self: &Rc<Self>) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        conv.stop();
        conv.session.replace(None);
        conv.msgs.borrow_mut().clear();
        conv.context.set(0);
        conv.set_memory(None);
        conv.save();
        self.history_btn.set_active(false);
        self.history_btn.set_sensitive(false);
        self.reset_list(0);
        self.collapse();
        (self.toast)(&crate::tr!("Neues Gespräch für {}", "New conversation for {}", conv.name));
    }

    // ---------------------------------------------------------------- Bilder

    fn attach_path(&self) -> Option<PathBuf> {
        let conv = self.conv.borrow().clone();
        let dir = conv.map(|c| c.dir.clone()).or_else(|| {
            let note = self.note().and_then(|v| v.canvas.st().doc_key());
            Some(module_dir(note.as_deref(), &self.root))
        })?;
        let cache = cache_dir(&dir);
        std::fs::create_dir_all(&cache).ok()?;
        let n = self.attach_serial.get() + 1;
        self.attach_serial.set(n);
        Some(cache.join(format!("anhang-{n}.png")))
    }

    fn attach_bytes(self: &Rc<Self>, png: &[u8]) {
        if let Some(p) = self.attach_path() {
            if std::fs::write(&p, png).is_ok() {
                self.add_thumb(p);
            }
        }
    }

    pub fn attach_image(self: &Rc<Self>, src: &Path) {
        self.attach_file(src);
    }

    fn attach_file(self: &Rc<Self>, src: &Path) {
        let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !["png", "jpg", "jpeg", "webp", "gif"].contains(&ext.as_str()) {
            (self.toast)(crate::i18n::tl("Nur Bilder lassen sich anhängen"));
            return;
        }
        if let Some(p) = self.attach_path().map(|p| p.with_extension(&ext)) {
            if std::fs::copy(src, &p).is_ok() {
                self.add_thumb(p);
            }
        }
    }

    fn add_thumb(self: &Rc<Self>, path: PathBuf) {
        let pic = gtk::Picture::for_filename(&path);
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_size_request(30, 30);
        let b = gtk::Button::new();
        b.set_child(Some(&pic));
        b.set_tooltip_text(Some(crate::i18n::tl("Bild entfernen")));
        b.add_css_class("flat");
        b.add_css_class("fk-claude-thumb");
        let w = Rc::downgrade(self);
        let p = path.clone();
        b.connect_clicked(move |b| {
            if let Some(panel) = w.upgrade() {
                panel.attachments.borrow_mut().retain(|q| *q != p);
                panel.thumbs.remove(b);
                panel.update_bar();
            }
        });
        self.thumbs.append(&b);
        self.attachments.borrow_mut().push(path);
        self.update_bar();
    }

    fn paste_image(self: &Rc<Self>) -> bool {
        let clip = self.entry.clipboard();
        if !clip.formats().contains_type(gdk::Texture::static_type()) {
            return false;
        }
        let w = Rc::downgrade(self);
        clip.read_texture_async(gio::Cancellable::NONE, move |res| {
            let (Some(p), Ok(Some(tex))) = (w.upgrade(), res) else { return };
            p.attach_bytes(&tex.save_to_png_bytes());
        });
        true
    }

    fn choose_image(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title(crate::i18n::tl("Bild anhängen")).modal(true).build();
        let root = self.overlay.root().and_downcast::<gtk::Window>();
        let w = Rc::downgrade(self);
        dialog.open(root.as_ref(), gio::Cancellable::NONE, move |res| {
            if let (Some(p), Ok(f)) = (w.upgrade(), res) {
                if let Some(path) = f.path() {
                    p.attach_file(&path);
                }
            }
        });
    }

    // ---------------------------------------------------------------- Anzeige

    /// Neue Nachrichten anhängen, die laufende Antwort und den Status nachziehen.
    fn sync(self: &Rc<Self>) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        let running = conv.pid.get().is_some();
        let count = conv.msgs.borrow().len();
        for i in self.rendered.get()..count {
            self.add_row(&conv, i);
        }
        self.rendered.set(count.max(self.first.get()));
        if let Some((rich, button, box_)) = &*self.streaming.borrow() {
            let last = conv.msgs.borrow().last().filter(|m| m.kind == Kind::Claude).map(|m| index::marks(&m.text));
            if let Some(m) = &last {
                if m.clean != rich.source() {
                    rich.set(&m.clean, &self.engine);
                }
                // Quellen erst am Ende, dann ist die Marke sicher vollständig
                if !running && box_.first_child().is_none() {
                    self.source_buttons(&conv, &m.sources, box_);
                }
            }
            button.set_visible(!running);
        }
        // Schaltfläche unter der Antwort erscheint erst am Ende: auch sie noch zeigen
        if !running && self.stick.get() {
            let adj = self.scroller.vadjustment();
            glib::timeout_add_local_once(Duration::from_millis(80), move || adj.set_value(adj.upper() - adj.page_size()));
        }
        self.status_row.set_visible(running);
        self.status.set_text(&conv.status.borrow());
        self.action_btn.set_icon_name(if running { "media-playback-stop-symbolic" } else { "go-up-symbolic" });
        self.action_btn.set_tooltip_text(Some(if running { crate::i18n::tl("Anhalten") } else { crate::i18n::tl("Senden") }));
        self.update_bar();
    }

    fn add_row(self: &Rc<Self>, conv: &Rc<Conv>, i: usize) {
        let (kind, text) = {
            let m = conv.msgs.borrow();
            (m[i].kind, m[i].text.clone())
        };
        let label = gtk::Label::new(None);
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        label.set_selectable(true);
        label.set_can_focus(false);
        match kind {
            Kind::User => {
                label.set_text(&text);
                label.add_css_class("fk-claude-user");
                label.set_halign(gtk::Align::End);
                self.list.append(&label);
            }
            Kind::Tool => {
                label.set_text(&text);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
                label.set_halign(gtk::Align::Start);
                self.list.append(&label);
            }
            Kind::Error => {
                label.set_text(&text);
                label.add_css_class("error");
                label.set_halign(gtk::Align::Start);
                self.list.append(&label);
            }
            Kind::Claude => {
                // Formeln passen sich der Breite an, in der sie stehen
                let room = if self.pinned.get() { self.column.size_request().0 } else { CARD_WIDTH };
                let rich = Rich::new((room - 56) as f64);
                let found = index::marks(&text);
                rich.set(&found.clean, &self.engine);
                let sources = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                self.source_buttons(conv, &found.sources, &sources);
                let insert = gtk::Button::with_label(crate::i18n::tl("In Notiz einfügen"));
                insert.add_css_class("flat");
                insert.add_css_class("caption");
                insert.add_css_class("fk-claude-insert");
                insert.set_halign(gtk::Align::Start);
                let speak = gtk::Button::from_icon_name("audio-volume-high-symbolic");
                speak.add_css_class("flat");
                speak.add_css_class("fk-claude-insert");
                speak.set_tooltip_text(Some(crate::i18n::tl("Vorlesen")));
                let w = Rc::downgrade(self);
                let c = Rc::downgrade(conv);
                speak.connect_clicked(move |_| {
                    let (Some(p), Some(c)) = (w.upgrade(), c.upgrade()) else { return };
                    if let Some(m) = c.msgs.borrow().get(i) {
                        p.speaker.say(&index::marks(&m.text).clean);
                    }
                });
                let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                actions.append(&insert);
                actions.append(&speak);
                let w = Rc::downgrade(self);
                let c = Rc::downgrade(conv);
                insert.connect_clicked(move |_| {
                    let (Some(p), Some(c)) = (w.upgrade(), c.upgrade()) else { return };
                    let text = c.msgs.borrow().get(i).map(|m| index::marks(&m.text).clean);
                    if let (Some(v), Some(t)) = (p.note(), text) {
                        insert_text(v, None, t);
                        p.step_aside();
                    }
                });
                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                row.append(&rich.root);
                row.append(&sources);
                row.append(&actions);
                self.list.append(&row);
                self.rich_rows.borrow_mut().push(rich.clone());
                self.streaming.replace(Some((rich, actions, sources)));
            }
        }
    }

    /// Aktuelle Seite, ganze Notiz und Schreibzeilen ablegen, solange der Nutzer noch tippt.
    fn export(&self, v: &Rc<DocView>, conv: &Rc<Conv>) {
        let (stand, current, pages) = {
            let st = v.canvas.st();
            let pages: Vec<_> = st.doc.pages.iter().map(|p| { let p = p.borrow(); p.snapshot(st.doc.page_pdf(&p)) }).collect();
            ((st.doc_key(), st.history.serial), st.current_page(), pages)
        };
        let cache = cache_dir(&conv.dir);
        let _ = std::fs::create_dir_all(&cache);
        let words = v.canvas.page_words(current);
        let data = data_dir(&conv.dir);
        let whole = conv.exported.borrow().as_ref() != Some(&stand);
        conv.exported.replace(Some(stand));
        let busy = conv.exporting.clone();
        busy.set(busy.get() + 1);
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || {
                if let Some(p) = pages.get(current) {
                    let _ = crate::window::export_pages(std::slice::from_ref(p), &cache.join(SEITE_PDF));
                    place::write_geometry(&cache, current, p, &words);
                }
                if whole {
                    index::offer_transcripts(&data, &cache, &pages);
                    let _ = crate::window::export_pages(&pages, &cache.join(NOTIZ_PDF));
                    let _ = std::fs::write(cache.join(INK_FILE), ink_json(&pages));
                }
            })
            .await;
            busy.set(busy.get() - 1);
        });
    }

    pub fn ask(self: &Rc<Self>, question: &str) {
        if !self.is_open() {
            self.open();
        }
        self.entry.set_text(question);
        self.send();
    }

    fn viewer_context(&self) -> String {
        let mut out = String::new();
        for (name, label) in [("viewer1", "Lese-Viewer oben"), ("viewer2", "Lese-Viewer unten")] {
            if let Some(v) = (self.view_for)(name) {
                let st = v.canvas.st();
                if let Some(k) = st.doc_key() {
                    out.push_str(&format!("{label} ({name}): {} (Seite {})\n", k.display(), st.current_page() + 1));
                }
            }
        }
        out
    }

    fn send(self: &Rc<Self>) {
        let Some(conv) = self.conv.borrow().clone() else { return };
        let question = self.entry.text().trim().to_string();
        let handwritten = !self.strokes.borrow().is_empty();
        let empty = question.is_empty() && self.attachments.borrow().is_empty() && !handwritten;
        if !empty && conv.compacting.get() {
            conv.send_after.set(true);
            return;
        }
        if empty || conv.pid.get().is_some() {
            return;
        }
        conv.serial.set(conv.serial.get() + 1);
        let pad_image = if handwritten {
            let n = self.attachments.borrow().len();
            if let Some(png) = self.pad_png() {
                self.attach_bytes(&png);
            }
            self.attachments.borrow().get(n).cloned()
        } else {
            None
        };
        self.clear_pad();
        self.pen_btn.set_active(false);
        let images: Vec<PathBuf> = self.attachments.borrow().clone();
        self.entry.set_text("");
        self.attachments.borrow_mut().clear();
        while let Some(c) = self.thumbs.first_child() {
            self.thumbs.remove(&c);
        }
        let others = images.len() - pad_image.is_some() as usize;
        let mut shown = question.clone();
        if pad_image.is_some() {
            shown = crate::tr!("{shown}\n(Frage mit dem Stift geschrieben)", "{shown}\n(question written with the pen)").trim().to_string();
        }
        if others > 0 {
            let pictures = if others == 1 { crate::i18n::tl("1 Bild").to_string() } else { crate::tr!("{others} Bilder", "{others} images") };
            shown = crate::tr!("{shown}\n({pictures} angehängt)", "{shown}\n({pictures} attached)").trim().to_string();
        }
        conv.push(Kind::User, shown);
        self.set_chips(&[]);
        conv.status.replace(crate::i18n::tl("Claude denkt nach …").into());
        // schon als laufend markieren, auch wenn die PDFs noch geschrieben werden
        conv.pid.set(Some(0));
        self.history_btn.set_sensitive(true);
        self.expand();
        self.sync();
        let view = self.note();
        let (note, page, pages) = view
            .as_ref()
            .map(|v| {
                let st = v.canvas.st();
                (st.doc_key(), st.current_page(), st.doc.pages.len())
            })
            .unwrap_or((None, 0, 0));
        if let Some(v) = &view {
            self.export(v, &conv);
        }
        let origin = Origin { view: view.as_ref().map(Rc::downgrade), page };
        let cache = cache_dir(&conv.dir);
        let mut context = match &note {
            Some(n) => format!(
                "[Graphit] Offene Notiz: {} (Seite {} von {pages}).\nAktuelle Seite als PDF: {}\nGanze Notiz als PDF: {}\nSchreibzeilen: {}\n",
                n.display(),
                page + 1,
                cache.join(SEITE_PDF).display(),
                cache.join(NOTIZ_PDF).display(),
                cache.join(INK_FILE).display()
            ),
            None => "[Graphit] Keine Notiz offen.\n".to_string(),
        };
        context.push_str(&self.viewer_context());
        for i in &images {
            if Some(i) == pad_image.as_ref() {
                context.push_str(&format!("Die Frage des Nutzers steht handgeschrieben in diesem Bild, bitte ansehen: {}\n", i.display()));
            } else {
                context.push_str(&format!("Angehängtes Bild, bitte ansehen: {}\n", i.display()));
            }
        }
        context.push_str(STYLES[conv.style.get()].1);
        if let Some(f) = self.followup.take() {
            context.push_str(&f);
        }
        if let Some(r) = self.region.take() {
            context.push_str(&r);
        }
        let shaky: Vec<String> = index::load_progress(&data_dir(&conv.dir)).into_iter().filter(|t| !t.sure).take(index::TOPICS_IN_PROMPT).map(|t| t.name).collect();
        if !shaky.is_empty() {
            context.push_str(&format!("[Lernstand: wackelig sind {}]\n", shaky.join(", ")));
        }
        let quiz = conv.quiz_log.take();
        if !quiz.is_empty() {
            context.push_str(&format!("[Quizantworten seit der letzten Frage]\n{}\n", quiz.join("\n")));
        }
        // eine neue Session kennt das Bisherige nur aus der Zusammenfassung
        let memory = conv.memory.borrow().clone().filter(|_| conv.session.borrow().is_none());
        let remembered = memory.map(|m| format!("[Gedächtnis aus dem bisherigen Gespräch zu diesem Modul]\n{m}\n\n")).unwrap_or_default();
        let prompt = format!("{remembered}{context}\n{question}");
        let (model, effort) = (self.model_alias(), self.effort_value());
        let p = self.clone();
        glib::spawn_future_local(async move {
            // die PDFs sind meist längst fertig; große Notizen warten höchstens kurz
            for _ in 0..100 {
                if conv.exporting.get() == 0 {
                    break;
                }
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            p.run(conv, prompt, origin, model, effort, false).await;
        });
    }

    async fn run(self: &Rc<Self>, conv: Rc<Conv>, prompt: String, origin: Origin, model: &'static str, effort: &'static str, retried: bool) {
        let cache = cache_dir(&conv.dir);
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::remove_file(cache.join(TOUR_FILE));
        let _ = std::fs::remove_file(cache.join(place::RESULT));
        let _ = std::fs::remove_file(cache.join(place::REQUEST));
        place::write_guide(&cache);
        let _ = std::fs::write(cache.join(LECTURE_GUIDE_FILE), LECTURE_GUIDE.replace("CACHE", &cache.display().to_string()));
        let session = conv.session.borrow().clone();
        let started = match launch(&conv.dir, &cache, &prompt, session.as_deref(), model, effort) {
            Ok(l) => l,
            Err(e) => {
                conv.pid.set(None);
                conv.push(Kind::Error, crate::tr!("Claude Code ließ sich nicht starten: {e}", "Claude Code could not be started: {e}"));
                conv.save();
                self.sync();
                return;
            }
        };
        conv.pid.set(Some(started.pid));
        // Claude kann schreiben und das Ergebnis noch während der Antwort prüfen
        let polling = Rc::new(Cell::new(true));
        let poller = {
            let (p, c, o, flag) = (self.clone(), conv.clone(), origin.clone(), polling.clone());
            glib::spawn_future_local(async move {
                while flag.get() {
                    glib::timeout_future(Duration::from_millis(250)).await;
                    p.apply_changes(&c, &o, false).await;
                }
            })
        };
        let mut failure = None;
        while let Ok(ev) = started.events.recv().await {
            match ev {
                Event::Session(s) => {
                    conv.session.replace(Some(s));
                }
                Event::TextStart => {
                    conv.push(Kind::Claude, "");
                    conv.status.replace(crate::i18n::tl("Claude schreibt …").into());
                }
                Event::Text(t) => {
                    let mut m = conv.msgs.borrow_mut();
                    match m.last_mut().filter(|m| m.kind == Kind::Claude) {
                        Some(last) => last.text.push_str(&t),
                        None => m.push(Msg { kind: Kind::Claude, text: t }),
                    }
                }
                Event::Tool { text, lasting } => {
                    if lasting {
                        conv.push(Kind::Tool, text.clone());
                    }
                    conv.status.replace(format!("{text} …"));
                }
                Event::Context(n) => conv.context.set(n),
                Event::Finished { error } => {
                    failure = error;
                    break;
                }
            }
            self.sync_if_bound(&conv);
        }
        polling.set(false);
        let _ = poller.await;
        self.apply_changes(&conv, &origin, true).await;
        conv.pid.set(None);
        // Session gibt es nicht mehr (z. B. aufgeräumt): einmal neu anfangen
        if failure.as_deref().is_some_and(|e| e.contains("No conversation found")) && !retried {
            conv.session.replace(None);
            conv.pid.set(Some(0));
            return Box::pin(self.run(conv, prompt, origin, model, effort, true)).await;
        }
        let finished_clean = failure.is_none() && !conv.stopped.get();
        let lecture = conv.lecture.replace(false);
        let repairing = conv.repairing.replace(false);
        let insert = conv.insert_at.take();
        if conv.stopped.replace(false) {
            conv.push(Kind::Tool, crate::i18n::tl("Angehalten"));
        } else if let Some(e) = failure {
            let hint = if e.contains("authenticate") { crate::i18n::tl("\nMelde dich im Terminal neu an: claude") } else { "" };
            conv.push(Kind::Error, format!("{e}{hint}"));
        }
        index::collect_transcripts(&data_dir(&conv.dir), &cache);
        // Vorlesungsmodus: Claude sagt, wo der nächste Abschnitt beginnt
        let next = conv.msgs.borrow()[conv.msgs.borrow().iter().rposition(|m| m.kind == Kind::User).unwrap_or(0)..].iter().filter(|m| m.kind == Kind::Claude).find_map(|m| index::marks(&m.text).next);
        if let (Some(next), Some(n)) = (next, origin.view.as_ref().and_then(Weak::upgrade).and_then(|v| v.canvas.st().doc_key())) {
            index::set_lecture_next(&data_dir(&conv.dir), &n, next);
            self.tour_next.set(next);
            self.quick_page.set(None);
            if next.is_none() {
                (self.toast)(crate::i18n::tl("Vorlesung durchgearbeitet"));
            }
        } else if insert.is_none() && !repairing {
            self.tour_next.set(None);
        }
        // Einschätzungen aus dieser Antwort in den Lernstand übernehmen
        let start = conv.msgs.borrow().iter().rposition(|m| m.kind == Kind::User).unwrap_or(0);
        let topics: Vec<(String, bool)> = conv.msgs.borrow()[start..].iter().filter(|m| m.kind == Kind::Claude).flat_map(|m| index::marks(&m.text).topics).collect();
        if !topics.is_empty() {
            let data = data_dir(&conv.dir);
            let mut all = index::load_progress(&data);
            index::update_topics(&mut all, &topics);
            index::save_progress(&data, &all);
            self.quick_page.set(None);
        }
        let raw_tour = std::fs::read_to_string(cache.join(TOUR_FILE)).unwrap_or_default();
        let (tour, hints) = parse_tour(&raw_tour);
        if let Some(after) = insert {
            let _ = std::fs::remove_file(cache.join(TOUR_FILE));
            conv.save();
            self.insert_steps(after, tour);
            conv.last_run.set(Some((model, effort)));
            self.plan_compact(conv);
            return;
        }
        // Text statt Führung ist beim Vorlesungsdurchgang kein Ergebnis: einmal nachfordern
        if lecture && tour.is_empty() && finished_clean {
            conv.repairing.set(true);
            conv.status.replace(crate::i18n::tl("Claude baut die Führung …").into());
            conv.pid.set(Some(0));
            self.sync_if_bound(&conv);
            let repair = format!("[Graphit] Es liegt keine gültige fuehrung.json vor. Der Nutzer will den Durchgang interaktiv, nicht als Text. Schreibe jetzt {} nach der Anleitung in {}. Erkläre nichts im Chat.", cache.join(TOUR_FILE).display(), cache.join(LECTURE_GUIDE_FILE).display());
            return Box::pin(self.run(conv, repair, origin, model, effort, retried)).await;
        }
        if repairing && tour.is_empty() && finished_clean {
            conv.push(Kind::Error, crate::i18n::tl("Claude hat keine Führung erstellt. Bitte versuche es noch einmal."));
        }
        let adaptive = lecture || repairing;
        if !tour.is_empty() {
            // zum späteren Abspielen ohne neue Anfrage
            let note = origin.view.as_ref().and_then(Weak::upgrade).and_then(|v| v.canvas.st().doc_key());
            let title = conv.msgs.borrow().iter().rev().find(|m| m.kind == Kind::User).map(|m| short_title(&m.text)).unwrap_or_default();
            if let Some(n) = note {
                save_tour(&conv.dir, &n, tour[0].page.unwrap_or(origin.page), &title, &raw_tour);
                self.quick_page.set(None);
            }
        }
        let _ = std::fs::remove_file(cache.join(TOUR_FILE));
        conv.save();
        let seen = self.is_open() && self.conv.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, &conv));
        if seen {
            self.sync();
            if tour.is_empty() {
                self.set_chips(&hints);
            }
        }
        if !tour.is_empty() {
            let home = origin.view.as_ref().and_then(Weak::upgrade);
            let here = match (&home, self.note()) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, &b),
                _ => false,
            };
            if here {
                self.step_aside();
                self.tour_adaptive.set(adaptive);
                self.start_tour(tour, hints);
            } else {
                conv.pending_tour.replace((tour, hints));
                (self.toast)(&crate::tr!("Claude hat eine Erklärung für {}. Esc startet sie.", "Claude has a walkthrough for {}. Press Esc to start it.", conv.name));
            }
        } else if !seen {
            conv.unread.set(true);
            (self.toast)(&crate::tr!("Claude ist fertig ({}). Esc zeigt die Antwort.", "Claude is done ({}). Press Esc to see the answer.", conv.name));
        }
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE Claude fertig: {} ({model}, Effort „{effort}“), Session {:?}, Kontext {}", conv.name, conv.session.borrow(), conv.context.get());
        }
        conv.last_run.set(Some((model, effort)));
        self.plan_compact(conv);
    }

    /// Von Claude abgelegte Änderungen in die Notiz der Anfrage übernehmen.
    async fn apply_changes(self: &Rc<Self>, conv: &Rc<Conv>, origin: &Origin, final_try: bool) {
        let Some(v) = origin.view.as_ref().and_then(Weak::upgrade) else { return };
        let cache = cache_dir(&conv.dir);
        let Some(out) = place::apply(&v, &cache, origin.page, &conv.last_batch, final_try).await else { return };
        if out.undone {
            conv.push(Kind::Tool, crate::i18n::tl("Einfügen rückgängig gemacht"));
        }
        match out.placed {
            0 => {}
            1 => conv.push(Kind::Tool, crate::i18n::tl("In die Notiz eingefügt")),
            n => conv.push(Kind::Tool, crate::tr!("{n} Einträge in die Notiz eingefügt", "{n} items inserted into the note")),
        }
        for e in out.errors {
            conv.push(Kind::Tool, e);
        }
        self.sync_if_bound(conv);
    }

    /// Großes Gespräch gleich zusammenfassen, mittleres erst nach einer Pause.
    fn plan_compact(self: &Rc<Self>, conv: Rc<Conv>) {
        let scale = std::env::var("GRAPHIT_COMPACT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let used = conv.context.get().saturating_mul(scale);
        if used >= COMPACT_AT {
            let p = self.clone();
            glib::spawn_future_local(async move { p.compact(conv).await });
        } else if used >= COMPACT_IDLE_AT {
            let (w, serial) = (Rc::downgrade(self), conv.serial.get());
            glib::timeout_add_local_once(COMPACT_IDLE, move || {
                if let Some(p) = w.upgrade().filter(|_| conv.serial.get() == serial) {
                    glib::spawn_future_local(async move { p.compact(conv).await });
                }
            });
        }
    }

    /// Claude fasst das Gespräch selbst zusammen, solange der Cache noch warm ist.
    /// Danach beginnt eine neue Session, die nur die Zusammenfassung kennt. Geht etwas
    /// schief, bleibt die alte Session einfach bestehen.
    async fn compact(self: &Rc<Self>, conv: Rc<Conv>) {
        let Some(session) = conv.session.borrow().clone() else { return };
        if conv.pid.get().is_some() {
            return;
        }
        let (model, effort) = conv.last_run.get().unwrap_or((self.model_alias(), self.effort_value()));
        let cache = cache_dir(&conv.dir);
        let _ = std::fs::create_dir_all(&cache);
        let Ok(started) = launch(&conv.dir, &cache, COMPACT_PROMPT, Some(&session), model, effort) else { return };
        conv.pid.set(Some(started.pid));
        conv.compacting.set(true);
        conv.status.replace(crate::i18n::tl("Graphit fasst das Gespräch zusammen …").into());
        self.sync_if_bound(&conv);
        let (mut summary, mut failed) = (String::new(), true);
        while let Ok(ev) = started.events.recv().await {
            match ev {
                Event::TextStart => summary.clear(),
                Event::Text(t) => summary.push_str(&t),
                Event::Finished { error } => {
                    failed = error.is_some();
                    break;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(cache.join(TOUR_FILE));
        conv.pid.set(None);
        conv.compacting.set(false);
        let stopped = conv.stopped.replace(false);
        if !failed && !stopped && !summary.trim().is_empty() {
            let before = conv.context.replace(0);
            conv.set_memory(Some(summary.trim().to_string()));
            conv.session.replace(None);
            conv.push(Kind::Tool, crate::tr!("Gespräch zusammengefasst, {}k Kontext freigegeben", "Conversation summarized, {}k of context freed", (before + 500) / 1000));
            conv.save();
        }
        self.sync_if_bound(&conv);
        if conv.send_after.replace(false) && self.conv.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, &conv)) {
            self.send();
        }
    }

    fn sync_if_bound(self: &Rc<Self>, conv: &Rc<Conv>) {
        if self.conv.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, conv)) {
            self.sync();
        }
    }

    /// Laufende Anfragen beenden (Fenster wird geschlossen).
    pub fn stop_all(&self) {
        self.hide_keyboard();
        for c in self.convs.borrow().values() {
            c.stop();
        }
    }

    // ---------------------------------------------------------------- Führung

    pub fn start_tour_file(self: &Rc<Self>, path: &Path) {
        let (steps, hints) = std::fs::read_to_string(path).map(|t| parse_tour(&t)).unwrap_or_default();
        self.start_tour(steps, hints);
    }

    fn start_tour(self: &Rc<Self>, steps: Vec<Step>, hints: Vec<String>) {
        if steps.is_empty() {
            return;
        }
        self.guide_offsets.replace(vec![None; steps.len()]);
        self.tour.replace(steps);
        self.tour_hints.replace(hints);
        self.guide_placed.set(false);
        self.guide.set_visible(true);
        self.show_step(0);
    }

    fn read_step(&self) {
        if let Some(s) = self.tour.borrow().get(self.tour_idx.get()) {
            match s.think.as_ref().filter(|_| !self.guide_revealed.get()) {
                Some(t) => self.speaker.say(&t.question),
                None => self.speaker.say(&index::marks(&s.text).clean),
            }
        }
    }

    /// Claude baut hinter dem aktuellen Schritt einen einfacheren Zwischenschritt ein.
    /// Die Karte bleibt offen, man kann währenddessen weiterlesen.
    fn simplify_step(self: &Rc<Self>, reason: &str) {
        let i = self.tour_idx.get();
        let Some(step) = self.tour.borrow().get(i).cloned() else { return };
        let Some(conv) = self.conv.borrow().clone() else { return };
        if conv.pid.get().is_some() || !self.tour_adaptive.get() {
            return;
        }
        let view = self.note();
        let page = step.page.unwrap_or_else(|| view.as_ref().map_or(0, |v| v.canvas.st().current_page()));
        let language = if crate::i18n::english() { "Englisch" } else { "Deutsch" };
        let cache = cache_dir(&conv.dir);
        let shown = step.think.as_ref().map_or(String::new(), |t| format!(" Denkfrage: {}", t.question));
        let quiz = conv.quiz_log.take().join("\n");
        let prompt = format!(
            "[Anpassung der laufenden Führung. {reason}. Schritt auf Seite {}: {}.{shown} {quiz}\nSchreibe {} neu, mit genau 1 bis 2 einfacheren Zwischenschritten auf derselben Seite, nach {}. Setze an der vermuteten Verständnislücke an, mit kleinem Zahlenbeispiel oder Bild, mindestens einer mit denken. Keine landkarte. Alle Texte auf {language}. Erkläre nichts im Chat.]",
            page + 1,
            step.text,
            cache.join(TOUR_FILE).display(),
            cache.join(LECTURE_GUIDE_FILE).display()
        );
        conv.insert_at.set(Some(i));
        conv.status.replace(crate::i18n::tl("Claude baut einen Zwischenschritt …").into());
        conv.pid.set(Some(0));
        self.guide_stuck.set_sensitive(false);
        self.guide_note.set_text(crate::i18n::tl("Claude baut einen einfacheren Zwischenschritt …"));
        self.guide_note.set_visible(true);
        let origin = Origin { view: view.as_ref().map(Rc::downgrade), page };
        let (model, effort) = (self.model_alias(), self.effort_value());
        let p = self.clone();
        glib::spawn_future_local(async move { p.run(conv, prompt, origin, model, effort, false).await });
    }

    /// Fügt Zwischenschritte hinter `after` ein, ohne die aktuelle Karte zu wechseln.
    fn insert_steps(&self, after: usize, steps: Vec<Step>) {
        self.guide_stuck.set_sensitive(true);
        if steps.is_empty() || self.tour.borrow().is_empty() {
            self.guide_note.set_visible(false);
            return;
        }
        let at = (after + 1).min(self.tour.borrow().len());
        let added = steps.len();
        self.tour.borrow_mut().splice(at..at, steps);
        self.guide_offsets.borrow_mut().splice(at..at, vec![None; added]);
        if self.tour_idx.get() >= at {
            self.tour_idx.set(self.tour_idx.get() + added);
        }
        self.guide_note.set_text(crate::i18n::tl("Einfacherer Zwischenschritt ist bereit: Weiter"));
        self.guide_note.set_visible(true);
        self.refresh_progress();
    }

    /// Zähler, Fortschrittsbalken und Beschriftung des Weiter-Knopfs.
    fn refresh_progress(&self) {
        let (i, n) = (self.tour_idx.get(), self.tour.borrow().len());
        self.guide_count.set_text(&format!("{} / {n}", i + 1));
        while let Some(c) = self.guide_steps.first_child() {
            self.guide_steps.remove(&c);
        }
        for k in 0..n {
            let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            seg.set_hexpand(true);
            seg.add_css_class("fk-claude-seg");
            if k <= i {
                seg.add_css_class("fk-claude-seg-on");
            }
            self.guide_steps.append(&seg);
        }
        self.guide_steps.set_visible(n > 1);
        self.guide_prev.set_sensitive(i > 0);
        self.guide_next.set_label(if i + 1 == n { crate::i18n::tl("Fertig") } else { crate::i18n::tl("Weiter") });
    }

    fn end_tour(&self) {
        self.speaker.stop();
        self.tour_next.set(None);
        if let Some(v) = self.tour_view.take() {
            v.canvas.clear_guide();
        }
        self.tour.borrow_mut().clear();
        self.tour_adaptive.set(false);
        self.guide_note.set_visible(false);
        self.guide.set_visible(false);
    }

    fn step(self: &Rc<Self>, d: i32) {
        let n = self.tour.borrow().len() as i32;
        let i = self.tour_idx.get() as i32 + d;
        if i >= n {
            self.end_tour();
        } else if i >= 0 {
            self.show_step(i as usize);
        }
    }

    /// Knöpfe für die nächste Frage unter der Antwort.
    fn set_chips(self: &Rc<Self>, hints: &[String]) {
        self.chips.remove_all();
        for h in hints {
            self.chips.append(&self.hint_button(h));
        }
        self.chips.set_visible(!hints.is_empty());
    }

    fn hint_button(self: &Rc<Self>, hint: &str) -> gtk::Button {
        self.ask_button(hint, hint)
    }

    fn ask_button(self: &Rc<Self>, caption: &str, question: &str) -> gtk::Button {
        let label = gtk::Label::new(Some(caption));
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        let b = gtk::Button::new();
        b.set_child(Some(&label));
        b.add_css_class("fk-claude-hint");
        let (w, text) = (Rc::downgrade(self), question.to_string());
        b.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.ask(&text);
            }
        });
        b
    }

    /// Text in der Lernkarte, der Formeln enthalten darf.
    fn extra_text(&self, text: &str) -> Rich {
        let r = Rich::new(330.0);
        r.set(text, &self.engine);
        self.extra_rich.borrow_mut().push(r.clone());
        r
    }

    fn option_button(&self, k: usize, opt: &str) -> gtk::Button {
        let text = self.extra_text(opt);
        let letter = gtk::Label::new(Some(&char::from(b'A' + (k as u8).min(25)).to_string()));
        letter.add_css_class("fk-claude-letter");
        letter.set_valign(gtk::Align::Start);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&letter);
        row.append(&text.root);
        // Klicks gehen an den Knopf, nicht an das Textfeld darin
        row.set_can_target(false);
        let b = gtk::Button::new();
        b.set_child(Some(&row));
        b.add_css_class("fk-claude-option");
        b
    }

    /// Denkschritt: Die Frage steht zuerst da, alles Weitere erscheint nach der Antwort.
    fn add_think(self: &Rc<Self>, step: &Step, think: &Think) {
        let mut hidden = Vec::new();
        let mut child = self.guide_extra.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            c.set_visible(false);
            hidden.push(c);
        }
        let verdict = self.extra_text("");
        verdict.root.set_visible(false);
        let text = index::marks(&step.text).clean;
        let reveal: Rc<dyn Fn(&Rc<ClaudePanel>, Option<bool>)> = {
            let (hidden, verdict, text) = (hidden, verdict.clone(), text);
            Rc::new(move |p, correct| {
                p.guide_revealed.set(true);
                p.guide_text.set(&text, &p.engine);
                for c in &hidden {
                    c.set_visible(true);
                }
                if let Some(ok) = correct {
                    verdict.set(if ok { crate::i18n::tl("**Richtig.**") } else { crate::i18n::tl("**Nicht ganz.**") }, &p.engine);
                    verdict.root.set_visible(true);
                }
                if p.guide_speak.is_active() {
                    p.read_step();
                }
            })
        };
        if think.options.is_empty() {
            let b = gtk::Button::with_label(crate::i18n::tl("Antwort zeigen"));
            b.add_css_class("fk-claude-next");
            let w = Rc::downgrade(self);
            let show = reveal.clone();
            b.connect_clicked(move |b| {
                if let Some(p) = w.upgrade() {
                    b.set_visible(false);
                    show(&p, None);
                }
            });
            self.guide_extra.prepend(&b);
            return;
        }
        let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::default();
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for (k, opt) in think.options.iter().enumerate() {
            let b = self.option_button(k, opt);
            let (w, all, right, show, question, options) = (Rc::downgrade(self), buttons.clone(), think.right, reveal.clone(), think.question.clone(), think.options.clone());
            b.connect_clicked(move |clicked| {
                let Some(p) = w.upgrade() else { return };
                for (i, o) in all.borrow().iter().enumerate() {
                    o.set_sensitive(false);
                    if Some(i) == right {
                        o.add_css_class("fk-claude-right");
                    }
                }
                let correct = right.map(|r| r == k);
                if correct == Some(false) {
                    clicked.add_css_class("fk-claude-wrong");
                    if let Some(c) = p.conv.borrow().as_ref() {
                        c.quiz_log.borrow_mut().push(format!("- Zu „{question}“ gewählt: „{}“ (falsch)", options.get(k).cloned().unwrap_or_default()));
                    }
                }
                show(&p, correct);
                if correct == Some(false) {
                    p.simplify_step("Der Nutzer lag bei der Denkfrage falsch");
                }
            });
            buttons.borrow_mut().push(b.clone());
            box_.append(&b);
        }
        box_.append(&verdict.root);
        self.guide_extra.prepend(&box_);
    }

    /// Landkarte: Antippen einer Idee springt zu ihrer Stelle und zeigt ihren Satz.
    fn add_map(self: &Rc<Self>, map: &IdeaMap, view: &Rc<DocView>) {
        const ROW_GAP: i32 = 26;
        let rows = gtk::Box::new(gtk::Orientation::Vertical, ROW_GAP);
        let detail = self.extra_text("");
        detail.root.set_visible(false);
        let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::default();
        let mut placed = vec![None; map.ideas.len()];
        for row in idea_rows(map) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_homogeneous(true);
            for i in row {
                let idea = &map.ideas[i];
                let label = gtk::Label::new(Some(&strip_math(&idea.title)));
                label.set_wrap(true);
                label.set_justify(gtk::Justification::Center);
                let b = gtk::Button::new();
                b.set_child(Some(&label));
                b.add_css_class("fk-claude-idea");
                let (w, v, all, shown, data) = (Rc::downgrade(self), view.clone(), buttons.clone(), detail.clone(), idea.clone());
                b.connect_clicked(move |clicked| {
                    let Some(p) = w.upgrade() else { return };
                    for o in all.borrow().iter() {
                        o.remove_css_class("fk-claude-idea-on");
                    }
                    clicked.add_css_class("fk-claude-idea-on");
                    shown.set(&data.text, &p.engine);
                    shown.root.set_visible(!data.text.is_empty());
                    p.show_idea(&data, &v);
                });
                buttons.borrow_mut().push(b.clone());
                placed[i] = Some(b.clone());
                line.append(&b);
            }
            rows.append(&line);
        }
        let area = gtk::DrawingArea::new();
        area.set_can_target(false);
        let (links, nodes) = (map.links.clone(), placed);
        let anchor = area.clone();
        area.set_draw_func(move |_, cr, _, _| {
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
            cr.set_font_size(9.5);
            cr.set_line_width(1.2);
            for (a, b, name) in &links {
                let (Some(Some(from)), Some(Some(to))) = (nodes.get(*a), nodes.get(*b)) else { continue };
                let at = |w: &gtk::Button, fx: f32, fy: f32| w.compute_point(&anchor, &gtk::graphene::Point::new(w.width() as f32 * fx, w.height() as f32 * fy));
                let (Some(p0), Some(p1)) = (at(from, 0.5, 1.0), at(to, 0.5, 0.0)) else { continue };
                let (x0, y0, x1, y1) = (p0.x() as f64, p0.y() as f64, p1.x() as f64, p1.y() as f64);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.42);
                cr.move_to(x0, y0);
                let mid = (y0 + y1) / 2.0;
                cr.curve_to(x0, mid, x1, mid, x1, y1 - 1.0);
                let _ = cr.stroke();
                cr.move_to(x1 - 3.5, y1 - 6.0);
                cr.line_to(x1, y1 - 1.0);
                cr.line_to(x1 + 3.5, y1 - 6.0);
                let _ = cr.stroke();
                if !name.is_empty() {
                    if let Ok(e) = cr.text_extents(name) {
                        // Kartenfarbe unter der Schrift, sonst läuft die Linie hindurch
                        cr.set_source_rgb(0.122, 0.122, 0.137);
                        cr.rectangle((x0 + x1) / 2.0 - e.width() / 2.0 - 3.0, mid - 7.0, e.width() + 6.0, 13.0);
                        let _ = cr.fill();
                        cr.set_source_rgba(1.0, 1.0, 1.0, 0.7);
                        cr.move_to((x0 + x1) / 2.0 - e.width() / 2.0, mid + 3.0);
                        let _ = cr.show_text(name);
                    }
                }
            }
        });
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&rows));
        overlay.add_overlay(&area);
        self.guide_extra.prepend(&detail.root);
        self.guide_extra.prepend(&overlay);
    }

    /// Springt zur Stelle einer Idee und rahmt sie ein.
    fn show_idea(&self, idea: &Idea, view: &Rc<DocView>) {
        let Some(spec) = &idea.mark else { return };
        let c = &view.canvas;
        let hint = idea.page.unwrap_or_else(|| c.st().current_page()).min(c.page_count().saturating_sub(1));
        let (page, rects) = match (spec.find.as_ref().and_then(|q| c.find_text(q, hint)), spec.area) {
            (Some(hit), _) => hit,
            (None, Some(a)) => {
                let Some((w, h)) = c.page_size(hint) else { return };
                (hint, vec![Rect::new(a[0] * w, a[1] * h, a[2] * w, a[3] * h)])
            }
            _ => return,
        };
        c.show_guide(page, vec![Mark { rects, kind: spec.kind, label: None, color: None }], Vec::new(), Vec::new());
        self.guide_spot.set(None);
        self.guide_anchor.set(c.guide_page_area());
    }

    /// Antwortmöglichkeiten: nach dem Tippen zeigt die Karte Richtig/Falsch und warum.
    fn add_quiz(self: &Rc<Self>, quiz: &Quiz, question: &str) {
        let answer = self.extra_text("");
        answer.root.set_visible(false);
        let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::default();
        for (k, opt) in quiz.options.iter().enumerate() {
            let b = self.option_button(k, opt);
            let (w, all, right, why, out) = (Rc::downgrade(self), buttons.clone(), quiz.right, quiz.why.clone(), answer.clone());
            let (asked, options) = (question.to_string(), quiz.options.clone());
            b.connect_clicked(move |clicked| {
                let Some(p) = w.upgrade() else { return };
                for (i, o) in all.borrow().iter().enumerate() {
                    o.set_sensitive(false);
                    if i == right {
                        o.add_css_class("fk-claude-right");
                    }
                }
                if k != right {
                    clicked.add_css_class("fk-claude-wrong");
                }
                let verdict = if k == right { crate::i18n::tl("**Richtig.**") } else { crate::i18n::tl("**Nicht ganz.**") };
                out.set(&format!("{verdict} {}", why.clone().unwrap_or_default()), &p.engine);
                out.root.set_visible(true);
                let chosen = options.get(k).cloned().unwrap_or_default();
                let correct = options.get(right).cloned().unwrap_or_default();
                let conv = p.conv.borrow().clone();
                if let Some(c) = conv {
                    let verdict = if k == right { "richtig" } else { "falsch" };
                    c.quiz_log.borrow_mut().push(format!("- Zu „{asked}“ gewählt: „{chosen}“ ({verdict}), richtig ist „{correct}“"));
                }
                if k != right && p.tour_adaptive.get() {
                    p.simplify_step("Der Nutzer hat im Quiz falsch geantwortet");
                } else if k != right {
                    let more = p.ask_button(crate::i18n::tl("Erklär mir das"), &crate::tr!("Ich habe im Quiz „{chosen}“ gewählt, richtig ist „{correct}“. Wo liegt mein Denkfehler?", "I chose “{chosen}” in the quiz, but “{correct}” is correct. Where did my reasoning go wrong?"));
                    p.guide_extra.insert_child_after(&more, Some(&out.root));
                }
            });
            buttons.borrow_mut().push(b.clone());
            self.guide_extra.append(&b);
        }
        self.guide_extra.append(&answer.root);
    }

    /// Knöpfe zu den Termen: Antippen hebt den Term auf der Seite hervor und erklärt ihn.
    fn add_terms(self: &Rc<Self>, step: &Step, marks: &[Option<usize>], view: &Rc<DocView>) {
        if step.terms.is_empty() {
            return;
        }
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let answer = self.extra_text("");
        answer.root.set_visible(false);
        let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::default();
        for (k, t) in step.terms.iter().enumerate() {
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("fk-term-dot");
            dot.add_css_class(&format!("fk-term-{}", k % TERM_COLORS.len()));
            dot.set_valign(gtk::Align::Center);
            let name = self.extra_text(&t.name);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.append(&dot);
            row.append(&name.root);
            row.set_can_target(false);
            let b = gtk::Button::new();
            b.set_child(Some(&row));
            b.add_css_class("fk-claude-term");
            let (w, v, out, why, mark, all) = (Rc::downgrade(self), view.clone(), answer.clone(), t.why.clone(), marks.get(k).copied().flatten(), buttons.clone());
            b.connect_clicked(move |b| {
                let Some(p) = w.upgrade() else { return };
                for o in all.borrow().iter() {
                    o.remove_css_class("fk-claude-term-on");
                }
                b.add_css_class("fk-claude-term-on");
                v.canvas.emphasize_mark(mark);
                out.set(why.as_deref().unwrap_or(""), &p.engine);
                out.root.set_visible(why.is_some());
            });
            buttons.borrow_mut().push(b.clone());
            list.append(&b);
        }
        self.guide_extra.prepend(&answer.root);
        self.guide_extra.prepend(&list);
    }

    /// Diagramm mit Reglern; Ausgangskurve bleibt gestrichelt zum Vergleich.
    fn add_plot(self: &Rc<Self>, p: &Plot, view: &Rc<DocView>) {
        let start = plot::initial_values(p);
        let now = Rc::new(RefCell::new(start.clone()));
        let area = gtk::DrawingArea::new();
        area.set_content_height(190);
        area.set_hexpand(true);
        area.add_css_class("fk-claude-plot");
        let (data, begin, values) = (p.clone(), start.clone(), now.clone());
        area.set_draw_func(move |_, cr, w, h| plot::draw(cr, w as f64, h as f64, &data, &begin, &values.borrow(), plot::DARK));
        self.guide_extra.append(&area);
        let mut scales: Vec<(String, gtk::Scale)> = Vec::new();
        for s in &p.sliders {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.add_css_class("fk-claude-slider");
            let text = |v: f64, name: &str, unit: &str| format!("{name}: {} {unit}", plot::fmt(v)).trim().to_string();
            label.set_text(&text(s.value, &s.name, &s.unit));
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, s.min, s.max, (s.max - s.min) / 200.0);
            scale.set_value(s.value);
            scale.set_draw_value(false);
            let (values, a, l, sym, name, unit) = (now.clone(), area.clone(), label.clone(), s.symbol.clone(), s.name.clone(), s.unit.clone());
            scale.connect_value_changed(move |r| {
                values.borrow_mut().insert(sym.clone(), r.value());
                l.set_text(&text(r.value(), &name, &unit));
                a.queue_draw();
            });
            self.guide_extra.append(&label);
            self.guide_extra.append(&scale);
            scales.push((s.symbol.clone(), scale));
        }
        self.add_plot_controls(p, &area, &now, &scales);
        let take = gtk::Button::with_label(crate::i18n::tl("In Notiz übernehmen"));
        take.add_css_class("flat");
        let (data, begin, values, v) = (p.clone(), start, now, view.clone());
        take.connect_clicked(move |b| {
            if let Some(png) = plot::png(&data, &begin, &values.borrow()) {
                let (page, pos) = v.canvas.paste_target();
                if v.canvas.insert_image_scaled(page, pos, &png, 1.0 / 3.0).is_ok() {
                    b.set_visible(false);
                }
            }
        });
        self.guide_extra.append(&take);
    }

    /// Schritt- und Abspielknöpfe für Folgen und durchfahrbare Regler.
    fn add_plot_controls(&self, p: &Plot, area: &gtk::DrawingArea, now: &Rc<RefCell<HashMap<String, f64>>>, scales: &[(String, gtk::Scale)]) {
        const SWEEP_TICK_MS: u64 = 33;
        const SWEEP_FRAMES: u32 = 110;
        const STEP_TICK_MS: u64 = 420;
        let sweep = p.animate.as_ref().and_then(|a| scales.iter().find(|s| &s.0 == a)).map(|s| s.1.clone());
        if p.sequence.is_none() && sweep.is_none() {
            return;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("flat");
        play.set_tooltip_text(Some(crate::i18n::tl("Abspielen")));
        row.append(&play);
        let running = Rc::new(Cell::new(false));
        if let Some(q) = p.sequence.clone() {
            let count = gtk::Label::new(None);
            count.add_css_class("fk-claude-slider");
            count.set_hexpand(true);
            count.set_xalign(0.0);
            let show = {
                let (count, now, q, area) = (count.clone(), now.clone(), q.clone(), area.clone());
                Rc::new(move || {
                    let values = now.borrow();
                    let k = values.get(plot::STEP).copied().unwrap_or(0.0) as usize;
                    let n = plot::sequence_len(&q, &values);
                    let k = k.min(n);
                    count.set_text(&crate::tr!("Schritt {k} von {n}", "Step {k} of {n}"));
                    area.queue_draw();
                })
            };
            show();
            let forward = gtk::Button::from_icon_name("media-skip-forward-symbolic");
            forward.add_css_class("flat");
            forward.set_tooltip_text(Some(crate::i18n::tl("Ein Schritt")));
            let back = gtk::Button::from_icon_name("media-skip-backward-symbolic");
            back.add_css_class("flat");
            back.set_tooltip_text(Some(crate::i18n::tl("Zurücksetzen")));
            let advance = {
                let (now, q, show) = (now.clone(), q.clone(), show.clone());
                Rc::new(move || -> bool {
                    let more = {
                        let mut values = now.borrow_mut();
                        let n = plot::sequence_len(&q, &values) as f64;
                        let k = (values.get(plot::STEP).copied().unwrap_or(0.0) + 1.0).min(n);
                        values.insert(plot::STEP.to_string(), k);
                        k < n
                    };
                    show();
                    more
                })
            };
            {
                let a = advance.clone();
                forward.connect_clicked(move |_| {
                    a();
                });
            }
            {
                let (now, show) = (now.clone(), show.clone());
                back.connect_clicked(move |_| {
                    now.borrow_mut().insert(plot::STEP.to_string(), 0.0);
                    show();
                });
            }
            for (_, scale) in scales {
                let show = show.clone();
                scale.connect_value_changed(move |_| show());
            }
            row.append(&back);
            row.append(&forward);
            row.append(&count);
            let (area, run, now_for_play) = (area.clone(), running.clone(), now.clone());
            play.connect_clicked(move |_| {
                if run.replace(true) {
                    run.set(false);
                    return;
                }
                now_for_play.borrow_mut().insert(plot::STEP.to_string(), 0.0);
                let (a, run, widget) = (advance.clone(), run.clone(), area.clone());
                glib::timeout_add_local(Duration::from_millis(STEP_TICK_MS), move || {
                    // die Karte kann währenddessen weitergeblättert sein
                    if !run.get() || widget.root().is_none() || !a() {
                        run.set(false);
                        return glib::ControlFlow::Break;
                    }
                    glib::ControlFlow::Continue
                });
            });
        } else if let Some(scale) = sweep {
            let run = running.clone();
            play.connect_clicked(move |_| {
                if run.replace(true) {
                    run.set(false);
                    return;
                }
                let adj = scale.adjustment();
                let (lo, hi, back_to) = (adj.lower(), adj.upper(), adj.value());
                let frame = Cell::new(0u32);
                let (run, scale) = (run.clone(), scale.clone());
                glib::timeout_add_local(Duration::from_millis(SWEEP_TICK_MS), move || {
                    frame.set(frame.get() + 1);
                    if !run.get() || scale.root().is_none() || frame.get() > SWEEP_FRAMES {
                        if scale.root().is_some() {
                            scale.set_value(back_to);
                        }
                        run.set(false);
                        return glib::ControlFlow::Break;
                    }
                    scale.set_value(lo + (hi - lo) * frame.get() as f64 / SWEEP_FRAMES as f64);
                    glib::ControlFlow::Continue
                });
            });
        }
        self.guide_extra.append(&row);
    }

    fn show_extras(self: &Rc<Self>, step: &Step, last: bool, view: &Rc<DocView>) {
        while let Some(c) = self.guide_extra.first_child() {
            self.guide_extra.remove(&c);
        }
        self.extra_rich.borrow_mut().clear();
        let conv = self.conv.borrow().clone();
        if let Some(c) = conv {
            let found = index::marks(&step.text).sources;
            if !found.is_empty() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                self.source_buttons(&c, &found, &row);
                self.guide_extra.append(&row);
            }
        }
        if let Some(d) = &step.plot {
            self.add_plot(d, view);
        }
        if let Some(q) = &step.quiz {
            self.add_quiz(q, &index::marks(&step.text).clean);
        }
        if let Some(tip) = &step.tip {
            let shown = self.extra_text(tip);
            shown.root.set_visible(false);
            let b = gtk::Button::with_label(crate::i18n::tl("Tipp zeigen"));
            b.add_css_class("flat");
            let root = shown.root.clone();
            b.connect_clicked(move |b| {
                root.set_visible(true);
                b.set_visible(false);
            });
            self.guide_extra.append(&b);
            self.guide_extra.append(&shown.root);
        }
        if let Some(task) = &step.task {
            let b = gtk::Button::with_label(crate::i18n::tl("Prüfen"));
            b.add_css_class("fk-claude-next");
            b.set_tooltip_text(Some(crate::i18n::tl("Rechne auf der Seite, dann sieht Claude sich deine Lösung an")));
            let (w, t) = (Rc::downgrade(self), task.clone());
            b.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.ask(&crate::tr!("Ich habe die Aufgabe bearbeitet: {t}\nPrüfe meine Lösung auf der aktuellen Seite und gib mir Rückmeldung.", "I worked on the exercise: {t}\nCheck my solution on the current page and give me feedback."));
                }
            });
            self.guide_extra.append(&self.extra_text(&crate::tr!("**Du bist dran:** {task}", "**Your turn:** {task}")).root);
            self.guide_extra.append(&b);
        }
        if !step.sketch.is_empty() {
            let b = gtk::Button::with_label(crate::i18n::tl("Skizze in die Notiz übernehmen"));
            b.add_css_class("flat");
            let v = view.clone();
            b.connect_clicked(move |b| {
                if v.canvas.take_sketch() {
                    b.set_visible(false);
                }
            });
            self.guide_extra.append(&b);
        }
        if last {
            if let Some(next) = self.tour_next.get() {
                let b = gtk::Button::with_label(&crate::tr!("Nächster Abschnitt ab Seite {next}", "Next section from page {next}"));
                b.add_css_class("fk-claude-hint");
                let w = Rc::downgrade(self);
                b.connect_clicked(move |_| {
                    if let Some(p) = w.upgrade() {
                        let kind = p.last_lecture_kind();
                        p.start_lecture(next, kind);
                    }
                });
                self.guide_extra.append(&b);
            }
            for h in self.tour_hints.borrow().iter() {
                self.guide_extra.append(&self.hint_button(h));
            }
        }
    }

    fn show_step(self: &Rc<Self>, i: usize) {
        let Some(step) = self.tour.borrow().get(i).cloned() else { return };
        self.tour_idx.set(i);
        self.guide_revealed.set(step.think.is_none());
        self.guide_note.set_visible(false);
        self.guide_stuck.set_visible(self.tour_adaptive.get());
        self.guide_kind.set_text(match (&step.map, &step.think, &step.quiz, &step.task) {
            (Some(_), ..) => crate::i18n::tl("Überblick"),
            (_, Some(_), ..) => crate::i18n::tl("Erst denken"),
            (_, _, Some(_), _) => "Quiz",
            (_, _, _, Some(_)) => crate::i18n::tl("Du bist dran"),
            _ => crate::i18n::tl("Erklärung"),
        });
        self.refresh_progress();
        // zwei gleiche Animationen im Wechsel, sonst läuft sie nur beim ersten Schritt
        let (on, off) = if i % 2 == 0 { ("fk-claude-step-a", "fk-claude-step-b") } else { ("fk-claude-step-b", "fk-claude-step-a") };
        self.guide.remove_css_class(off);
        self.guide.add_css_class(on);
        match &step.think {
            Some(t) => self.guide_text.set(&t.question, &self.engine),
            None => self.guide_text.set(&index::marks(&step.text).clean, &self.engine),
        }
        let previous = self.tour_view.take();
        if let Some(old) = &previous {
            old.canvas.clear_guide();
        }
        let Some(view) = (self.view_for)(&step.target).or_else(|| self.note()) else { return };
        let same_view = previous.is_some_and(|old| Rc::ptr_eq(&old, &view));
        if !same_view {
            self.guide_ink.borrow_mut().clear();
        }
        let c = &view.canvas;
        let hint = step.page.unwrap_or_else(|| c.st().current_page()).min(c.page_count().saturating_sub(1));
        let mut page = None;
        // Bereiche und Suchtexte auf die Seite auflösen; nicht gefundene Zeichen fallen weg
        let mut marks = Vec::new();
        let mut index = vec![None; step.marks.len()];
        for (k, m) in step.marks.iter().enumerate() {
            let rects = match (m.find.as_ref().and_then(|q| c.find_text(q, hint)), m.area) {
                (Some((p, r)), _) if *page.get_or_insert(p) == p => r,
                (_, Some(a)) => {
                    let p = *page.get_or_insert(hint);
                    let Some((w, h)) = c.page_size(p) else { continue };
                    let area = Rect::new(a[0] * w, a[1] * h, a[2] * w, a[3] * h);
                    vec![snap_to_ink(area, &ink_lines(c.page_ink(p), w), w)]
                }
                _ => continue,
            };
            index[k] = Some(marks.len());
            marks.push(Mark { rects, kind: m.kind, label: m.label.clone(), color: None });
        }
        let page = page.unwrap_or(hint);
        // Terme: grober Bereich reicht, er wird am Inhalt der Seite eingerastet
        let mut term_marks = vec![None; step.terms.len()];
        if let Some((w, h)) = c.page_size(page) {
            let mut found: Vec<(usize, Rect)> = Vec::new();
            let mut rough: Vec<(usize, Rect)> = Vec::new();
            for (k, t) in step.terms.iter().enumerate() {
                match (t.find.as_ref().and_then(|q| c.find_on_page(q, page)), t.area) {
                    (Some(r), _) if !r.is_empty() => found.push((k, r.iter().skip(1).fold(r[0], |a, b| a.union(b)))),
                    (_, Some(a)) => rough.push((k, Rect::new(a[0] * w, a[1] * h, a[2] * w, a[3] * h))),
                    _ => {}
                }
            }
            let snapped = c.snap_terms(page, &rough.iter().map(|r| r.1).collect::<Vec<_>>());
            found.extend(rough.iter().map(|r| r.0).zip(snapped));
            found.sort_by_key(|f| f.0);
            for (k, r) in found {
                let (cr, cg, cb) = TERM_COLORS[k % TERM_COLORS.len()];
                term_marks[k] = Some(marks.len());
                marks.push(Mark { rects: vec![r], kind: MarkKind::Term, label: Some(strip_math(&step.terms[k].name)), color: Some(gdk::RGBA::new(cr, cg, cb, 1.0)) });
            }
        }
        let links = step.links.iter().filter_map(|(a, b)| Some(((*index.get(*a)?)?, (*index.get(*b)?)?))).collect();
        let (w, h) = c.page_size(page).unwrap_or((1.0, 1.0));
        let sketch = step.sketch.iter().map(|l| l.iter().map(|q| Pt::new(q[0] * w, q[1] * h)).collect()).collect();
        c.show_guide(page, marks, links, sketch);
        self.show_extras(&step, i + 1 == self.tour.borrow().len(), &view);
        self.add_terms(&step, &term_marks, &view);
        if let Some(t) = &step.think {
            self.add_think(&step, t);
        }
        if let Some(m) = &step.map {
            self.add_map(m, &view);
        }
        if self.guide_speak.is_active() {
            self.read_step();
        }
        if std::env::var("GRAPHIT_PROFILE").is_ok() {
            eprintln!("PROFILE Schritt {} ({}): Zeichen bei {:?}", i + 1, step.target, view.canvas.guide_screen_rect());
        }
        // Karte ohne eigene Zeichen bleibt, wo die vorige stand, solange es dieselbe Seite ist
        let own = view.canvas.guide_page_area();
        let stays = own.is_none() && same_view && step.page.is_none_or(|_| self.guide_anchor.get().is_some_and(|a| a.0 == page));
        if !stays {
            self.guide_spot.set(None);
            self.guide_anchor.set(own);
        }
        self.tour_view.replace(Some(view));
    }

    /// Lernkarte neben ihr Zeichen führen; läuft jedes Bild, damit sie beim Scrollen mitgeht.
    fn place_guide(&self) {
        if self.tour_view.borrow().is_none() {
            return;
        }
        let mark = self.guide_mark();
        let (card, view, bottom) = (self.guide_size(), self.guide_view(), DOCK_SPACE as f64);
        let (x, y) = match (self.step_offset(), mark) {
            (Some((ox, oy)), _) => {
                let (ax, ay) = mark.map_or((0.0, 0.0), |r| (r.x0, r.y0));
                self.clamp_card(ax + ox, ay + oy)
            }
            (None, Some(r)) => {
                let (ox, oy) = self.card_spot(r);
                fit_card((r.x0 + ox, r.y0 + oy), card, view, bottom)
            }
            (None, None) => free_card_position(card, view, bottom),
        };
        let (cx, cy) = (self.guide.margin_start() as f64, self.guide.margin_top() as f64);
        let dragging = self.guide_drag.get().is_some();
        let glide = if self.guide_placed.replace(true) && !dragging { 0.22 } else { 1.0 };
        let (nx, ny) = (cx + (x - cx) * glide, cy + (y - cy) * glide);
        if (nx - cx).abs() >= 1.0 || (ny - cy).abs() >= 1.0 || glide == 1.0 {
            self.guide.set_margin_start(nx.round() as i32);
            self.guide.set_margin_top(ny.round() as i32);
        }
    }
}

/// Lage eines Zeigerereignisses in einem Widget.
fn widget_point(widget: &impl IsA<gtk::Widget>, ev: &gdk::Event) -> Option<(f64, f64)> {
    let (x, y) = ev.position()?;
    let native = widget.as_ref().native()?;
    let (sx, sy) = native.surface_transform();
    let p = native.compute_point(widget, &gtk::graphene::Point::new((x - sx) as f32, (y - sy) as f32))?;
    Some((p.x() as f64, p.y() as f64))
}

/// Lage des letzten Ereignisses einer Geste, unabhängig davon, ob ihr Widget gerade mitwandert.
fn gesture_point(g: &gtk::GestureDrag, widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64)> {
    let sequence = g.last_updated_sequence();
    widget_point(widget, &g.last_event(sequence.as_ref())?)
}

/// Handschrift des Schreibfelds zeichnen, in der aktuellen Farbe des Kontexts.
fn draw_strokes(cr: &cairo::Context, strokes: &[Vec<(f64, f64)>], width: f64) {
    cr.set_line_width(width);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    for s in strokes {
        let Some(first) = s.first() else { continue };
        cr.move_to(first.0, first.1);
        // weiche Kurve durch die Mittelpunkte, ein einzelner Tipp bleibt ein Punkt
        for w in s.windows(2) {
            let mid = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
            cr.curve_to(w[0].0, w[0].1, w[0].0, w[0].1, mid.0, mid.1);
        }
        if let Some(last) = s.last() {
            cr.line_to(last.0 + 0.01, last.1);
        }
        let _ = cr.stroke();
    }
}

/// Schreibzeilen aller Seiten als Anteile der Seite.
fn ink_json(pages: &[Arc<crate::model::PageData>]) -> String {
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    let list: Vec<Value> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let boxes = p.layers.iter().flat_map(|l| l.iter()).filter(|it| matches!(&***it, crate::model::Item::Stroke(_))).filter_map(|it| it.bbox()).collect();
            let lines = ink_lines(boxes, p.width);
            (!lines.is_empty()).then(|| {
                let rows: Vec<Value> = lines.iter().map(|r| json!([round(r.x0 / p.width), round(r.y0 / p.height), round(r.x1 / p.width), round(r.y1 / p.height)])).collect();
                json!({"seite": i + 1, "zeilen": rows})
            })
        })
        .collect();
    json!({"seiten": list}).to_string()
}

/// Text in die Notiz setzen; Formeln gehen als gesetztes Bild, sonst bliebe LaTeX-Code stehen.
fn insert_text(view: Rc<DocView>, page: Option<usize>, text: String) {
    if !latex::needs_typesetting(&text) {
        view.canvas.paste_text(&text);
        return;
    }
    glib::spawn_future_local(async move {
        match latex::note_image(text.clone()).await {
            Some(png) => {
                let (current, pos) = view.canvas.paste_target();
                let _ = view.canvas.insert_image_scaled(page.unwrap_or(current), pos, &png, 1.0 / latex::NOTE_RASTER);
            }
            None => view.canvas.paste_text(&text),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_of_note() {
        let root = Path::new("/d/X");
        let m = |p: &str| module_dir(Some(Path::new(p)), root);
        assert_eq!(m("/d/X/3rd_Semester/TD II/Tabellen/a.xopp"), Path::new("/d/X/3rd_Semester/TD II"));
        assert_eq!(m("/d/X/3rd_Semester/TD II/a.xopp"), Path::new("/d/X/3rd_Semester/TD II"));
        assert_eq!(m("/d/X/3rd_Semester/a.xopp"), Path::new("/d/X/3rd_Semester"));
        assert_eq!(m("/d/X/personal/a.xopp"), Path::new("/d/X/personal"));
        assert_eq!(m("/d/X/Old_exams/2024/a.xopp"), Path::new("/d/X/Old_exams"));
        assert_eq!(m("/d/X/a.xopp"), root);
        assert_eq!(m("/tmp/y/a.pdf"), Path::new("/tmp/y"));
        assert_eq!(module_dir(None, root), root);
    }

    #[test]
    fn stream_lines() {
        assert_eq!(parse_line(r#"{"type":"system","subtype":"init","session_id":"abc"}"#), [Event::Session("abc".into())]);
        let delta = r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#;
        assert_eq!(parse_line(delta), [Event::Text("Hi".into())]);
        let nested = r#"{"type":"stream_event","parent_tool_use_id":"t1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}"#;
        assert!(parse_line(nested).is_empty());
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/a/b.md"}}]}}"#;
        assert_eq!(parse_line(tool), [Event::Tool { text: "Schreibt b.md".into(), lasting: true }]);
        let (steps, _) = parse_tour(r#"{"schritte":[{"text":"T","denken":{"frage":"F?","optionen":["a","b"],"richtig":1},
            "landkarte":{"ideen":[{"id":"a","titel":"A","suche":"x"},{"id":"b","titel":"B"},{"id":"c","titel":"C"}],"verbindungen":[["a","b","liefert"],["b","c",""],["c","a","Kreis"]]}}]}"#);
        assert_eq!(steps[0].think.as_ref().unwrap().right, Some(1));
        let map = steps[0].map.as_ref().unwrap();
        assert_eq!(map.links.len(), 3);
        assert_eq!(idea_rows(map).concat().len(), 3);
        let bad = r#"{"type":"result","is_error":true,"result":"kaputt"}"#;
        assert_eq!(parse_line(bad), [Event::Finished { error: Some("kaputt".into()) }]);
        assert_eq!(parse_line(r#"{"type":"result","is_error":false,"result":"ok"}"#), [Event::Finished { error: None }]);
        assert!(parse_line("kein json").is_empty());
        // Kontext: alles, was der Aufruf mitgeschickt hat, auch aus dem Cache
        let used = r#"{"type":"assistant","message":{"usage":{"input_tokens":3,"cache_creation_input_tokens":200,"cache_read_input_tokens":40000},"content":[]}}"#;
        assert_eq!(parse_line(used), [Event::Context(40203)]);
    }

    #[test]
    fn tour_from_json() {
        let (t, hints) = parse_tour(r#"{"schritte":[
            {"text":"Hier","ziel":"viewer1","seite":3,"zeichen":"markierung","suche":"Amagat"},
            {"text":"Da","seite":1,"marken":[{"bereich":[0.1,0.2,0.5,0.3],"text":"Schild"},{"zeichen":"fokus","suche":"x"}],"verbinde":[[0,1]],
             "skizze":[[[0.1,0.1],[0.2,0.2]],[[0.5,0.5]]],"tipp":"Denk an c_p","aufgabe":"Rechne aus"},
            {"text":"Quiz","quiz":{"optionen":["a","b"],"richtig":1,"erklaerung":"weil"}},
            {"text":"","suche":"fällt weg"}],"vorschlaege":["Noch ein Beispiel"]}"#);
        assert_eq!(t.len(), 3);
        assert_eq!(hints, ["Noch ein Beispiel"]);
        assert_eq!((t[0].target.as_str(), t[0].page), ("viewer1", Some(2)));
        assert_eq!((t[0].marks[0].kind, t[0].marks[0].find.as_deref()), (MarkKind::Highlight, Some("Amagat")));
        assert_eq!((t[1].marks.len(), t[1].marks[0].kind, t[1].marks[1].kind), (2, MarkKind::Circle, MarkKind::Focus));
        assert_eq!(t[1].marks[0].label.as_deref(), Some("Schild"));
        assert_eq!(t[1].links, [(0, 1)]);
        assert_eq!(t[1].sketch.len(), 1, "Linie aus einem Punkt fällt weg");
        assert_eq!((t[1].tip.as_deref(), t[1].task.as_deref()), (Some("Denk an c_p"), Some("Rechne aus")));
        let q = t[2].quiz.as_ref().unwrap();
        assert_eq!((q.options.len(), q.right, q.why.as_deref()), (2, 1, Some("weil")));
        assert!(parse_tour("kaputt").0.is_empty());
        assert_eq!(parse_tour(r#"{"vorschlaege":["x"]}"#).1, ["x"]);
    }

    fn spot(marks: &[Rect], ink: impl Fn(&Rect) -> f64) -> (f64, f64) {
        best_card_spot(Rect::new(12.0, 12.0, 988.0, 716.0), (300.0, 120.0), marks, &[], ink, None)
    }

    #[test]
    fn card_stays_close_on_an_empty_page() {
        let mark = Rect::new(400.0, 300.0, 600.0, 340.0);
        let (x, y) = spot(&[mark], |_| 0.0);
        let r = Rect::new(x, y, x + 300.0, y + 120.0);
        assert!(!r.intersects(&mark.expand(CARD_CLEARANCE)));
        assert!(rect_gap(&r, &mark) < 40.0);
    }

    #[test]
    fn card_avoids_text_and_stays_inside() {
        let mark = Rect::new(400.0, 300.0, 600.0, 340.0);
        // Text überall außer in einem Streifen unter dem Zeichen
        let free = Rect::new(250.0, 350.0, 750.0, 500.0);
        let (x, y) = spot(&[mark], |r| (r.width() * r.height()) - overlap(r, &free));
        let r = Rect::new(x, y, x + 300.0, y + 120.0);
        assert!(overlap(&r, &free) > 0.95 * 300.0 * 120.0, "{r:?}");
        assert!(r.x0 >= 12.0 && r.x1 <= 988.0 && r.y0 >= 12.0 && r.y1 <= 716.0);
    }

    #[test]
    fn free_card_stays_above_the_dock() {
        let (_, y) = free_card_position((300.0, 120.0), (1000.0, 800.0), 84.0);
        assert!(y + 120.0 <= 800.0 - 84.0);
    }
}
