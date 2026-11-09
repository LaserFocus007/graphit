//! Minimaler, toleranter XML-Parser und -Writer. Das .xopp-Format nutzt nur einen
//! kleinen Teil von XML; ein eigener Parser hält Abhängigkeiten und Überraschungen klein.

#[derive(Clone, Debug, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// Zusammengefasster Textinhalt (xopp nutzt keinen gemischten Inhalt).
    pub text: String,
}

impl Element {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    pub fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        write_attrs(out, &self.attrs);
        if self.children.is_empty() && self.text.is_empty() {
            out.push_str("/>\n");
            return;
        }
        out.push('>');
        escape_into(out, &self.text);
        if !self.children.is_empty() {
            out.push('\n');
        }
        for c in &self.children {
            c.write(out);
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push_str(">\n");
    }
}

pub fn write_attrs(out: &mut String, attrs: &[(String, String)]) {
    for (k, v) in attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_into(out, v);
        out.push('"');
    }
}

pub fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push('\n'),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let ent = &rest[1..end];
        let rep = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match rep {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn parse(src: &str) -> Result<Element, String> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<Element> = vec![Element { name: "#root".into(), ..Default::default() }];

    while i < b.len() {
        if b[i] != b'<' {
            let end = src[i..].find('<').map(|n| i + n).unwrap_or(b.len());
            let txt = &src[i..end];
            if !txt.trim().is_empty() {
                stack.last_mut().unwrap().text.push_str(&unescape(txt));
            }
            i = end;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<?") {
            i += rest.find("?>").ok_or(crate::i18n::tl("unterminierte Verarbeitungsanweisung"))? + 2;
        } else if rest.starts_with("<!--") {
            i += rest.find("-->").ok_or(crate::i18n::tl("unterminierter Kommentar"))? + 3;
        } else if rest.starts_with("<![CDATA[") {
            let e = rest.find("]]>").ok_or(crate::i18n::tl("unterminiertes CDATA"))?;
            stack.last_mut().unwrap().text.push_str(&rest[9..e]);
            i += e + 3;
        } else if rest.starts_with("<!") {
            i += rest.find('>').ok_or(crate::i18n::tl("unterminierte Deklaration"))? + 1;
        } else if rest.starts_with("</") {
            let e = rest.find('>').ok_or(crate::i18n::tl("unterminiertes End-Tag"))?;
            i += e + 1;
            if stack.len() > 1 {
                let el = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(el);
            }
        } else {
            // Start-Tag
            let mut j = i + 1;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let mut el = Element { name: src[i + 1..j].to_string(), ..Default::default() };
            let mut self_closing = false;
            loop {
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= b.len() {
                    return Err(crate::i18n::tl("unerwartetes Dateiende im Tag").into());
                }
                if b[j] == b'>' {
                    j += 1;
                    break;
                }
                if b[j] == b'/' {
                    self_closing = true;
                    j += 1;
                    continue;
                }
                let ns = j;
                while j < b.len() && b[j] != b'=' && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                    j += 1;
                }
                let name = src[ns..j].to_string();
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == b'=' {
                    j += 1;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let q = *b.get(j).ok_or(crate::i18n::tl("unerwartetes Dateiende"))?;
                    if q == b'"' || q == b'\'' {
                        let vs = j + 1;
                        let ve = src[vs..].find(q as char).ok_or(crate::i18n::tl("unterminierter Attributwert"))? + vs;
                        el.attrs.push((name, unescape(&src[vs..ve])));
                        j = ve + 1;
                    } else {
                        let vs = j;
                        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                            j += 1;
                        }
                        el.attrs.push((name, unescape(&src[vs..j])));
                    }
                } else {
                    el.attrs.push((name, String::new()));
                }
            }
            i = j;
            if self_closing {
                stack.last_mut().unwrap().children.push(el);
            } else {
                stack.push(el);
            }
        }
    }
    while stack.len() > 1 {
        let el = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(el);
    }
    let root = stack.pop().unwrap();
    root.children.into_iter().next().ok_or_else(|| crate::i18n::tl("leeres Dokument").to_string())
}
