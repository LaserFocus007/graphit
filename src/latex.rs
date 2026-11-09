//! Formeln im Claude-Dialog: `$…$` und `$$…$$` werden mit pdflatex gesetzt und als Bild
//! in den Text gestellt, auf derselben Grundlinie wie die Schrift. Mehrere Formeln teilen
//! sich einen Lauf; bis eine fertig ist, steht ihr Quelltext da.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk};

/// Bildpunkte je TeX-Punkt: Anzeige (10 pt ergibt etwa die Schrift im Dialog) und Raster.
const SHOW: f64 = 1.8;
const RASTER: f64 = 3.6;
/// Pixel je Punkt der Notiz-Bilder: scharf, auf der Seite in normaler Schriftgröße.
pub const NOTE_RASTER: f64 = 3.0;

// ------------------------------------------------------------------------ Text zerlegen

#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text { text: String, bold: bool, code: bool },
    Math { tex: String, display: bool },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Para {
    pub segs: Vec<Seg>,
    pub head: bool,
    pub code: bool,
}

fn text(out: &mut Vec<Seg>, s: &mut String, bold: bool, code: bool) {
    if !s.is_empty() {
        out.push(Seg::Text { text: std::mem::take(s), bold, code });
    }
}

/// Eine Zeile in Text und Formeln zerlegen.
fn inline(line: &str) -> Vec<Seg> {
    let c: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut bold, mut code) = (false, false);
    let mut i = 0;
    let find = |from: usize, pat: &[char]| (from..c.len().saturating_sub(pat.len() - 1)).find(|&k| c[k..k + pat.len()] == *pat);
    while i < c.len() {
        let rest = &c[i..];
        if rest[0] == '`' {
            text(&mut out, &mut cur, bold, code);
            code = !code;
            i += 1;
        } else if code {
            cur.push(rest[0]);
            i += 1;
        } else if rest.starts_with(&['*', '*']) {
            text(&mut out, &mut cur, bold, code);
            bold = !bold;
            i += 2;
        } else if let Some((open, close, display)) = [(&['$', '$'][..], &['$', '$'][..], true), (&['\\', '['], &['\\', ']'], true), (&['\\', '('], &['\\', ')'], false)]
            .into_iter()
            .find(|d| rest.starts_with(d.0))
        {
            match find(i + open.len(), close) {
                Some(e) => {
                    text(&mut out, &mut cur, bold, code);
                    out.push(Seg::Math { tex: c[i + open.len()..e].iter().collect(), display });
                    i = e + close.len();
                }
                None => {
                    cur.push(rest[0]);
                    i += 1;
                }
            }
        } else if rest[0] == '$' && i + 1 < c.len() && !c[i + 1].is_whitespace() {
            // schließendes $ steht direkt hinter der Formel, sonst ist es ein Geldbetrag
            let e = (i + 1..c.len()).find(|&k| c[k] == '$' && c[k - 1] != '\\' && !c[k - 1].is_whitespace());
            match e {
                Some(e) => {
                    text(&mut out, &mut cur, bold, code);
                    out.push(Seg::Math { tex: c[i + 1..e].iter().collect(), display: false });
                    i = e + 1;
                }
                None => {
                    cur.push('$');
                    i += 1;
                }
            }
        } else if rest[0] == '\\' && rest.get(1) == Some(&'$') {
            cur.push('$');
            i += 2;
        } else {
            cur.push(rest[0]);
            i += 1;
        }
    }
    text(&mut out, &mut cur, bold, code);
    out
}

/// Markdown mit Formeln in Absätze zerlegen. Halbfertiger Text beim Streamen bleibt Text.
pub fn parse(md: &str) -> Vec<Para> {
    let mut out = Vec::new();
    let mut lines = md.lines().peekable();
    let mut fence = false;
    while let Some(l) = lines.next() {
        let t = l.trim_start();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence || t.starts_with('|') {
            out.push(Para { segs: vec![Seg::Text { text: l.to_string(), bold: false, code: true }], head: false, code: true });
            continue;
        }
        // Formel über mehrere Zeilen
        let open = [("$$", "$$"), ("\\[", "\\]")].into_iter().find(|(o, c)| t.trim_end().starts_with(o) && !t.trim_end()[o.len()..].contains(c));
        if let Some((o, c)) = open {
            let mut body = vec![t.trim_end()[o.len()..].to_string()];
            let mut rest = lines.clone();
            let mut used = 0;
            let mut closed = false;
            for n in rest.by_ref() {
                used += 1;
                if let Some(p) = n.find(c) {
                    body.push(n[..p].to_string());
                    closed = true;
                    break;
                }
                body.push(n.to_string());
            }
            if closed {
                for _ in 0..used {
                    lines.next();
                }
                out.push(Para { segs: vec![Seg::Math { tex: body.join("\n").trim().to_string(), display: true }], ..Default::default() });
                continue;
            }
        }
        if let Some(h) = t.strip_prefix("### ").or(t.strip_prefix("## ")).or(t.strip_prefix("# ")) {
            out.push(Para { segs: inline(&h.replace("**", "")), head: true, code: false });
        } else if let Some(i) = t.strip_prefix("- ").or(t.strip_prefix("* ")) {
            let mut segs = vec![Seg::Text { text: format!("{}•  ", &l[..l.len() - t.len()]), bold: false, code: false }];
            segs.extend(inline(i));
            out.push(Para { segs, ..Default::default() });
        } else if t.len() >= 3 && t.chars().all(|c| c == '-') {
            out.push(Para::default());
        } else {
            out.push(Para { segs: inline(l), ..Default::default() });
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Text(String),
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
}

/// Tabellenzeile in Zellen teilen; `|` in Formeln und Code trennt nicht.
fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = vec![String::new()];
    let mut quoted = None;
    let mut prev = ' ';
    for c in t.chars() {
        match quoted {
            Some(q) if c == q && prev != '\\' => quoted = None,
            None if (c == '$' || c == '`') && prev != '\\' => quoted = Some(c),
            _ => {}
        }
        if c == '|' && quoted.is_none() && prev != '\\' {
            cells.push(String::new());
        } else if let Some(last) = cells.last_mut() {
            last.push(c);
        }
        prev = c;
    }
    cells.iter().map(|c| c.trim().replace("\\|", "|")).collect()
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Text und Tabellen trennen. Eine Tabelle gilt ab ihrer Trennzeile, vorher bleibt sie Text.
pub fn blocks(md: &str) -> Vec<Block> {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = Vec::new();
    let mut text: Vec<&str> = Vec::new();
    let mut fence = false;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.trim_start().starts_with("```") {
            fence = !fence;
        }
        if !fence && l.trim_start().starts_with('|') && lines.get(i + 1).is_some_and(|n| is_rule(n)) {
            if text.iter().any(|t| !t.trim().is_empty()) {
                out.push(Block::Text(text.join("\n").trim_end().to_string()));
            }
            text.clear();
            let head = table_cells(l);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                let mut r = table_cells(lines[i]);
                r.resize(head.len(), String::new());
                rows.push(r);
                i += 1;
            }
            out.push(Block::Table { head, rows });
            continue;
        }
        text.push(l);
        i += 1;
    }
    if text.iter().any(|t| !t.trim().is_empty()) {
        out.push(Block::Text(text.join("\n").trim().to_string()));
    }
    out
}

/// Braucht der Text das Setzen als Bild (Formeln oder Tabellen)?
pub fn needs_typesetting(md: &str) -> bool {
    blocks(md).iter().any(|b| matches!(b, Block::Table { .. }))
        || parse(md).iter().any(|p| p.segs.iter().any(|s| matches!(s, Seg::Math { .. })))
}

// ------------------------------------------------------------------------ Setzen

/// Formel prüfen, bevor sie in den gemeinsamen Lauf darf: eine offene Klammer oder ein
/// Dateibefehl würde die übrigen Formeln mitreißen.
fn acceptable(tex: &str) -> bool {
    let mut depth = 0i32;
    let mut prev = ' ';
    for ch in tex.chars() {
        if prev != '\\' {
            depth += (ch == '{') as i32 - (ch == '}') as i32;
        }
        if depth < 0 {
            return false;
        }
        prev = if prev == '\\' && ch == '\\' { ' ' } else { ch };
    }
    let banned = ["\\input", "\\include", "\\write", "\\read", "\\open", "\\immediate", "\\catcode", "\\csname", "\\def", "\\end{document}", "\\shipout"];
    depth == 0 && !tex.is_empty() && !banned.iter().any(|b| tex.contains(b))
}

/// `align` und Co. gehen nicht innerhalb von Mathe: auf die passenden Varianten abbilden.
fn normalize(tex: &str) -> String {
    let mut t = tex.trim().to_string();
    for (a, b) in [("align*", "aligned"), ("align", "aligned"), ("gather*", "gathered"), ("gather", "gathered")] {
        t = t.replace(&format!("\\begin{{{a}}}"), &format!("\\begin{{{b}}}")).replace(&format!("\\end{{{a}}}"), &format!("\\end{{{b}}}"));
    }
    for e in ["equation*", "equation"] {
        t = t.replace(&format!("\\begin{{{e}}}"), "").replace(&format!("\\end{{{e}}}"), "");
    }
    t
}

const PREAMBLE: &str = r"\documentclass[10pt]{article}
\usepackage[utf8]{inputenc}
\usepackage[T1]{fontenc}
\usepackage{amsmath,amssymb,mathtools,bm,xcolor,array}
";

/// Unicode-Zeichen, die Claude auch außerhalb von Formeln schreibt (H₂, Δ, ≈ …);
/// pdflatex kennt sie von sich aus nicht.
const UNICODE: &[(char, &str)] = &[
    ('₀', "_0"), ('₁', "_1"), ('₂', "_2"), ('₃', "_3"), ('₄', "_4"), ('₅', "_5"), ('₆', "_6"), ('₇', "_7"), ('₈', "_8"), ('₉', "_9"),
    ('⁰', "^0"), ('⁴', "^4"), ('⁵', "^5"), ('⁶', "^6"), ('⁷', "^7"), ('⁸', "^8"), ('⁹', "^9"), ('⁻', "^-"), ('⁺', "^+"),
    ('α', "\\alpha"), ('β', "\\beta"), ('γ', "\\gamma"), ('δ', "\\delta"), ('ε', "\\varepsilon"), ('η', "\\eta"), ('θ', "\\theta"),
    ('κ', "\\kappa"), ('λ', "\\lambda"), ('μ', "\\mu"), ('ν', "\\nu"), ('π', "\\pi"), ('ρ', "\\rho"), ('σ', "\\sigma"), ('τ', "\\tau"),
    ('φ', "\\varphi"), ('ω', "\\omega"), ('Γ', "\\Gamma"), ('Δ', "\\Delta"), ('Θ', "\\Theta"), ('Λ', "\\Lambda"), ('Π', "\\Pi"),
    ('Σ', "\\Sigma"), ('Φ', "\\Phi"), ('Ω', "\\Omega"), ('∂', "\\partial"), ('∇', "\\nabla"), ('∞', "\\infty"), ('≈', "\\approx"),
    ('≠', "\\neq"), ('≤', "\\leq"), ('≥', "\\geq"), ('→', "\\rightarrow"), ('←', "\\leftarrow"), ('⇒', "\\Rightarrow"), ('↔', "\\leftrightarrow"),
    ('∝', "\\propto"), ('∑', "\\sum"), ('∫', "\\int"), ('√', "\\sqrt{}"), ('−', "-"), ('×', "\\times"), ('·', "\\cdot"), ('±', "\\pm"), ('°', "^\\circ"),
];

fn preamble() -> String {
    let mut d = String::from(PREAMBLE);
    for (c, tex) in UNICODE {
        d.push_str(&format!("\\DeclareUnicodeCharacter{{{:04X}}}{{\\ensuremath{{{tex}}}}}\n", *c as u32));
    }
    d
}

fn tex_escape(t: &str) -> String {
    let mut o = String::new();
    for c in t.chars() {
        match c {
            '\\' => o.push_str("\\textbackslash{}"),
            '&' | '%' | '$' | '#' | '_' | '{' | '}' => {
                o.push('\\');
                o.push(c);
            }
            '~' => o.push_str("\\textasciitilde{}"),
            '^' => o.push_str("\\textasciicircum{}"),
            c => o.push(c),
        }
    }
    o
}

fn cell_tex(cell: &str) -> String {
    inline(cell)
        .iter()
        .map(|s| match s {
            Seg::Math { tex, .. } => format!("${}$", normalize(tex)),
            Seg::Text { text, bold: true, .. } => format!("\\textbf{{{}}}", tex_escape(text)),
            Seg::Text { text, code: true, .. } => format!("\\texttt{{{}}}", tex_escape(text)),
            Seg::Text { text, .. } => tex_escape(text),
        })
        .collect()
}

/// Tabelle als tabular für das Notiz-Bild. Pandocs longtable geht in einer Box nicht.
fn table_tex(head: &[String], rows: &[Vec<String>]) -> String {
    let weights: Vec<f64> = column_weights(head, rows).iter().map(|c| c.0).collect();
    let total: f64 = weights.iter().sum();
    let cols: String = weights.iter().map(|w| format!(">{{\\raggedright\\arraybackslash}}p{{{:.2}cm}}", 10.2 * w / total)).collect();
    let mut t = format!("\\begin{{tabular}}{{@{{}}{cols}@{{}}}}\n\\hline\n");
    t.push_str(&head.iter().map(|c| format!("\\textbf{{{}}}", cell_tex(c))).collect::<Vec<_>>().join(" & "));
    t.push_str(" \\\\\n\\hline\n");
    for r in rows {
        t.push_str(&r.iter().map(|c| cell_tex(c)).collect::<Vec<_>>().join(" & "));
        t.push_str(" \\\\\n");
    }
    t.push_str("\\hline\n\\end{tabular}");
    t
}

/// Spaltenbreiten nach Inhalt, damit schmale Spalten nicht Platz verschenken:
/// Anteil am Platz und Mindestbreite in Pixeln, in die das längste Wort passt.
fn column_weights(head: &[String], rows: &[Vec<String>]) -> Vec<(f64, f64)> {
    (0..head.len())
        .map(|c| {
            let cells = || std::iter::once(&head[c]).chain(rows.iter().filter_map(|r| r.get(c)));
            let longest = cells().map(|s| s.chars().count()).max().unwrap_or(1);
            let word = cells().flat_map(|s| s.split_whitespace()).map(|w| w.chars().count()).max().unwrap_or(1).min(16);
            ((longest as f64).clamp(6.0, 34.0).max(word as f64 + 3.0), word as f64 * 9.0 + 20.0)
        })
        .collect()
}

/// Breiten der Spalten: erst jedem Wort seinen Platz, der Rest nach Anteil.
fn column_widths(cols: &[(f64, f64)], width: f64) -> Vec<f64> {
    let floor: f64 = cols.iter().map(|c| c.1).sum();
    let total: f64 = cols.iter().map(|c| c.0).sum();
    let spare = (width - floor).max(0.0);
    cols.iter().map(|c| if floor >= width { width * c.1 / floor } else { c.1 + spare * c.0 / total }).collect()
}

fn tex_document(items: &[(String, bool)], ink: &str) -> String {
    let mut d = preamble();
    d.push_str(&format!("\\definecolor{{ink}}{{HTML}}{{{ink}}}\n\\newsavebox\\fb\n\\hoffset=-1in\n\\voffset=-1in\n\\begin{{document}}\n"));
    for (i, (tex, display)) in items.iter().enumerate() {
        let style = if *display { "\\displaystyle " } else { "" };
        d.push_str(&format!(
            "\\typeout{{GRAPHITSTART {i}}}\n\\sbox\\fb{{\\color{{ink}}${style}{}\n$}}\n\
             \\typeout{{GRAPHITBOX {i} \\the\\wd\\fb\\space\\the\\ht\\fb\\space\\the\\dp\\fb}}\n\
             \\pdfpagewidth=\\dimexpr\\wd\\fb+2pt\\relax\n\\pdfpageheight=\\dimexpr\\ht\\fb+\\dp\\fb+2pt\\relax\n\
             \\shipout\\vbox{{\\kern1pt\\hbox{{\\kern1pt\\usebox\\fb}}}}\n",
            normalize(tex)
        ));
    }
    d.push_str("\\end{document}\n");
    d
}

fn run_latex(dir: &Path, strict: bool) -> bool {
    let child = Command::new("pdflatex")
        .args(["-interaction=nonstopmode", "-no-shell-escape"])
        .args(strict.then_some("-halt-on-error"))
        .arg("-output-directory")
        .arg(dir)
        .arg(dir.join("f.tex"))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    for _ in 0..300 {
        if let Ok(Some(s)) = child.try_wait() {
            return s.success();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// Gesetzte Formel als Pixel; `ht`/`dp`: Höhe über und Tiefe unter der Grundlinie in TeX-Punkten.
struct Raw {
    w: i32,
    h: i32,
    stride: usize,
    data: Vec<u8>,
    ht: f64,
    dp: f64,
}

fn points(s: &str) -> Option<f64> {
    s.strip_suffix("pt")?.parse().ok()
}

fn work_dir() -> Option<PathBuf> {
    static RUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = glib::user_cache_dir().join("graphit").join("latex").join(format!("{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn raster(page: &poppler::Page, scale: f64, white: bool) -> Option<cairo::ImageSurface> {
    let (pw, ph) = page.size();
    let s = cairo::ImageSurface::create(cairo::Format::ARgb32, (pw * scale).ceil() as i32, (ph * scale).ceil() as i32).ok()?;
    {
        let cr = cairo::Context::new(&s).ok()?;
        if white {
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().ok()?;
        }
        cr.scale(scale, scale);
        page.render(&cr);
    }
    Some(s)
}

/// Alle Formeln in einem Lauf setzen. Scheitert eine, wird einzeln wiederholt, damit
/// sie nicht die anderen mitnimmt.
fn set_batch(items: &[(String, bool)], ink: &str) -> Vec<Option<Raw>> {
    let run = |items: &[(String, bool)]| -> Option<Vec<Option<Raw>>> {
        let dir = work_dir()?;
        let result = (|| {
            std::fs::write(dir.join("f.tex"), tex_document(items, ink)).ok()?;
            if !run_latex(&dir, true) {
                return None;
            }
            let log = String::from_utf8_lossy(&std::fs::read(dir.join("f.log")).ok()?).into_owned();
            let boxes: HashMap<usize, (f64, f64)> = log
                .lines()
                .filter_map(|l| {
                    let mut f = l.strip_prefix("GRAPHITBOX ")?.split(' ');
                    let i = f.next()?.parse().ok()?;
                    let _width = f.next()?;
                    Some((i, (points(f.next()?)?, points(f.next()?)?)))
                })
                .collect();
            let doc = poppler::Document::from_file(&format!("file://{}", dir.join("f.pdf").display()), None).ok()?;
            Some(
                (0..items.len())
                    .map(|i| {
                        let (ht, dp) = *boxes.get(&i)?;
                        let mut s = raster(&doc.page(i as i32)?, RASTER, false)?;
                        s.flush();
                        let (w, h, stride) = (s.width(), s.height(), s.stride() as usize);
                        let data = s.data().ok()?.to_vec();
                        Some(Raw { w, h, stride, data, ht, dp })
                    })
                    .collect(),
            )
        })();
        let _ = std::fs::remove_dir_all(&dir);
        result
    };
    if let Some(all) = run(items) {
        return all;
    }
    if items.len() == 1 {
        return vec![None];
    }
    items.iter().map(|it| run(std::slice::from_ref(it)).and_then(|mut v| v.pop().flatten())).collect()
}

/// Gesetzte Zeile als PNG, Maße in Punkt.
pub struct SetLine {
    pub png: Vec<u8>,
    pub w: f64,
    pub h: f64,
}

/// Einzeiler aus Text und Formeln als eine einzige TeX-Formel; `None` bei mehr als einer Zeile.
pub fn one_line_tex(md: &str) -> Option<(String, bool)> {
    let blocks = blocks(md);
    if !matches!(&blocks[..], [Block::Text(_)]) {
        return None;
    }
    let paras = parse(md);
    let [p] = &paras[..] else { return None };
    if p.head || p.code {
        return None;
    }
    let mut out = String::new();
    let mut display = false;
    for s in &p.segs {
        match s {
            Seg::Math { tex, display: d } => {
                display |= *d;
                out.push_str(tex);
            }
            Seg::Text { text, bold, .. } => {
                let mut esc = String::new();
                for c in text.chars() {
                    match c {
                        '\\' => esc.push_str("\\textbackslash{}"),
                        '{' | '}' | '$' | '%' | '&' | '#' | '_' => {
                            esc.push('\\');
                            esc.push(c);
                        }
                        '^' => esc.push_str("\\^{}"),
                        '~' => esc.push_str("\\~{}"),
                        c => esc.push(c),
                    }
                }
                out.push_str(&format!("\\text{}{{{esc}}}", if *bold { "bf" } else { "" }));
            }
        }
    }
    Some((out, display))
}

/// Mehrere Einzeiler in einem pdflatex-Lauf setzen.
pub fn set_lines(items: &[(String, bool)], ink: &str) -> Vec<Option<SetLine>> {
    set_batch(items, ink)
        .into_iter()
        .map(|raw| {
            let mut raw = raw?;
            let (w, h) = (raw.w, raw.h);
            let s = cairo::ImageSurface::create_for_data(std::mem::take(&mut raw.data), cairo::Format::ARgb32, w, h, raw.stride as i32).ok()?;
            let mut png = Vec::new();
            s.write_to_png(&mut png).ok()?;
            Some(SetLine { png, w: w as f64 / RASTER, h: h as f64 / RASTER })
        })
        .collect()
}

pub fn block_png(md: &str) -> Option<Vec<u8>> {
    document_png(md)
}

/// Antwort mit Formeln als Bild für die Notiz: schwarz auf weiß, Graphit macht das Weiß
/// beim Anzeigen durchsichtig. Pandoc übersetzt das Markdown.
fn document_png(md: &str) -> Option<Vec<u8>> {
    let md: String = blocks(md)
        .iter()
        .map(|b| match b {
            Block::Text(t) => t.clone(),
            Block::Table { head, rows } => format!("```{{=latex}}\n{}\n```", table_tex(head, rows)),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut pandoc = Command::new("pandoc")
        .args(["-f", "markdown+tex_math_dollars+tex_math_single_backslash", "-t", "latex", "--wrap=none"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        pandoc.stdin.take()?.write_all(md.as_bytes()).ok()?;
    }
    let body = String::from_utf8(pandoc.wait_with_output().ok()?.stdout).ok()?;
    let dir = work_dir()?;
    let png = (|| {
        let tex = format!(
            "{}\\providecommand{{\\tightlist}}{{\\setlength{{\\itemsep}}{{0pt}}\\setlength{{\\parskip}}{{0pt}}}}\n\
             \\setlength{{\\parindent}}{{0pt}}\\setlength{{\\parskip}}{{5pt}}\n\\newsavebox\\fb\n\\hoffset=-1in\n\\voffset=-1in\n\\begin{{document}}\n\
             \\sbox\\fb{{\\begin{{minipage}}{{11cm}}\n{body}\n\\end{{minipage}}}}\n\
             \\pdfpagewidth=\\dimexpr\\wd\\fb+8pt\\relax\n\\pdfpageheight=\\dimexpr\\ht\\fb+\\dp\\fb+8pt\\relax\n\
             \\shipout\\vbox{{\\kern4pt\\hbox{{\\kern4pt\\usebox\\fb}}}}\n\\end{{document}}\n",
            preamble()
        );
        std::fs::write(dir.join("f.tex"), tex).ok()?;
        // Dokumente dürfen kleine Fehler haben: ein unbekanntes Zeichen kostet nicht das Bild
        run_latex(&dir, false);
        let doc = poppler::Document::from_file(&format!("file://{}", dir.join("f.pdf").display()), None).ok()?;
        let s = raster(&doc.page(0)?, NOTE_RASTER, true)?;
        let mut png = Vec::new();
        s.write_to_png(&mut png).ok()?;
        Some(png)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    png
}

pub async fn note_image(md: String) -> Option<Vec<u8>> {
    gio::spawn_blocking(move || document_png(&md)).await.ok().flatten()
}

// ------------------------------------------------------------------------ Formel als Bild

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Formula {
        pub tex: RefCell<Option<gdk::Texture>>,
        /// sichtbare Breite, Höhe über der Grundlinie, ganze Bildhöhe
        pub size: Cell<(f64, f64, f64)>,
        pub depth: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Formula {
        const NAME: &'static str = "GraphitFormula";
        type Type = super::Formula;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Formula {}

    impl gtk::gdk::subclass::paintable::PaintableImpl for Formula {
        fn intrinsic_width(&self) -> i32 {
            self.size.get().0.ceil() as i32
        }
        // Das Textfeld stellt ein Bild auf die Grundlinie: gemeldet wird nur der Teil
        // darüber, der Rest wird unter die Grundlinie gezeichnet
        fn intrinsic_height(&self) -> i32 {
            self.size.get().1.ceil() as i32
        }
        fn snapshot(&self, snapshot: &gdk::Snapshot, _w: f64, h: f64) {
            let Some(s) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let Some(t) = &*self.tex.borrow() else { return };
            let (w, above, full) = self.size.get();
            let y = h - above;
            s.append_scaled_texture(t, gsk::ScalingFilter::Trilinear, &graphene::Rect::new(0.0, y as f32, w as f32, full as f32));
        }
    }
}

glib::wrapper! {
    pub struct Formula(ObjectSubclass<imp::Formula>) @implements gdk::Paintable;
}

impl Formula {
    fn new(raw: Raw) -> Formula {
        let f: Formula = glib::Object::new();
        let tex = gdk::MemoryTexture::new(raw.w, raw.h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &glib::Bytes::from_owned(raw.data), raw.stride);
        f.imp().tex.replace(Some(tex.upcast()));
        // 1 pt Rand rundum aus dem Setzen
        f.imp().size.set((raw.w as f64 / RASTER * SHOW, (raw.ht + 1.0) * SHOW, raw.h as f64 / RASTER * SHOW));
        f.imp().depth.set(raw.dp * SHOW);
        f
    }

    /// Wie weit die Formel unter die Grundlinie reicht.
    pub fn depth(&self) -> f64 {
        self.imp().depth.get()
    }

    /// Verkleinerte Fassung für schmale Spalten.
    fn fitted(&self, max_w: f64) -> Formula {
        let (w, above, full) = self.imp().size.get();
        if w <= max_w {
            return self.clone();
        }
        let k = max_w / w;
        let f: Formula = glib::Object::new();
        f.imp().tex.replace(self.imp().tex.borrow().clone());
        f.imp().size.set((max_w, above * k, full * k));
        f.imp().depth.set(self.depth() * k);
        f
    }
}

// ------------------------------------------------------------------------ Formel-Speicher

type Key = (String, bool);

enum Entry {
    Pending,
    Ready(Formula),
    Failed,
}

/// Merkt sich gesetzte Formeln und arbeitet neue im Hintergrund ab.
#[derive(Default)]
pub struct Engine {
    cache: RefCell<HashMap<Key, Entry>>,
    queue: RefCell<Vec<Key>>,
    busy: Cell<bool>,
    on_ready: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Engine {
    pub fn connect_ready(&self, f: Rc<dyn Fn()>) {
        self.on_ready.replace(Some(f));
    }

    /// Fertige Formel, sonst wird sie zum Setzen vorgemerkt.
    fn get(self: &Rc<Self>, tex: &str, display: bool) -> Option<Formula> {
        let key = (tex.to_string(), display);
        if let Some(e) = self.cache.borrow().get(&key) {
            return match e {
                Entry::Ready(f) => Some(f.clone()),
                _ => None,
            };
        }
        if !acceptable(tex) {
            self.cache.borrow_mut().insert(key, Entry::Failed);
            return None;
        }
        self.cache.borrow_mut().insert(key.clone(), Entry::Pending);
        self.queue.borrow_mut().push(key);
        self.start();
        None
    }

    fn start(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        let this = self.clone();
        // kurz warten: alle Formeln einer Antwort kommen so in einen Lauf
        glib::timeout_add_local_once(std::time::Duration::from_millis(30), move || {
            let items = this.queue.take();
            let job = items.clone();
            let worker = this.clone();
            glib::spawn_future_local(async move {
                let raws = gio::spawn_blocking(move || set_batch(&job, "FFFFFF")).await.unwrap_or_default();
                {
                    let mut cache = worker.cache.borrow_mut();
                    let mut raws = raws.into_iter();
                    for key in items {
                        let e = raws.next().flatten().map(|r| Entry::Ready(Formula::new(r))).unwrap_or(Entry::Failed);
                        cache.insert(key, e);
                    }
                }
                worker.busy.set(false);
                if !worker.queue.borrow().is_empty() {
                    worker.start();
                }
                let f = worker.on_ready.borrow().clone();
                if let Some(f) = f {
                    f();
                }
            });
        });
    }
}

// ------------------------------------------------------------------------ Textfeld

/// Fließtext mit Formeln: lesbar, auswählbar, Höhe folgt dem Inhalt.
#[derive(Clone)]
struct TextBlock {
    view: gtk::TextView,
    /// wartet noch auf Formeln
    pending: Rc<Cell<bool>>,
    max_w: Rc<Cell<f64>>,
    source: Rc<RefCell<String>>,
    bold: gtk::TextTag,
    code: gtk::TextTag,
    head: gtk::TextTag,
    raw: gtk::TextTag,
    center: gtk::TextTag,
    gaps: Rc<RefCell<HashMap<i32, gtk::TextTag>>>,
}

impl TextBlock {
    fn new(max_w: f64) -> TextBlock {
        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_hexpand(true);
        view.add_css_class("fk-claude-rich");
        // GTK misst ein neues Textfeld vor dem Zeilenumbruch mit Höhe 0 und meldet sich bei
        // einer einzelnen Zeile nicht von selbst: ohne neues Messen bliebe der Text unsichtbar.
        view.connect_map(|v| {
            let v = v.clone();
            glib::idle_add_local_once(move || v.queue_resize());
        });
        let buf = view.buffer();
        let table = buf.tag_table();
        let add = |t: gtk::TextTag| {
            table.add(&t);
            t
        };
        TextBlock {
            pending: Rc::new(Cell::new(false)),
            bold: add(gtk::TextTag::builder().weight(700).build()),
            code: add(gtk::TextTag::builder().family("monospace").scale(0.92).build()),
            head: add(gtk::TextTag::builder().weight(700).scale(1.12).pixels_above_lines(6).build()),
            raw: add(gtk::TextTag::builder().family("monospace").scale(0.86).foreground("#a9a7a2").build()),
            center: add(gtk::TextTag::builder().justification(gtk::Justification::Center).pixels_above_lines(6).pixels_below_lines(6).build()),
            view,
            max_w: Rc::new(Cell::new(max_w)),
            source: Rc::new(RefCell::new(String::new())),
            gaps: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Abstand unter einer Zeile, damit tiefe Formeln nicht in die nächste ragen.
    fn gap(&self, px: i32) -> gtk::TextTag {
        self.gaps
            .borrow_mut()
            .entry(px)
            .or_insert_with(|| {
                let t = gtk::TextTag::builder().pixels_below_lines(px).pixels_inside_wrap(px).build();
                self.view.buffer().tag_table().add(&t);
                t
            })
            .clone()
    }

    fn set(&self, md: &str, engine: &Rc<Engine>) {
        if *self.source.borrow() == md && !self.pending.get() {
            return;
        }
        self.source.replace(md.to_string());
        self.pending.set(false);
        let buf = self.view.buffer();
        buf.set_text("");
        let paras = parse(md);
        let last = paras.len().saturating_sub(1);
        for (n, p) in paras.iter().enumerate() {
            let start = buf.end_iter().offset();
            let mut depth: f64 = 0.0;
            for s in &p.segs {
                let mut end = buf.end_iter();
                match s {
                    Seg::Text { text, bold, code } => {
                        let mut tags = Vec::new();
                        if *bold {
                            tags.push(&self.bold);
                        }
                        if *code {
                            tags.push(&self.code);
                        }
                        if p.head {
                            tags.push(&self.head);
                        }
                        buf.insert_with_tags(&mut end, text, &tags);
                    }
                    Seg::Math { tex, display } => match engine.get(tex, *display) {
                        Some(f) => {
                            let f = f.fitted(self.max_w.get());
                            depth = depth.max(f.depth());
                            buf.insert_paintable(&mut end, &f);
                        }
                        None => {
                            self.pending.set(true);
                            buf.insert_with_tags(&mut end, tex, &[&self.raw]);
                        }
                    },
                }
            }
            let only_display = !p.segs.is_empty() && p.segs.iter().all(|s| matches!(s, Seg::Math { display: true, .. }));
            let (a, b) = (buf.iter_at_offset(start), buf.end_iter());
            if only_display {
                buf.apply_tag(&self.center, &a, &b);
            }
            if depth > 3.0 {
                buf.apply_tag(&self.gap(depth.ceil() as i32), &a, &b);
            }
            if n != last {
                buf.insert(&mut buf.end_iter(), "\n");
            }
        }
    }
}

/// Tabelle als Raster; Zellen sind eigene Textfelder, Formeln gehen also auch darin.
struct TableBlock {
    grid: gtk::Grid,
    cells: Vec<Vec<TextBlock>>,
}

impl TableBlock {
    fn new() -> TableBlock {
        let grid = gtk::Grid::new();
        grid.add_css_class("fk-claude-table");
        grid.set_overflow(gtk::Overflow::Hidden);
        grid.set_halign(gtk::Align::Start);
        TableBlock { grid, cells: Vec::new() }
    }

    fn set(&mut self, head: &[String], rows: &[Vec<String>], max_w: f64, engine: &Rc<Engine>) {
        let cols = column_weights(head, rows);
        let total: f64 = cols.iter().map(|c| c.0).sum();
        // kurze Tabellen bleiben schmal, breite teilen sich den Platz nach Inhalt
        let widths = column_widths(&cols, (total * 8.5 + 20.0 * head.len() as f64).min(max_w));
        let lines = std::iter::once(head).chain(rows.iter().map(|r| r.as_slice()));
        for (y, line) in lines.enumerate() {
            if self.cells.len() <= y {
                self.cells.push(Vec::new());
            }
            for (x, text) in line.iter().enumerate() {
                let w = widths[x];
                if self.cells[y].len() <= x {
                    let c = TextBlock::new(w);
                    c.view.add_css_class("fk-claude-cell");
                    if y == 0 {
                        c.view.add_css_class("fk-claude-th");
                    }
                    c.view.set_left_margin(9);
                    c.view.set_right_margin(9);
                    c.view.set_top_margin(5);
                    c.view.set_bottom_margin(5);
                    c.view.set_hexpand(false);
                    self.grid.attach(&c.view, x as i32, y as i32, 1, 1);
                    self.cells[y].push(c);
                }
                let c = &self.cells[y][x];
                c.max_w.set(w - 18.0);
                c.view.set_size_request(w as i32, -1);
                // Kopf fett, ohne das Markdown der Zelle anzufassen
                let t = if y == 0 && !text.is_empty() && !text.contains("**") { format!("**{text}**") } else { text.clone() };
                c.set(&t, engine);
            }
        }
    }
}

enum Child {
    Text(TextBlock),
    Table(TableBlock),
}

/// Antwort aus Fließtext und Tabellen.
#[derive(Clone)]
pub struct Rich {
    pub root: gtk::Box,
    max_w: f64,
    source: Rc<RefCell<String>>,
    children: Rc<RefCell<Vec<Child>>>,
}

impl Rich {
    pub fn new(max_w: f64) -> Rich {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_hexpand(true);
        Rich { root, max_w, source: Rc::new(RefCell::new(String::new())), children: Rc::new(RefCell::new(Vec::new())) }
    }

    pub fn source(&self) -> String {
        self.source.borrow().clone()
    }

    /// Vorhandene Blöcke bleiben stehen und werden nur geändert: beim Streamen flackert nichts.
    pub fn set(&self, md: &str, engine: &Rc<Engine>) {
        self.source.replace(md.to_string());
        let list = blocks(md);
        let mut children = self.children.borrow_mut();
        for (i, b) in list.iter().enumerate() {
            let fits = matches!((children.get(i), b), (Some(Child::Text(_)), Block::Text(_)) | (Some(Child::Table(_)), Block::Table { .. }));
            if !fits {
                for c in children.drain(i..) {
                    match c {
                        Child::Text(t) => self.root.remove(&t.view),
                        Child::Table(t) => self.root.remove(&t.grid),
                    }
                }
                let c = match b {
                    Block::Text(_) => {
                        let t = TextBlock::new(self.max_w);
                        self.root.append(&t.view);
                        Child::Text(t)
                    }
                    Block::Table { .. } => {
                        let t = TableBlock::new();
                        self.root.append(&t.grid);
                        Child::Table(t)
                    }
                };
                children.push(c);
            }
            match (&mut children[i], b) {
                (Child::Text(t), Block::Text(md)) => t.set(md, engine),
                (Child::Table(t), Block::Table { head, rows }) => t.set(head, rows, self.max_w, engine),
                _ => {}
            }
        }
        for c in children.drain(list.len()..) {
            match c {
                Child::Text(t) => self.root.remove(&t.view),
                Child::Table(t) => self.root.remove(&t.grid),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn math(tex: &str, display: bool) -> Seg {
        Seg::Math { tex: tex.into(), display }
    }
    fn plain(t: &str) -> Seg {
        Seg::Text { text: t.into(), bold: false, code: false }
    }

    #[test]
    fn finds_formulas() {
        assert_eq!(inline("a $x^2$ b"), [plain("a "), math("x^2", false), plain(" b")]);
        assert_eq!(inline(r"\(a\) und $$b$$"), [math("a", false), plain(" und "), math("b", true)]);
        assert_eq!(inline("kostet 5$ oder $ 6"), [plain("kostet 5$ oder $ 6")]);
        assert_eq!(inline(r"nur \$5"), [plain("nur $5")]);
        assert_eq!(inline("`$x$`"), [Seg::Text { text: "$x$".into(), bold: false, code: true }]);
        assert_eq!(inline("**fett $y$**")[0], Seg::Text { text: "fett ".into(), bold: true, code: false });
    }

    #[test]
    fn finds_tables() {
        let b = blocks("Davor\n\n| A | B |\n|---|:-:|\n| 1 | $x|y$ |\n| 2 |\n\nDanach");
        assert_eq!(b.len(), 3);
        let Block::Table { head, rows } = &b[1] else { panic!() };
        assert_eq!(head, &["A", "B"]);
        assert_eq!(rows[0], ["1", "$x|y$"]);
        assert_eq!(rows[1], ["2", ""]);
        assert_eq!(b[2], Block::Text("Danach".into()));
        // ohne Trennzeile noch keine Tabelle, im Code-Block nie
        assert!(matches!(&blocks("| A | B |")[..], [Block::Text(_)]));
        assert!(matches!(&blocks("```\n| A |\n|---|\n```")[..], [Block::Text(_)]));
    }

    #[test]
    fn table_words_fit() {
        let head = ["Größe".to_string(), "Symbol".to_string(), "Einheit".to_string()];
        let rows = vec![vec!["Spezifische Enthalpie".to_string(), "h".to_string(), "J/kg".to_string()]];
        let cols = column_weights(&head, &rows);
        let w = column_widths(&cols, 344.0);
        assert!((w.iter().sum::<f64>() - 344.0).abs() < 0.01);
        assert!(w[1] >= 6.0 * 9.0 + 20.0, "{w:?}");
        // zu wenig Platz: alles schrumpft gleichmäßig, die Tabelle bleibt in ihrer Breite
        assert!((column_widths(&cols, 100.0).iter().sum::<f64>() - 100.0).abs() < 0.01);
    }

    #[test]
    fn multiline_display() {
        let p = parse("Davor\n$$\na = b\n$$\nDanach");
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].segs, [math("a = b", true)]);
        // beim Streamen noch offen: bleibt Text
        assert_eq!(parse("$$\na = b").len(), 2);
    }

    #[test]
    fn answer_as_note_image() {
        if Command::new("pandoc").arg("--version").stdout(Stdio::null()).status().is_err() {
            return;
        }
        let png = document_png("**Größe:** H₂ und Δ $x^2$\n\n$$\\frac{a}{b}$$\n\n- Punkt\n\n| Stoff | 50 % |\n|---|---|\n| H₂O | $k_1$ |").expect("Bild");
        assert!(png.starts_with(b"\x89PNG") && png.len() > 1000);
    }

    #[test]
    fn rejects_broken_or_risky() {
        assert!(acceptable(r"\frac{a}{b}"));
        assert!(acceptable(r"\{ a \}"));
        assert!(!acceptable(r"\frac{a"));
        assert!(!acceptable(r"a}"));
        assert!(!acceptable(r"\input{/etc/passwd}"));
    }

    #[test]
    fn sets_formulas() {
        if Command::new("pdflatex").arg("--version").stdout(Stdio::null()).status().is_err() {
            return;
        }
        let items = vec![(r"\frac{\partial L}{\partial \dot q_i}".to_string(), true), (r"x_i^2".to_string(), false)];
        let r = set_batch(&items, "000000");
        let a = r[0].as_ref().expect("Bruch");
        assert!(a.dp > 3.0 && a.ht > 5.0, "{} {}", a.ht, a.dp);
        assert!(r[1].as_ref().expect("Index").w > 10);
        // eine kaputte Formel nimmt die gute nicht mit
        let r = set_batch(&[(r"\nichtda{x}".to_string(), false), ("y".to_string(), false)], "000000");
        assert!(r[0].is_none() && r[1].is_some());
    }
}
