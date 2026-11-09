//! Live-Diagramm für die Lernkarte: Kurven aus Formeln und Schieberegler für ihre Parameter.

use std::collections::HashMap;

use serde_json::Value;

// ------------------------------------------------------------------------ Formeln

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Var(String),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Call(String, Box<Expr>),
}

const FUNCTIONS: [&str; 11] = ["sin", "cos", "tan", "exp", "ln", "log", "sqrt", "abs", "asin", "acos", "atan"];

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        let hit = self.peek() == Some(c);
        self.i += hit as usize;
        hit
    }
    fn sum(&mut self) -> Result<Expr, String> {
        let mut a = self.product()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            a = Expr::Bin(op as char, Box::new(a), Box::new(self.product()?));
        }
        Ok(a)
    }
    fn product(&mut self) -> Result<Expr, String> {
        let mut a = self.unary()?;
        while let Some(op @ (b'*' | b'/')) = self.peek() {
            self.i += 1;
            a = Expr::Bin(op as char, Box::new(a), Box::new(self.unary()?));
        }
        Ok(a)
    }
    /// Minus bindet schwächer als die Potenz: -x^2 ist -(x^2).
    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(b'-') {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.eat(b'+');
        self.power()
    }
    fn power(&mut self) -> Result<Expr, String> {
        let base = self.atom()?;
        if self.eat(b'^') {
            return Ok(Expr::Bin('^', Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        let Some(c) = self.peek() else { return Err("Formel endet zu früh".into()) };
        if self.eat(b'(') {
            let e = self.sum()?;
            return if self.eat(b')') { Ok(e) } else { Err("Klammer fehlt".into()) };
        }
        let start = self.i;
        if c.is_ascii_digit() || c == b'.' {
            while self.s.get(self.i).is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
                self.i += 1;
            }
            // 1e-3, aber nicht 2e als 2 mal e
            if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
                let mut j = self.i + 1;
                if matches!(self.s.get(j), Some(b'+' | b'-')) {
                    j += 1;
                }
                if self.s.get(j).is_some_and(u8::is_ascii_digit) {
                    while self.s.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    self.i = j;
                }
            }
            let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
            return t.parse().map(Expr::Num).map_err(|_| format!("Zahl {t}"));
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while self.s.get(self.i).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                self.i += 1;
            }
            let name = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("").to_string();
            if self.eat(b'(') {
                if !FUNCTIONS.contains(&name.as_str()) {
                    return Err(format!("unbekannte Funktion {name}"));
                }
                let e = self.sum()?;
                return if self.eat(b')') { Ok(Expr::Call(name, Box::new(e))) } else { Err("Klammer fehlt".into()) };
            }
            return Ok(match name.as_str() {
                "pi" => Expr::Num(std::f64::consts::PI),
                "e" => Expr::Num(std::f64::consts::E),
                _ => Expr::Var(name),
            });
        }
        Err(format!("unerwartetes Zeichen {}", c as char))
    }
}

pub fn parse(s: &str) -> Result<Expr, String> {
    let mut p = Parser { s: s.as_bytes(), i: 0 };
    let e = p.sum()?;
    if p.peek().is_some() {
        return Err(format!("unerwartet am Ende: {}", &s[p.i..]));
    }
    Ok(e)
}

impl Expr {
    pub fn eval(&self, vars: &HashMap<String, f64>) -> f64 {
        match self {
            Expr::Num(n) => *n,
            Expr::Var(v) => vars.get(v).copied().unwrap_or(f64::NAN),
            Expr::Neg(a) => -a.eval(vars),
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.eval(vars), b.eval(vars));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    _ => a.powf(b),
                }
            }
            Expr::Call(f, a) => {
                let a = a.eval(vars);
                match f.as_str() {
                    "sin" => a.sin(),
                    "cos" => a.cos(),
                    "tan" => a.tan(),
                    "exp" => a.exp(),
                    "ln" => a.ln(),
                    "log" => a.log10(),
                    "sqrt" => a.sqrt(),
                    "abs" => a.abs(),
                    "asin" => a.asin(),
                    "acos" => a.acos(),
                    _ => a.atan(),
                }
            }
        }
    }

    fn vars_into(&self, out: &mut Vec<String>) {
        match self {
            Expr::Var(v) => out.push(v.clone()),
            Expr::Neg(a) | Expr::Call(_, a) => a.vars_into(out),
            Expr::Bin(_, a, b) => {
                a.vars_into(out);
                b.vars_into(out);
            }
            Expr::Num(_) => {}
        }
    }
}

// ------------------------------------------------------------------------ Diagramm

#[derive(Clone, Debug, PartialEq)]
pub struct Slider {
    pub symbol: String,
    pub name: String,
    pub unit: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    pub title: String,
    pub x_name: String,
    pub y_name: String,
    pub x: (f64, f64),
    pub y: Option<(f64, f64)>,
    pub curves: Vec<(String, Expr)>,
    pub sliders: Vec<Slider>,
}

/// `None`, wenn das Diagramm unvollständig ist oder Formeln unbekannte Größen nutzen.
pub fn from_json(v: &Value) -> Option<Plot> {
    let range = |a: &Value, b: &Value| Some((a.as_f64()?, b.as_f64()?)).filter(|(l, h)| h > l);
    let x = range(&v["x"]["min"], &v["x"]["max"])?;
    let sliders: Vec<Slider> = v["regler"]
        .as_array()
        .into_iter()
        .flatten()
        .take(4)
        .filter_map(|s| {
            let (min, max) = range(&s["min"], &s["max"])?;
            let symbol = s["symbol"].as_str()?.to_string();
            Some(Slider {
                name: s["name"].as_str().unwrap_or(&symbol).to_string(),
                unit: s["einheit"].as_str().unwrap_or("").to_string(),
                value: s["wert"].as_f64().unwrap_or((min + max) / 2.0).clamp(min, max),
                symbol,
                min,
                max,
            })
        })
        .collect();
    let curves: Vec<(String, Expr)> = v["kurven"].as_array()?.iter().take(4).filter_map(|c| Some((c["name"].as_str().unwrap_or("").to_string(), parse(c["formel"].as_str()?).ok()?))).collect();
    if curves.is_empty() {
        return None;
    }
    let known = |n: &String| n == "x" || sliders.iter().any(|s| &s.symbol == n);
    for (_, e) in &curves {
        let mut used = Vec::new();
        e.vars_into(&mut used);
        if !used.iter().all(known) {
            return None;
        }
    }
    Some(Plot {
        title: v["titel"].as_str().unwrap_or("").to_string(),
        x_name: v["x"]["name"].as_str().unwrap_or("x").to_string(),
        y_name: v["y"]["name"].as_str().unwrap_or("y").to_string(),
        x,
        y: range(&v["y"]["min"], &v["y"]["max"]),
        curves,
        sliders,
    })
}

pub fn initial_values(p: &Plot) -> HashMap<String, f64> {
    p.sliders.iter().map(|s| (s.symbol.clone(), s.value)).collect()
}

const SAMPLES: usize = 240;

fn samples(p: &Plot, e: &Expr, values: &HashMap<String, f64>) -> Vec<(f64, f64)> {
    let mut vars = values.clone();
    (0..=SAMPLES)
        .map(|k| {
            let x = p.x.0 + (p.x.1 - p.x.0) * k as f64 / SAMPLES as f64;
            vars.insert("x".to_string(), x);
            (x, e.eval(&vars))
        })
        .collect()
}

/// Y-Bereich; ohne Vorgabe aus Startwerten und aktueller Einstellung.
fn y_range(p: &Plot, start: &HashMap<String, f64>, now: &HashMap<String, f64>) -> (f64, f64) {
    if let Some(y) = p.y {
        return y;
    }
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for (_, e) in &p.curves {
        for v in [start, now] {
            for (_, y) in samples(p, e, v) {
                if y.is_finite() {
                    lo = lo.min(y);
                    hi = hi.max(y);
                }
            }
        }
    }
    if lo > hi {
        return (0.0, 1.0);
    }
    let pad = ((hi - lo) * 0.08).max(1e-9);
    (lo - pad, hi + pad)
}

/// Runde Teilung für eine Achse.
pub fn ticks(lo: f64, hi: f64) -> Vec<f64> {
    let raw = (hi - lo) / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag);
    let mut v = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while v <= hi + step * 1e-6 {
        out.push(if v.abs() < step * 1e-6 { 0.0 } else { v });
        v += step;
    }
    out
}

pub fn fmt(v: f64) -> String {
    let a = v.abs();
    if a != 0.0 && !(0.01..100_000.0).contains(&a) {
        format!("{v:.1e}")
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Farbschema: dunkel in der Karte, hell für die Notiz.
#[derive(Clone, Copy)]
pub struct Theme {
    pub ink: (f64, f64, f64),
    pub grid: (f64, f64, f64, f64),
}
pub const DARK: Theme = Theme { ink: (0.945, 0.941, 0.929), grid: (1.0, 1.0, 1.0, 0.12) };
pub const LIGHT: Theme = Theme { ink: (0.11, 0.11, 0.13), grid: (0.0, 0.0, 0.0, 0.14) };
const COLORS: [(f64, f64, f64); 4] = [(0.25, 0.66, 0.96), (0.95, 0.5, 0.05), (0.36, 0.79, 0.35), (0.91, 0.33, 0.61)];

pub fn draw(cr: &cairo::Context, w: f64, h: f64, p: &Plot, start: &HashMap<String, f64>, now: &HashMap<String, f64>, t: Theme) {
    const LEFT: f64 = 46.0;
    const RIGHT: f64 = 10.0;
    const BOTTOM: f64 = 32.0;
    let top = if p.title.is_empty() { 8.0 } else { 24.0 };
    let (x0, y0, x1, y1) = (LEFT, top, w - RIGHT, h - BOTTOM);
    let yr = y_range(p, start, now);
    let sx = |x: f64| x0 + (x - p.x.0) / (p.x.1 - p.x.0) * (x1 - x0);
    let sy = |y: f64| y1 - (y - yr.0) / (yr.1 - yr.0) * (y1 - y0);
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(10.0);
    let text = |s: &str, x: f64, y: f64, anchor: f64| {
        if let Ok(e) = cr.text_extents(s) {
            cr.move_to(x - e.width() * anchor, y);
            let _ = cr.show_text(s);
        }
    };
    cr.set_line_width(1.0);
    for v in ticks(p.x.0, p.x.1) {
        cr.set_source_rgba(t.grid.0, t.grid.1, t.grid.2, t.grid.3);
        cr.move_to(sx(v), y0);
        cr.line_to(sx(v), y1);
        let _ = cr.stroke();
        cr.set_source_rgb(t.ink.0, t.ink.1, t.ink.2);
        text(&fmt(v), sx(v), y1 + 13.0, 0.5);
    }
    for v in ticks(yr.0, yr.1) {
        cr.set_source_rgba(t.grid.0, t.grid.1, t.grid.2, t.grid.3);
        cr.move_to(x0, sy(v));
        cr.line_to(x1, sy(v));
        let _ = cr.stroke();
        cr.set_source_rgb(t.ink.0, t.ink.1, t.ink.2);
        text(&fmt(v), x0 - 4.0, sy(v) + 3.0, 1.0);
    }
    cr.set_source_rgb(t.ink.0, t.ink.1, t.ink.2);
    text(&p.x_name, (x0 + x1) / 2.0, h - 5.0, 0.5);
    text(&p.y_name, x0, y0 - 3.0, 0.5);
    if !p.title.is_empty() {
        text(&p.title, x1, 12.0, 1.0);
    }
    // Kurven nur im Diagrammfeld
    let _ = cr.save();
    cr.rectangle(x0, y0, x1 - x0, y1 - y0);
    cr.clip();
    let changed = start != now;
    for (i, (_, e)) in p.curves.iter().enumerate() {
        let c = COLORS[i % COLORS.len()];
        let stroke = |vals: &HashMap<String, f64>, dash: bool| {
            let mut pen_down = false;
            for (x, y) in samples(p, e, vals) {
                if !y.is_finite() {
                    pen_down = false;
                    continue;
                }
                if pen_down { cr.line_to(sx(x), sy(y)) } else { cr.move_to(sx(x), sy(y)) }
                pen_down = true;
            }
            if dash {
                cr.set_dash(&[4.0, 4.0], 0.0);
                cr.set_source_rgba(c.0, c.1, c.2, 0.5);
                cr.set_line_width(1.2);
            } else {
                cr.set_dash(&[], 0.0);
                cr.set_source_rgb(c.0, c.1, c.2);
                cr.set_line_width(2.0);
            }
            let _ = cr.stroke();
        };
        // Ausgangskurve gestrichelt: zeigt, was der Regler verändert hat
        if changed {
            stroke(start, true);
        }
        stroke(now, false);
    }
    cr.set_dash(&[], 0.0);
    let _ = cr.restore();
    // Legende bei mehreren Kurven
    if p.curves.len() > 1 {
        let mut y = y0 + 12.0;
        for (i, (name, _)) in p.curves.iter().enumerate() {
            let c = COLORS[i % COLORS.len()];
            cr.set_source_rgb(c.0, c.1, c.2);
            text(name, x1 - 4.0, y, 1.0);
            y += 12.0;
        }
    }
}

/// Aktueller Stand als PNG für die Notiz.
pub fn png(p: &Plot, start: &HashMap<String, f64>, now: &HashMap<String, f64>) -> Option<Vec<u8>> {
    const SCALE: f64 = 3.0;
    const W: f64 = 340.0;
    const H: f64 = 200.0;
    let s = cairo::ImageSurface::create(cairo::Format::Rgb24, (W * SCALE) as i32, (H * SCALE) as i32).ok()?;
    {
        let cr = cairo::Context::new(&s).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(SCALE, SCALE);
        draw(&cr, W, H, p, start, now, LIGHT);
    }
    let mut out = Vec::new();
    s.write_to_png(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn val(s: &str, x: f64) -> f64 {
        parse(s).unwrap().eval(&HashMap::from([("x".to_string(), x), ("a".to_string(), 2.0)]))
    }

    #[test]
    fn evaluates() {
        assert_eq!(val("1+2*3", 0.0), 7.0);
        assert_eq!(val("(1+2)*3", 0.0), 9.0);
        assert_eq!(val("-x^2", 3.0), -9.0);
        assert_eq!(val("2^3^2", 0.0), 512.0);
        assert_eq!(val("a*x/4", 6.0), 3.0);
        assert_eq!(val("1e-3*1000", 0.0), 1.0);
        assert!((val("2*e", 0.0) - 2.0 * std::f64::consts::E).abs() < 1e-12);
        assert!((val("sin(pi/2)", 0.0) - 1.0).abs() < 1e-12);
        assert_eq!(val("sqrt(x)", 16.0), 4.0);
        assert!(val("ln(x)", -1.0).is_nan());
        assert!(parse("2+").is_err());
        assert!(parse("2 3").is_err());
        assert!(parse("foo(1)").is_err());
        assert!(parse("(1").is_err());
    }

    #[test]
    fn reads_diagram() {
        let v = json!({"titel":"T","x":{"name":"x","min":0,"max":1},"kurven":[{"name":"k","formel":"a*x"}],"regler":[{"symbol":"a","min":0,"max":5,"wert":9}]});
        let p = from_json(&v).unwrap();
        assert_eq!(p.sliders[0].value, 5.0);
        assert_eq!(p.y, None);
        assert_eq!(initial_values(&p)["a"], 5.0);
        // Formel nutzt eine Größe ohne Regler
        let bad = json!({"x":{"min":0,"max":1},"kurven":[{"formel":"b*x"}]});
        assert!(from_json(&bad).is_none());
        assert!(from_json(&json!({"x":{"min":1,"max":0},"kurven":[{"formel":"x"}]})).is_none());
    }

    #[test]
    fn nice_ticks() {
        assert_eq!(ticks(0.0, 10.0), [0.0, 5.0, 10.0]);
        assert_eq!(fmt(0.5), "0.5");
        assert_eq!(fmt(210000.0), "2.1e5");
    }
}
