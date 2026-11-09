//! Werkzeuge und ihre Einstellungen.

use crate::geom::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Select,
    Shape,
    Text,
    Hand,
    Space,
    /// Stelle einkreisen und Claude dazu fragen
    Ask,
}

impl Tool {
    pub fn label(&self) -> &'static str {
        match self {
            Tool::Pen => crate::i18n::tl("Stift"),
            Tool::Highlighter => crate::i18n::tl("Marker"),
            Tool::Eraser => crate::i18n::tl("Radierer"),
            Tool::Select => crate::i18n::tl("Auswahl"),
            Tool::Shape => crate::i18n::tl("Formen"),
            Tool::Text => "Text",
            Tool::Hand => "Hand",
            Tool::Space => crate::i18n::tl("Platz einfügen"),
            Tool::Ask => crate::i18n::tl("Einkreisen und Claude fragen"),
        }
    }
    pub fn icon(&self) -> &'static str {
        match self {
            Tool::Pen => "fk-pen-symbolic",
            Tool::Highlighter => "fk-highlighter-symbolic",
            Tool::Eraser => "fk-eraser-symbolic",
            Tool::Select => "fk-lasso-symbolic",
            Tool::Shape => "fk-shapes-symbolic",
            Tool::Text => "fk-text-symbolic",
            Tool::Hand => "fk-hand-symbolic",
            Tool::Space => "fk-space-symbolic",
            Tool::Ask => "fk-ask-symbolic",
        }
    }
    /// Werkzeuge, die mit Farbe/Breite arbeiten.
    pub fn uses_pen(&self) -> bool {
        matches!(self, Tool::Pen | Tool::Highlighter | Tool::Shape | Tool::Text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EraserMode {
    /// Radiert Teile von Strichen weg.
    Standard,
    /// Löscht ganze Striche.
    Stroke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    Lasso,
    Rect,
    Object,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Line,
    Arrow,
    DoubleArrow,
    Rect,
    Ellipse,
    Axes,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 6] =
        [ShapeKind::Line, ShapeKind::Arrow, ShapeKind::DoubleArrow, ShapeKind::Rect, ShapeKind::Ellipse, ShapeKind::Axes];
    pub fn label(&self) -> &'static str {
        match self {
            ShapeKind::Line => crate::i18n::tl("Linie"),
            ShapeKind::Arrow => crate::i18n::tl("Pfeil"),
            ShapeKind::DoubleArrow => crate::i18n::tl("Doppelpfeil"),
            ShapeKind::Rect => crate::i18n::tl("Rechteck"),
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Axes => crate::i18n::tl("Koordinatensystem"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStyle {
    Plain,
    Dash,
    Dot,
    DashDot,
}

impl LineStyle {
    pub fn attr(&self) -> Option<&'static str> {
        match self {
            LineStyle::Plain => None,
            LineStyle::Dash => Some("dash"),
            LineStyle::Dot => Some("dot"),
            LineStyle::DashDot => Some("dashdot"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PenSettings {
    pub color: Color,
    pub width: f64,
    pub style: LineStyle,
    pub fill: bool,
}

/// Breitenstufen (sehr fein, fein, mittel, dick) in pt - Werte wie in Xournal++.
pub const PEN_WIDTHS: [f64; 4] = [0.42, 0.85, 1.41, 2.26];
pub const HL_WIDTHS: [f64; 4] = [3.0, 5.0, 9.0, 15.0];
pub const ERASER_SIZES: [f64; 4] = [1.5, 3.0, 7.0, 16.0];

/// Farbpalette, im Farbmenü als 4 Reihen zu je 5: kräftig, hell, weitere, dunkel/pastell.
pub const PALETTE: [Color; 20] = [
    Color::rgb(0.114, 0.114, 0.133),
    Color::rgb(0.122, 0.373, 0.820),
    Color::rgb(0.851, 0.169, 0.227),
    Color::rgb(0.122, 0.604, 0.310),
    Color::rgb(0.949, 0.502, 0.051),
    Color::rgb(0.247, 0.663, 0.961),
    Color::rgb(0.361, 0.788, 0.353),
    Color::rgb(1.000, 0.478, 0.400),
    Color::rgb(0.949, 0.784, 0.067),
    Color::rgb(0.663, 0.545, 0.941),
    Color::rgb(0.494, 0.271, 0.780),
    Color::rgb(0.078, 0.639, 0.627),
    Color::rgb(0.910, 0.333, 0.608),
    Color::rgb(0.545, 0.353, 0.169),
    Color::rgb(0.502, 0.502, 0.533),
    Color::rgb(0.106, 0.184, 0.478),
    Color::rgb(0.118, 0.369, 0.227),
    Color::rgb(0.561, 0.114, 0.227),
    Color::rgb(0.310, 0.839, 0.722),
    Color::rgb(0.961, 0.604, 0.776),
];

pub const COLOR_NAMES: [&str; 20] = [
    "Schwarz",
    "Blau",
    "Rot",
    "Grün",
    "Orange",
    "Hellblau",
    "Hellgrün",
    "Koralle",
    "Gelb",
    "Lavendel",
    "Violett",
    "Türkis",
    "Pink",
    "Braun",
    "Grau",
    "Dunkelblau",
    "Dunkelgrün",
    "Weinrot",
    "Mint",
    "Rosa",
];

/// Farben direkt in Werkzeug- und Auswahlleiste: Schwarz, Blau, Rot, Grün, Hellblau, Hellgrün, Gelb, Orange.
pub const QUICK_COLORS: [usize; 8] = [0, 1, 2, 3, 5, 6, 8, 4];
/// Farben im Radialmenü (8 Segmente).
pub const RADIAL_COLORS: [usize; 8] = [0, 1, 2, 3, 5, 6, 4, 8];

#[derive(Clone, Debug)]
pub struct ToolState {
    pub tool: Tool,
    pub pen: PenSettings,
    pub highlighter: PenSettings,
    pub eraser_mode: EraserMode,
    pub eraser_size: f64,
    pub select_mode: SelectMode,
    pub shape: ShapeKind,
    pub recognizer: bool,
    pub text_size: f64,
    pub text_font: String,
    /// Marker rastet auf PDF-Textzeilen ein
    pub hl_snap: bool,
}

impl Default for ToolState {
    fn default() -> Self {
        ToolState {
            tool: Tool::Pen,
            pen: PenSettings { color: PALETTE[0], width: PEN_WIDTHS[2], style: LineStyle::Plain, fill: false },
            highlighter: PenSettings {
                color: PALETTE[8].with_alpha(0.5),
                width: HL_WIDTHS[2],
                style: LineStyle::Plain,
                fill: false,
            },
            eraser_mode: EraserMode::Standard,
            eraser_size: ERASER_SIZES[2],
            select_mode: SelectMode::Lasso,
            shape: ShapeKind::Line,
            recognizer: false,
            text_size: 12.0,
            text_font: "Sans".into(),
            hl_snap: true,
        }
    }
}

impl ToolState {
    /// Farbe/Breite des Werkzeugs, das gerade "Stift-artig" ist.
    pub fn pen_for(&mut self, t: Tool) -> &mut PenSettings {
        if t == Tool::Highlighter { &mut self.highlighter } else { &mut self.pen }
    }
    pub fn current_pen(&mut self) -> &mut PenSettings {
        let t = self.tool;
        self.pen_for(t)
    }
    pub fn set_color(&mut self, c: Color) {
        match self.tool {
            Tool::Highlighter => self.highlighter.color = c.with_alpha(self.highlighter.color.a),
            _ => self.pen.color = c,
        }
    }
    /// Index der Breitenstufe (0..4) des aktuellen Werkzeugs.
    pub fn width_level(&self) -> usize {
        let (v, table) = match self.tool {
            Tool::Highlighter => (self.highlighter.width, &HL_WIDTHS),
            Tool::Eraser => (self.eraser_size, &ERASER_SIZES),
            _ => (self.pen.width, &PEN_WIDTHS),
        };
        table.iter().enumerate().min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs())).map(|e| e.0).unwrap_or(1)
    }
    pub fn set_width_level(&mut self, i: usize) {
        match self.tool {
            Tool::Highlighter => self.highlighter.width = HL_WIDTHS[i],
            Tool::Eraser => self.eraser_size = ERASER_SIZES[i],
            _ => self.pen.width = PEN_WIDTHS[i],
        }
    }
}
