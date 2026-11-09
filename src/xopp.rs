//! Lesen und Schreiben des Xournal++-Formats (.xopp = gzip-komprimiertes XML).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::geom::{Color, Pt, Rect};
use crate::model::*;
use crate::xml::{self, escape_into, write_attrs, Element};

pub fn read_maybe_gz(path: &Path) -> std::io::Result<String> {
    let raw = fs::read(path)?;
    let mut s = String::new();
    if raw.starts_with(&[0x1f, 0x8b]) {
        GzDecoder::new(&raw[..]).read_to_string(&mut s)?;
    } else {
        s = String::from_utf8_lossy(&raw).into_owned();
    }
    Ok(s)
}

fn floats(s: &str) -> Vec<f64> {
    s.split_ascii_whitespace().filter_map(|v| v.parse().ok()).collect()
}

fn take(attrs: &mut Attrs, k: &str) -> Option<String> {
    let i = attrs.iter().position(|(n, _)| n == k)?;
    Some(attrs.remove(i).1)
}

fn parse_stroke(e: &Element) -> Option<Item> {
    let mut a = e.attrs.clone();
    let tool = StrokeTool::parse(&take(&mut a, "tool").unwrap_or_default());
    let mut color = take(&mut a, "color").and_then(|c| Color::from_hex(&c)).unwrap_or(Color::BLACK);
    let wl = floats(&take(&mut a, "width").unwrap_or_else(|| "1".into()));
    let nums = floats(&e.text);
    let pts: Vec<Pt> = nums.chunks_exact(2).map(|c| Pt::new(c[0], c[1])).collect();
    if pts.is_empty() {
        return None;
    }
    let width = wl.first().copied().unwrap_or(1.0);
    let rest = &wl[wl.len().min(1)..];
    let n = pts.len();
    // Xournal speichert n-1 Segmentbreiten; manche Versionen n Punktbreiten.
    let widths = if rest.is_empty() {
        None
    } else if rest.len() + 1 == n {
        let mut w = rest.to_vec();
        w.push(*rest.last().unwrap());
        Some(w)
    } else if rest.len() >= n {
        Some(rest[..n].to_vec())
    } else {
        None
    };
    if tool == StrokeTool::Highlighter && color.a >= 1.0 {
        color.a = 0.5;
    }
    Some(Item::Stroke(Stroke::new(tool, color, width, pts, widths, a)))
}

fn parse_text(e: &Element) -> Item {
    let mut a = e.attrs.clone();
    let num = |a: &mut Attrs, k: &str, d: f64| take(a, k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let x = num(&mut a, "x", 0.0);
    let y = num(&mut a, "y", 0.0);
    let size = num(&mut a, "size", 12.0);
    let font = take(&mut a, "font").unwrap_or_else(|| "Sans".into());
    let color = take(&mut a, "color").and_then(|c| Color::from_hex(&c)).unwrap_or(Color::BLACK);
    Item::Text(TextItem::new(x, y, e.text.clone(), font, size, color, a))
}

fn rect_attrs(a: &mut Attrs) -> Option<Rect> {
    let mut v = [0.0f64; 4];
    for (i, k) in ["left", "top", "right", "bottom"].iter().enumerate() {
        v[i] = take(a, k)?.parse().ok()?;
    }
    Some(Rect::new(v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])))
}

fn parse_raw(e: &Element) -> Item {
    let mut a = e.attrs.clone();
    let bbox = rect_attrs(&mut a);
    Item::Raw(RawItem { elem: e.clone(), bbox, decoded: Default::default() })
}

fn parse_image(e: &Element) -> Item {
    let mut a = e.attrs.clone();
    match rect_attrs(&mut a) {
        Some(r) => Item::Image(ImageItem::new(r, Arc::new(e.text.trim().to_string()), a)),
        None => parse_raw(e),
    }
}

fn resolve_pdf(fname: &str, domain: &str, xopp: &Path) -> PathBuf {
    let base = xopp.parent().unwrap_or(Path::new("."));
    let mut cands = Vec::new();
    if domain == "attach" {
        let mut s = xopp.as_os_str().to_owned();
        s.push(".");
        s.push(fname);
        cands.push(PathBuf::from(s));
    }
    let p = Path::new(fname);
    cands.push(if p.is_absolute() { p.to_path_buf() } else { base.join(p) });
    if let Some(name) = p.file_name() {
        cands.push(base.join(name));
    }
    cands.iter().find(|c| c.exists()).cloned().unwrap_or_else(|| cands[0].clone())
}

pub fn load(path: &Path) -> Result<Document, String> {
    let src = read_maybe_gz(path).map_err(|e| e.to_string())?;
    let root = xml::parse(&src)?;
    if root.name != "xournal" {
        return Err("Keine Xournal-Datei".into());
    }
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut doc = Document::new_blank("plain");
    doc.pages.clear();
    doc.path = Some(path.clone());
    doc.root_attrs = root.attrs.clone();
    for child in &root.children {
        match child.name.as_str() {
            "title" => doc.title = child.text.clone(),
            "page" => {
                let w = child.attr("width").and_then(|v| v.parse().ok()).unwrap_or(595.276);
                let h = child.attr("height").and_then(|v| v.parse().ok()).unwrap_or(841.89);
                let mut page = Page::new(w, h, Background::solid("plain"));
                let mut layers = Vec::new();
                for pe in &child.children {
                    match pe.name.as_str() {
                        "background" => {
                            let mut a = pe.attrs.clone();
                            if get_attr(&a, "type") == Some("pdf") {
                                let fname = take(&mut a, "filename");
                                let domain = take(&mut a, "domain").unwrap_or_else(|| "absolute".into());
                                if let (Some(f), None) = (fname, &doc.pdf_path) {
                                    if !f.is_empty() {
                                        doc.pdf_path = Some(resolve_pdf(&f, &domain, &path));
                                    }
                                }
                            }
                            page.bg = Background { attrs: a };
                        }
                        "layer" => {
                            let items: Vec<ItemRef> = pe
                                .children
                                .iter()
                                .filter_map(|le| match le.name.as_str() {
                                    "stroke" => parse_stroke(le),
                                    "text" => Some(parse_text(le)),
                                    "image" => Some(parse_image(le)),
                                    _ => Some(parse_raw(le)),
                                })
                                .map(Arc::new)
                                .collect();
                            layers.push(Layer { items: Arc::new(items), attrs: pe.attrs.clone() });
                        }
                        _ => {}
                    }
                }
                if !layers.is_empty() {
                    page.layers = layers;
                }
                doc.pages.push(page.into_ref());
            }
            _ => {}
        }
    }
    if doc.pages.is_empty() {
        doc.pages.push(Page::new(595.276, 841.89, Background::solid("plain")).into_ref());
    }
    Ok(doc)
}

// ------------------------------------------------------------------------ Speichern

/// Thread-sichere Momentaufnahme zum Speichern im Hintergrund.
pub struct SaveData {
    pages: Vec<(f64, f64, Attrs, Vec<(Attrs, ItemList)>)>,
    pdf_path: Option<PathBuf>,
    root_attrs: Attrs,
    title: String,
}

impl SaveData {
    pub fn from_doc(doc: &Document) -> SaveData {
        SaveData {
            pages: doc
                .pages
                .iter()
                .map(|p| {
                    let p = p.borrow();
                    (p.width, p.height, p.bg.attrs.clone(), p.layers.iter().map(|l| (l.attrs.clone(), l.items.clone())).collect())
                })
                .collect(),
            pdf_path: doc.pdf_path.clone(),
            root_attrs: doc.root_attrs.clone(),
            title: doc.title.clone(),
        }
    }
}

fn write_item(out: &mut String, it: &Item) {
    use std::fmt::Write as _;
    match it {
        Item::Stroke(s) => {
            let mut w = fmt_num(s.width);
            if let Some(ws) = &s.widths {
                if s.pts.len() > 1 {
                    for v in &ws[..ws.len() - 1] {
                        w.push(' ');
                        w.push_str(&fmt_num(*v));
                    }
                }
            }
            let _ = write!(out, "<stroke tool=\"{}\" color=\"{}\" width=\"{}\"", s.tool.as_str(), s.color.to_hex(), w);
            write_attrs(out, &s.attrs);
            out.push('>');
            for (i, p) in s.pts.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&fmt_num(p.x));
                out.push(' ');
                out.push_str(&fmt_num(p.y));
            }
            out.push_str("</stroke>\n");
        }
        Item::Text(t) => {
            out.push_str("<text font=\"");
            escape_into(out, &t.font);
            let _ = write!(
                out,
                "\" size=\"{}\" x=\"{}\" y=\"{}\" color=\"{}\"",
                fmt_num(t.size),
                fmt_num(t.x),
                fmt_num(t.y),
                t.color.to_hex()
            );
            write_attrs(out, &t.attrs);
            out.push('>');
            escape_into(out, &t.text);
            out.push_str("</text>\n");
        }
        Item::Image(i) => {
            let r = i.rect;
            let _ = write!(
                out,
                "<image left=\"{}\" top=\"{}\" right=\"{}\" bottom=\"{}\"",
                fmt_num(r.x0),
                fmt_num(r.y0),
                fmt_num(r.x1),
                fmt_num(r.y1)
            );
            write_attrs(out, &i.attrs);
            out.push('>');
            out.push_str(&i.data);
            out.push_str("</image>\n");
        }
        Item::Raw(r) => r.elem.write(out),
    }
}

pub fn serialize(d: &SaveData, target: &Path) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(1 << 20);
    out.push_str("<?xml version=\"1.0\" standalone=\"no\"?>\n");
    let mut ra: Attrs = d.root_attrs.iter().filter(|(k, _)| k != "creator").cloned().collect();
    ra.insert(0, ("creator".into(), "graphit 0.1".into()));
    if get_attr(&ra, "fileversion").is_none() {
        ra.push(("fileversion".into(), "4".into()));
    }
    out.push_str("<xournal");
    write_attrs(&mut out, &ra);
    out.push_str(">\n<title>");
    escape_into(&mut out, &d.title);
    out.push_str("</title>\n");
    let base = target.parent().unwrap_or(Path::new("."));
    let mut pdf_written = false;
    for (w, h, bg, layers) in &d.pages {
        let _ = writeln!(out, "<page width=\"{}\" height=\"{}\">", fmt_num(*w), fmt_num(*h));
        let mut bga = bg.clone();
        if get_attr(&bga, "type") == Some("pdf") && !pdf_written {
            if let Some(pdf) = &d.pdf_path {
                // relativ, wenn das PDF neben/unter der .xopp liegt (wie Xournal++ 1.2+)
                let fname = pdf.strip_prefix(base).map(|p| p.to_path_buf()).unwrap_or_else(|_| pdf.clone());
                bga.insert(1, ("domain".into(), "absolute".into()));
                bga.insert(2, ("filename".into(), fname.to_string_lossy().into_owned()));
                pdf_written = true;
            }
        }
        out.push_str("<background");
        write_attrs(&mut out, &bga);
        out.push_str("/>\n");
        for (la, items) in layers {
            out.push_str("<layer");
            write_attrs(&mut out, la);
            if items.is_empty() {
                out.push_str("/>\n");
                continue;
            }
            out.push_str(">\n");
            for it in items.iter() {
                write_item(&mut out, it);
            }
            out.push_str("</layer>\n");
        }
        out.push_str("</page>\n");
    }
    out.push_str("</xournal>\n");
    out
}

/// Atomisch speichern: Temp-Datei schreiben, fsync, umbenennen.
/// Ein Absturz mitten im Speichern hinterlässt nie eine kaputte Datei.
pub fn save(d: &SaveData, path: &Path) -> std::io::Result<()> {
    let xml = serialize(d, path);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".graphit-tmp");
    let tmp = PathBuf::from(tmp);
    {
        let f = fs::File::create(&tmp)?;
        let mut enc = GzEncoder::new(std::io::BufWriter::new(f), flate2::Compression::new(5));
        enc.write_all(xml.as_bytes())?;
        let w = enc.finish()?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    fs::rename(&tmp, path)
}
