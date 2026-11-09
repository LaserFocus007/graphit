//! Benutzereinstellungen (~/.config/graphit/settings.conf, einfaches key=value).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InkStyle {
    /// Druckstrich als eine glatte Fläche (stufenlose Breitenwechsel).
    Smooth,
    /// Segment für Segment wie Xournal++.
    Classic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Dot,
    Crosshair,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PalmMode {
    /// Touch nur sperren, während der Stift das Display berührt (+ kurz danach).
    Contact,
    /// Touch sperren, solange der Stift in der Nähe schwebt.
    Proximity,
    Off,
}

/// Belegung einer Stifttaste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    Eraser,
    Select,
    Hand,
    Radial,
    Nothing,
}

impl ButtonAction {
    pub const ALL: [ButtonAction; 5] =
        [ButtonAction::Eraser, ButtonAction::Select, ButtonAction::Hand, ButtonAction::Radial, ButtonAction::Nothing];
    pub fn label(&self) -> &'static str {
        match self {
            ButtonAction::Eraser => "Radierer",
            ButtonAction::Select => "Auswahl (Lasso)",
            ButtonAction::Hand => "Verschieben (Hand)",
            ButtonAction::Radial => "Radialmenü",
            ButtonAction::Nothing => "Nichts",
        }
    }
    fn key(&self) -> &'static str {
        match self {
            ButtonAction::Eraser => "eraser",
            ButtonAction::Select => "select",
            ButtonAction::Hand => "hand",
            ButtonAction::Radial => "radial",
            ButtonAction::Nothing => "none",
        }
    }
    fn parse(v: &str) -> Option<ButtonAction> {
        ButtonAction::ALL.into_iter().find(|a| a.key() == v)
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub pressure: bool,
    /// Exponent der Druckkurve (<1 weich, 1 linear, >1 hart)
    pub pressure_curve: f64,
    /// 0 = keine Glättung, 1 = maximal
    pub smoothing: f64,
    pub ink_style: InkStyle,
    /// Vorhersage in Millisekunden (0 = aus)
    pub prediction_ms: f64,
    pub cursor: CursorStyle,
    pub hold_to_straighten: bool,
    pub hold_delay_ms: u64,
    pub palm: PalmMode,
    pub kinetic: bool,
    /// Freihand-Formen beim Absetzen automatisch erkennen
    pub auto_shapes: bool,
    /// Radialmenü per langem Druck mit dem Stift
    pub radial_longpress: bool,
    pub radial_delay_ms: u64,
    /// Stifttaste 1 (GDK-Taste 2), Stifttaste 2 (GDK-Taste 3), Radierer-Ende
    pub button1: ButtonAction,
    pub button2: ButtonAction,
    pub eraser_end: ButtonAction,
    /// Kurzes Tippen (wie Xournal++ "Strichfilter"): öffnet das Radialmenü
    pub tap_radial: bool,
    /// Beim Tippen zuerst versuchen, ein Objekt auszuwählen
    pub tap_select: bool,
    pub tap_ms: u64,
    pub tap_mm: f64,
    /// Tippen kurz nach einem Strich zählt als Punkt (z. B. i-Punkt)
    pub tap_successive_ms: u64,
    /// Druckmodell wie Xournal++: Breite = Breite × Druck × Faktor (0 = Graphit-Kurve)
    /// Mindestgröße für die Formerkennung (mm)
    pub shape_min_mm: f64,
    /// Beim Schreiben auf der letzten Seite automatisch eine leere Seite anhängen
    pub append_page: bool,
    /// Radierer löscht ganze Striche, die er berührt (statt Teile wegzuradieren)
    pub eraser_whole: bool,
    pub dark_paper: bool,
    /// Papier für neue Dokumente und Seiten: graph, ruled, lined, dotted, plain
    pub paper_style: String,
    /// Claude-Modell, mit dem der Dialog startet (fable, opus, sonnet, haiku)
    pub claude_model: String,
    /// Effort, mit dem der Dialog startet (leer = Vorgabe des Modells)
    pub claude_effort: String,
    /// Öffnet Knopf oder Radialmenü den Dialog, die Bildschirmtastatur mit einblenden
    pub claude_osk: bool,
    /// Vom Nutzer gezogene Höhe des Antwortfensters (0 = Vorgabe)
    pub claude_height: i32,
    /// KI-Fenster als Spalte rechts angeheftet
    pub claude_pinned: bool,
    /// Ort des schwebenden KI-Fensters, negativ für die Mitte
    pub claude_x: f64,
    pub claude_y: f64,
    /// Breite der angehefteten Spalte
    pub claude_col_w: i32,
    /// Altklausur-Ordner je Modul, wenn vom Vorschlag abweichend
    pub claude_exams: String,
    /// Ordner der Altklausuren; leer: `Old_exams` im Notizordner
    pub exam_root: String,
    /// Schwung: Gleitweite 0 (kurz) … 1 (sehr lang)
    pub kinetic_glide: f64,
    /// Schwung: Stärke (Faktor auf die Wischgeschwindigkeit)
    pub kinetic_boost: f64,
    /// Schwung nur in Hauptrichtung (kein seitliches Driften)
    pub kinetic_axis_lock: bool,
    /// Finger-Vorhersage beim Schieben/Zoomen in ms (0 = aus)
    pub touch_predict_ms: f64,
    /// Stift-Vorhersage mit Beschleunigung und Kurven statt geradeaus
    /// Druck-Kalibrierung: dieser Bereich der Rohwerte wird auf 0…1 gestreckt
    pub pressure_lo: f64,
    pub pressure_hi: f64,
    /// Schräg gehaltener Stift macht Striche breiter (Bleistift)
    pub tilt_shading: bool,
}

/// Papierarten (Xournal++-Namen) mit deutscher Bezeichnung.
pub const PAPERS: [(&str, &str); 5] =
    [("Kariert", "graph"), ("Liniert", "ruled"), ("Liniert mit Rand", "lined"), ("Punkte", "dotted"), ("Blanko", "plain")];

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pressure: true,
            pressure_curve: 0.85,
            smoothing: 0.4,
            ink_style: InkStyle::Smooth,
            prediction_ms: 14.0,
            cursor: CursorStyle::Dot,
            hold_to_straighten: true,
            hold_delay_ms: 450,
            palm: PalmMode::Contact,
            kinetic: true,
            auto_shapes: false,
            radial_longpress: false,
            radial_delay_ms: 450,
            button1: ButtonAction::Select,
            button2: ButtonAction::Eraser,
            eraser_end: ButtonAction::Eraser,
            tap_radial: true,
            tap_select: true,
            tap_ms: 70,
            tap_mm: 2.0,
            tap_successive_ms: 300,
            shape_min_mm: 3.0,
            append_page: true,
            eraser_whole: false,
            dark_paper: false,
            paper_style: "graph".into(),
            claude_model: "sonnet".into(),
            claude_effort: String::new(),
            claude_osk: true,
            claude_height: 0,
            claude_pinned: false,
            claude_x: -1.0,
            claude_y: -1.0,
            claude_col_w: 400,
            claude_exams: String::new(),
            exam_root: String::new(),
            kinetic_glide: 0.5,
            kinetic_boost: 1.0,
            kinetic_axis_lock: true,
            touch_predict_ms: 14.0,
            pressure_lo: 0.0,
            pressure_hi: 1.0,
            tilt_shading: false,
        }
    }
}

fn path() -> PathBuf {
    gtk::glib::user_config_dir().join("graphit").join("settings.conf")
}

impl Settings {
    /// Abklingrate des Schwungs pro Sekunde (Gleitweite 0 → 8/s, 1 → 0,8/s).
    pub fn kinetic_decay(&self) -> f64 {
        8.0 * 0.1f64.powf(self.kinetic_glide)
    }

    /// Lädt die Einstellungen.
    pub fn load() -> Settings {
        let mut s = Settings::default();
        let Ok(text) = std::fs::read_to_string(path()) else {
            s.save();
            return s;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let b = v == "true";
            let f = v.parse::<f64>().ok();
            match k {
                "pressure" => s.pressure = b,
                "pressure_curve" => s.pressure_curve = f.unwrap_or(s.pressure_curve).clamp(0.3, 3.0),
                "smoothing" => s.smoothing = f.unwrap_or(s.smoothing).clamp(0.0, 1.0),
                "ink_style" => s.ink_style = if v == "classic" { InkStyle::Classic } else { InkStyle::Smooth },
                "prediction_ms" => s.prediction_ms = f.unwrap_or(s.prediction_ms).clamp(0.0, 40.0),
                "cursor" => {
                    s.cursor = match v {
                        "crosshair" => CursorStyle::Crosshair,
                        "none" => CursorStyle::None,
                        _ => CursorStyle::Dot,
                    }
                }
                "hold_to_straighten" => s.hold_to_straighten = b,
                "hold_delay_ms" => s.hold_delay_ms = f.unwrap_or(450.0).clamp(150.0, 2000.0) as u64,
                "palm" => {
                    s.palm = match v {
                        "proximity" => PalmMode::Proximity,
                        "off" => PalmMode::Off,
                        _ => PalmMode::Contact,
                    }
                }
                "kinetic" => s.kinetic = b,
                "auto_shapes" => s.auto_shapes = b,
                "radial_longpress" => s.radial_longpress = b,
                "radial_delay_ms" => s.radial_delay_ms = f.unwrap_or(450.0).clamp(200.0, 2000.0) as u64,
                "button1" => s.button1 = ButtonAction::parse(v).unwrap_or(s.button1),
                "button2" => s.button2 = ButtonAction::parse(v).unwrap_or(s.button2),
                "eraser_end" => s.eraser_end = ButtonAction::parse(v).unwrap_or(s.eraser_end),
                "tap_radial" => s.tap_radial = b,
                "tap_select" => s.tap_select = b,
                "tap_ms" => s.tap_ms = f.unwrap_or(70.0).clamp(10.0, 500.0) as u64,
                "tap_mm" => s.tap_mm = f.unwrap_or(2.0).clamp(0.3, 10.0),
                "tap_successive_ms" => s.tap_successive_ms = f.unwrap_or(300.0).clamp(0.0, 2000.0) as u64,
                "shape_min_mm" => s.shape_min_mm = f.unwrap_or(3.0).clamp(0.0, 50.0),
                "append_page" => s.append_page = b,
                "eraser_whole" => s.eraser_whole = b,
                "dark_paper" => s.dark_paper = b,
                "kinetic_glide" => s.kinetic_glide = f.unwrap_or(0.5).clamp(0.0, 1.0),
                "kinetic_boost" => s.kinetic_boost = f.unwrap_or(1.0).clamp(0.3, 3.0),
                "kinetic_axis_lock" => s.kinetic_axis_lock = b,
                "touch_predict_ms" => s.touch_predict_ms = f.unwrap_or(14.0).clamp(0.0, 40.0),
                "pressure_lo" => s.pressure_lo = f.unwrap_or(0.0).clamp(0.0, 0.8),
                "pressure_hi" => s.pressure_hi = f.unwrap_or(1.0).clamp(0.2, 1.0),
                "tilt_shading" => s.tilt_shading = b,
                "paper_style" if PAPERS.iter().any(|p| p.1 == v) => s.paper_style = v.into(),
                "claude_model" => s.claude_model = v.into(),
                "claude_effort" => s.claude_effort = v.into(),
                "claude_osk" => s.claude_osk = b,
                "claude_height" => s.claude_height = f.unwrap_or(0.0) as i32,
                "claude_pinned" => s.claude_pinned = b,
                "claude_x" => s.claude_x = f.unwrap_or(-1.0),
                "claude_y" => s.claude_y = f.unwrap_or(-1.0),
                "claude_col_w" => s.claude_col_w = f.unwrap_or(400.0) as i32,
                "claude_exams" => s.claude_exams = v.into(),
                "exam_root" => s.exam_root = v.into(),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let text = format!(
            "pressure={}\npressure_curve={}\nsmoothing={}\nink_style={}\nprediction_ms={}\ncursor={}\nhold_to_straighten={}\nhold_delay_ms={}\npalm={}\nkinetic={}\nauto_shapes={}\nradial_longpress={}\nradial_delay_ms={}\nbutton1={}\nbutton2={}\neraser_end={}\ntap_radial={}\ntap_select={}\ntap_ms={}\ntap_mm={}\ntap_successive_ms={}\nshape_min_mm={}\nappend_page={}\neraser_whole={}\ndark_paper={}\npaper_style={}\nkinetic_glide={}\nkinetic_boost={}\nkinetic_axis_lock={}\ntouch_predict_ms={}\npressure_lo={}\npressure_hi={}\ntilt_shading={}\nclaude_model={}\nclaude_effort={}\nclaude_osk={}\nclaude_height={}\nclaude_pinned={}\nclaude_x={}\nclaude_y={}\nclaude_col_w={}\nclaude_exams={}\nexam_root={}\n",
            self.pressure,
            self.pressure_curve,
            self.smoothing,
            if self.ink_style == InkStyle::Classic { "classic" } else { "smooth" },
            self.prediction_ms,
            match self.cursor {
                CursorStyle::Dot => "dot",
                CursorStyle::Crosshair => "crosshair",
                CursorStyle::None => "none",
            },
            self.hold_to_straighten,
            self.hold_delay_ms,
            match self.palm {
                PalmMode::Contact => "contact",
                PalmMode::Proximity => "proximity",
                PalmMode::Off => "off",
            },
            self.kinetic,
            self.auto_shapes,
            self.radial_longpress,
            self.radial_delay_ms,
            self.button1.key(),
            self.button2.key(),
            self.eraser_end.key(),
            self.tap_radial,
            self.tap_select,
            self.tap_ms,
            self.tap_mm,
            self.tap_successive_ms,
            self.shape_min_mm,
            self.append_page,
            self.eraser_whole,
            self.dark_paper,
            self.paper_style,
            self.kinetic_glide,
            self.kinetic_boost,
            self.kinetic_axis_lock,
            self.touch_predict_ms,
            self.pressure_lo,
            self.pressure_hi,
            self.tilt_shading,
            self.claude_model,
            self.claude_effort,
            self.claude_osk,
            self.claude_height,
            self.claude_pinned,
            self.claude_x,
            self.claude_y,
            self.claude_col_w,
            self.claude_exams,
            self.exam_root,
        );
        let p = path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, text);
    }
}

fn library_file() -> PathBuf {
    gtk::glib::user_config_dir().join("graphit").join("notizordner.txt")
}

/// Wurde der Notizordner schon festgelegt? Sonst läuft die Ersteinrichtung.
pub fn library_chosen() -> bool {
    library_file().exists()
}

pub fn choose_library(dir: &std::path::Path) {
    let file = library_file();
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(file, dir.to_string_lossy().as_bytes());
}

pub fn default_library() -> PathBuf {
    gtk::glib::user_special_dir(gtk::glib::UserDirectory::Documents).unwrap_or_else(|| gtk::glib::home_dir().join("Documents")).join("Graphit")
}

pub fn library() -> PathBuf {
    std::fs::read_to_string(library_file()).ok().map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(default_library)
}
