//! Grundlegende Geometrie in Dokument-Koordinaten (Punkte, 1/72 Zoll).

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    pub const fn new(x: f64, y: f64) -> Self {
        Pt { x, y }
    }
    pub fn dist(self, o: Pt) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }
    pub fn lerp(self, o: Pt, t: f64) -> Pt {
        Pt::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect { x0, y0, x1, y1 }
    }
    pub fn from_pts(a: Pt, b: Pt) -> Self {
        Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }
    pub fn bounding(pts: &[Pt]) -> Option<Rect> {
        let first = pts.first()?;
        let mut r = Rect::new(first.x, first.y, first.x, first.y);
        for p in &pts[1..] {
            r.x0 = r.x0.min(p.x);
            r.y0 = r.y0.min(p.y);
            r.x1 = r.x1.max(p.x);
            r.y1 = r.y1.max(p.y);
        }
        Some(r)
    }
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    pub fn center(&self) -> Pt {
        Pt::new((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
    pub fn union_opt(a: Option<Rect>, b: Rect) -> Rect {
        match a {
            Some(a) => a.union(&b),
            None => b,
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !(self.x1 < o.x0 || o.x1 < self.x0 || self.y1 < o.y0 || o.y1 < self.y0)
    }
    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }
    pub fn expand(&self, d: f64) -> Rect {
        Rect::new(self.x0 - d, self.y0 - d, self.x1 + d, self.y1 + d)
    }
    pub fn translate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
}

/// Uniforme Skalierung plus Verschiebung: p' = p * s + t.
/// (Xournal-Elemente wie Text und Bilder können nicht rotiert werden,
/// daher reicht das für alle Auswahl-Operationen.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform {
    /// Ähnlichkeitsabbildung: x' = a·x − b·y + tx, y' = b·x + a·y + ty
    /// (a = s·cos θ, b = s·sin θ)
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Xform {
    pub const IDENTITY: Xform = Xform { a: 1.0, b: 0.0, tx: 0.0, ty: 0.0 };

    pub fn translate(dx: f64, dy: f64) -> Xform {
        Xform { a: 1.0, b: 0.0, tx: dx, ty: dy }
    }
    /// Skalierung um einen Fixpunkt.
    pub fn scale_about(s: f64, o: Pt) -> Xform {
        Xform { a: s, b: 0.0, tx: o.x - o.x * s, ty: o.y - o.y * s }
    }
    /// Drehung um einen Fixpunkt (Winkel im Bogenmaß, im Uhrzeigersinn auf dem Bildschirm).
    pub fn rotate_about(angle: f64, o: Pt) -> Xform {
        let (b, a) = angle.sin_cos();
        Xform { a, b, tx: o.x - (a * o.x - b * o.y), ty: o.y - (b * o.x + a * o.y) }
    }
    /// Erst `self`, dann `o`.
    pub fn then(&self, o: &Xform) -> Xform {
        Xform {
            a: o.a * self.a - o.b * self.b,
            b: o.b * self.a + o.a * self.b,
            tx: o.a * self.tx - o.b * self.ty + o.tx,
            ty: o.b * self.tx + o.a * self.ty + o.ty,
        }
    }
    pub fn inverse(&self) -> Xform {
        let d = self.a * self.a + self.b * self.b;
        let (a, b) = (self.a / d, -self.b / d);
        Xform { a, b, tx: -(a * self.tx - b * self.ty), ty: -(b * self.tx + a * self.ty) }
    }
    /// Skalierungsfaktor
    pub fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }
    /// Drehwinkel im Bogenmaß
    pub fn angle(&self) -> f64 {
        self.b.atan2(self.a)
    }
    pub fn apply(&self, p: Pt) -> Pt {
        Pt::new(self.a * p.x - self.b * p.y + self.tx, self.b * p.x + self.a * p.y + self.ty)
    }
    /// Umschließendes Rechteck des abgebildeten Rechtecks.
    pub fn apply_rect(&self, r: &Rect) -> Rect {
        let c = self.corners(r);
        Rect::from_pts(c[0], c[2]).union(&Rect::from_pts(c[1], c[3]))
    }
    /// Abgebildete Ecken (oben links, oben rechts, unten rechts, unten links).
    pub fn corners(&self, r: &Rect) -> [Pt; 4] {
        [Pt::new(r.x0, r.y0), Pt::new(r.x1, r.y0), Pt::new(r.x1, r.y1), Pt::new(r.x0, r.y1)].map(|p| self.apply(p))
    }
    /// Rechteck, das aufrecht bleibt: Mitte wird abgebildet, Größe nur skaliert
    /// (für Text und Bilder, die sich im .xopp-Format nicht drehen lassen).
    pub fn apply_rect_upright(&self, r: &Rect) -> Rect {
        let c = self.apply(r.center());
        let s = self.scale();
        let (hw, hh) = ((r.x1 - r.x0) * s / 2.0, (r.y1 - r.y0) * s / 2.0);
        Rect::new(c.x - hw, c.y - hh, c.x + hw, c.y + hh)
    }
    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < 1e-9 && self.b.abs() < 1e-9 && self.tx.abs() < 1e-9 && self.ty.abs() < 1e-9
    }
}

pub fn dist_point_segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0);
    p.dist(Pt::new(a.x + t * dx, a.y + t * dy))
}

#[allow(dead_code)] // kommt mit dem Lasso (Etappe 2)
pub fn point_in_polygon(p: Pt, poly: &[Pt]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (pi, pj) = (poly[i], poly[j]);
        if (pi.y > p.y) != (pj.y > p.y)
            && p.x < (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y + 1e-12) + pi.x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);

    pub const fn rgb(r: f64, g: f64, b: f64) -> Color {
        Color { r, g, b, a: 1.0 }
    }
    pub fn with_alpha(self, a: f64) -> Color {
        Color { a, ..self }
    }
    pub fn from_hex(s: &str) -> Option<Color> {
        let s = match s.trim() {
            "black" => "#000000ff",
            "blue" => "#3333ccff",
            "red" => "#ff0000ff",
            "green" => "#008000ff",
            "gray" => "#808080ff",
            "lightblue" => "#00c0ffff",
            "lightgreen" => "#00ff00ff",
            "magenta" => "#ff00ffff",
            "orange" => "#ff8000ff",
            "yellow" => "#ffff00ff",
            "white" => "#ffffffff",
            other => other,
        };
        let h = s.strip_prefix('#')?;
        let v = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| v as f64 / 255.0);
        match h.len() {
            6 => Some(Color { r: v(0)?, g: v(2)?, b: v(4)?, a: 1.0 }),
            8 => Some(Color { r: v(0)?, g: v(2)?, b: v(4)?, a: v(6)? }),
            _ => None,
        }
    }
    pub fn to_hex(&self) -> String {
        let c = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        format!("#{:02x}{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b), c(self.a))
    }
    pub fn to_gdk(&self) -> gtk::gdk::RGBA {
        gtk::gdk::RGBA::new(self.r as f32, self.g as f32, self.b as f32, self.a as f32)
    }
    pub fn from_gdk(c: &gtk::gdk::RGBA) -> Color {
        Color { r: c.red() as f64, g: c.green() as f64, b: c.blue() as f64, a: c.alpha() as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Pt, b: Pt) -> bool {
        a.dist(b) < 1e-9
    }

    #[test]
    fn xform_rotate_compose_inverse() {
        let c = Pt::new(10.0, 20.0);
        let r = Xform::rotate_about(std::f64::consts::FRAC_PI_2, c);
        // 90° im Uhrzeigersinn (y nach unten): rechts vom Zentrum → darunter
        assert!(close(r.apply(Pt::new(15.0, 20.0)), Pt::new(10.0, 25.0)));
        assert!(close(r.apply(c), c));
        let x = Xform::translate(3.0, -2.0).then(&Xform::scale_about(2.0, Pt::new(1.0, 1.0))).then(&r);
        let p = Pt::new(-4.0, 7.5);
        assert!(close(x.inverse().apply(x.apply(p)), p));
        assert!((x.scale() - 2.0).abs() < 1e-9);
        assert!((x.angle() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // erst a, dann b
        let a = Xform::translate(1.0, 0.0);
        let b = Xform::scale_about(3.0, Pt::new(0.0, 0.0));
        assert!(close(a.then(&b).apply(Pt::new(1.0, 1.0)), Pt::new(6.0, 3.0)));
    }

    #[test]
    fn upright_rect_keeps_size() {
        let r = Rect::new(0.0, 0.0, 4.0, 2.0);
        let x = Xform::rotate_about(0.7, Pt::new(10.0, 10.0));
        let u = x.apply_rect_upright(&r);
        assert!((u.x1 - u.x0 - 4.0).abs() < 1e-9 && (u.y1 - u.y0 - 2.0).abs() < 1e-9);
        assert!(close(u.center(), x.apply(r.center())));
        // ohne Drehung identisch mit apply_rect
        let s = Xform::scale_about(1.5, Pt::new(2.0, 3.0)).then(&Xform::translate(5.0, 1.0));
        assert_eq!(s.apply_rect_upright(&r), s.apply_rect(&r));
    }
}
