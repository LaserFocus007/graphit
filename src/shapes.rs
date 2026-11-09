//! Formen (als normale Xournal-Striche gespeichert) und Formerkennung.

use std::f64::consts::{PI, TAU};

use crate::geom::{dist_point_segment, Pt, Rect};
use crate::tools::ShapeKind;

/// Erzeugt die Punktlisten (eine pro Strich) für eine Form von `a` nach `b`.
pub fn build(kind: ShapeKind, a: Pt, b: Pt, width: f64) -> Vec<Vec<Pt>> {
    let r = Rect::from_pts(a, b);
    match kind {
        ShapeKind::Line => vec![vec![a, b]],
        ShapeKind::Arrow => vec![arrow(a, b, width, false)],
        ShapeKind::DoubleArrow => vec![arrow(a, b, width, true)],
        ShapeKind::Rect => vec![rect_pts(&r)],
        ShapeKind::Ellipse => vec![ellipse_pts(r.center(), r.width() / 2.0, r.height() / 2.0)],
        ShapeKind::Axes => {
            // Ursprung links unten, Pfeile nach oben und rechts
            let o = Pt::new(r.x0, r.y1);
            vec![arrow(o, Pt::new(r.x0, r.y0), width, false), arrow(o, Pt::new(r.x1, r.y1), width, false)]
        }
    }
}

/// Beim Zeichnen von Formen: Linien rasten auf 15°-Schritte, fast-Quadrate/Kreise werden exakt.
pub fn constrain(kind: ShapeKind, a: Pt, b: Pt) -> Pt {
    match kind {
        ShapeKind::Line | ShapeKind::Arrow | ShapeKind::DoubleArrow => snap_angle(a, b),
        ShapeKind::Rect | ShapeKind::Ellipse => {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let (ax, ay) = (dx.abs(), dy.abs());
            if ax > 1.0 && ay > 1.0 && (ax - ay).abs() / ax.max(ay) < 0.08 {
                let m = (ax + ay) / 2.0;
                Pt::new(a.x + m * dx.signum(), a.y + m * dy.signum())
            } else {
                b
            }
        }
        ShapeKind::Axes => b,
    }
}

pub fn snap_angle(a: Pt, b: Pt) -> Pt {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len < 1e-6 {
        return b;
    }
    let ang = dy.atan2(dx);
    let step = PI / 12.0;
    let snapped = (ang / step).round() * step;
    if (ang - snapped).abs() < 4.0_f64.to_radians() {
        Pt::new(a.x + len * snapped.cos(), a.y + len * snapped.sin())
    } else {
        b
    }
}

fn arrow(a: Pt, b: Pt, width: f64, double: bool) -> Vec<Pt> {
    let len = a.dist(b).max(1e-6);
    let head = (len * 0.25).min(6.0 + width * 3.0).max(3.0);
    let heads = |from: Pt, tip: Pt| {
        let ang = (tip.y - from.y).atan2(tip.x - from.x);
        let spread = 25.0_f64.to_radians();
        (
            Pt::new(tip.x - head * (ang - spread).cos(), tip.y - head * (ang - spread).sin()),
            Pt::new(tip.x - head * (ang + spread).cos(), tip.y - head * (ang + spread).sin()),
        )
    };
    let (h1, h2) = heads(a, b);
    if double {
        let (g1, g2) = heads(b, a);
        vec![g1, a, g2, a, b, h1, b, h2]
    } else {
        vec![a, b, h1, b, h2]
    }
}

fn rect_pts(r: &Rect) -> Vec<Pt> {
    vec![Pt::new(r.x0, r.y0), Pt::new(r.x1, r.y0), Pt::new(r.x1, r.y1), Pt::new(r.x0, r.y1), Pt::new(r.x0, r.y0)]
}

pub fn ellipse_pts(c: Pt, rx: f64, ry: f64) -> Vec<Pt> {
    let n = ((rx.max(ry) * 0.8) as usize).clamp(36, 160);
    (0..=n).map(|i| {
        let t = i as f64 / n as f64 * TAU;
        Pt::new(c.x + rx * t.cos(), c.y + ry * t.sin())
    }).collect()
}

// ------------------------------------------------------------------------ Erkennung

/// Erkennt Linie, Ellipse/Kreis, Rechteck, Dreieck und Viereck in einem Freihandstrich.
/// Liefert die Punkte der sauberen Form oder `None`.
pub fn recognize(pts: &[Pt], force_line: bool) -> Option<Vec<Pt>> {
    if pts.len() < 2 {
        return None;
    }
    let len: f64 = pts.windows(2).map(|w| w[0].dist(w[1])).sum();
    let bb = Rect::bounding(pts)?;
    let diag = bb.width().hypot(bb.height());
    if diag < 4.0 {
        return None;
    }
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let closed = first.dist(last) < 0.18 * len.max(1.0) && len > 1.8 * diag;

    if !closed {
        let chord = first.dist(last);
        let dev = pts.iter().map(|p| dist_point_segment(*p, first, last)).fold(0.0, f64::max);
        if force_line || (chord > 0.0 && dev / chord < 0.06) {
            return Some(vec![first, snap_angle(first, last)]);
        }
        return None;
    }

    let c = bb.center();
    let (rx, ry) = (bb.width() / 2.0, bb.height() / 2.0);
    let n = pts.len() as f64;
    // Ellipse: radialer Fehler zur Ellipse durch die Bounding-Box
    let ell_err = if rx > 1.0 && ry > 1.0 {
        pts.iter().map(|p| (((p.x - c.x) / rx).hypot((p.y - c.y) / ry) - 1.0).abs()).sum::<f64>() / n
    } else {
        f64::MAX
    };
    // Rechteck (achsenparallel, auch mit abgerundeten Ecken): Abstand zur nächsten
    // Kante der Bounding-Box, im gleichen Maßstab wie der Ellipsenfehler
    let half = rx.min(ry).max(1.0);
    let rect_err = pts
        .iter()
        .map(|p| (p.x - bb.x0).abs().min((p.x - bb.x1).abs()).min((p.y - bb.y0).abs()).min((p.y - bb.y1).abs()))
        .sum::<f64>()
        / n
        / half;
    let sides_covered = {
        // jede Seite muss über mindestens die Hälfte ihrer Länge "begangen" sein
        let tol = 0.2 * half;
        let cover = |near: &dyn Fn(&Pt) -> bool, coord: &dyn Fn(&Pt) -> f64, len: f64| {
            let v: Vec<f64> = pts.iter().filter(|p| near(p)).map(coord).collect();
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |a, x| (a.0.min(*x), a.1.max(*x)));
            !v.is_empty() && (hi - lo) > 0.5 * len
        };
        cover(&|p| (p.y - bb.y0).abs() < tol, &|p| p.x, bb.width())
            && cover(&|p| (p.y - bb.y1).abs() < tol, &|p| p.x, bb.width())
            && cover(&|p| (p.x - bb.x0).abs() < tol, &|p| p.y, bb.height())
            && cover(&|p| (p.x - bb.x1).abs() < tol, &|p| p.y, bb.height())
    };
    let rect_ok = sides_covered && rect_err < 0.11 && rect_err < ell_err * 0.85;
    let ell_ok = ell_err < 0.14 && ell_err <= rect_err;
    if rect_ok {
        return Some(rect_pts(&bb));
    }
    if ell_ok {
        let (rx, ry) = if (rx - ry).abs() / rx.max(ry) < 0.12 { ((rx + ry) / 2.0, (rx + ry) / 2.0) } else { (rx, ry) };
        return Some(ellipse_pts(c, rx, ry));
    }
    // sonst: Dreieck / gedrehtes Viereck über Douglas-Peucker
    let mut closed_pts = pts.to_vec();
    closed_pts.push(first);
    let simple = douglas_peucker(&closed_pts, 0.07 * diag);
    let mut corners: Vec<Pt> = simple[..simple.len().saturating_sub(1)].to_vec();
    merge_close(&mut corners, 0.12 * diag);
    let poly_ok = (3..=4).contains(&corners.len()) && {
        let mut ring = corners.clone();
        ring.push(corners[0]);
        let err = pts
            .iter()
            .map(|p| ring.windows(2).map(|w| dist_point_segment(*p, w[0], w[1])).fold(f64::MAX, f64::min))
            .sum::<f64>()
            / n;
        err / diag < 0.035
    };
    if poly_ok {
        if corners.len() == 4 && axis_aligned(&corners) {
            return Some(rect_pts(&bb));
        }
        let mut out = corners.clone();
        out.push(corners[0]);
        return Some(out);
    }
    None
}

fn axis_aligned(c: &[Pt]) -> bool {
    (0..4).all(|i| {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        let ang = (b.y - a.y).atan2(b.x - a.x).rem_euclid(PI / 2.0);
        ang < 10.0_f64.to_radians() || ang > (90.0 - 10.0_f64).to_radians()
    })
}

fn merge_close(c: &mut Vec<Pt>, d: f64) {
    let mut i = 0;
    while c.len() > 2 && i < c.len() {
        let j = (i + 1) % c.len();
        if c[i].dist(c[j]) < d {
            let m = c[i].lerp(c[j], 0.5);
            c[i] = m;
            c.remove(j);
        } else {
            i += 1;
        }
    }
}

fn douglas_peucker(pts: &[Pt], eps: f64) -> Vec<Pt> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (mut idx, mut dmax) = (0, 0.0);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        // geschlossene Kurve: a == b, dann Abstand zum Punkt statt zur Strecke
        let d = if a.dist(b) < 1e-9 { p.dist(a) } else { dist_point_segment(*p, a, b) };
        if d > dmax {
            idx = i;
            dmax = d;
        }
    }
    if dmax > eps {
        let mut l = douglas_peucker(&pts[..=idx], eps);
        let r = douglas_peucker(&pts[idx..], eps);
        l.pop();
        l.extend(r);
        l
    } else {
        vec![a, b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(pts: Vec<Pt>) -> Vec<Pt> {
        pts.into_iter().enumerate().map(|(i, p)| Pt::new(p.x + ((i * 7919) % 13) as f64 * 0.08, p.y + ((i * 104729) % 11) as f64 * 0.08)).collect()
    }

    #[test]
    fn circle() {
        let pts = noisy(ellipse_pts(Pt::new(100.0, 100.0), 40.0, 41.0));
        let r = recognize(&pts, false).unwrap();
        assert!(r.len() > 20);
    }

    #[test]
    fn rectangle() {
        let mut pts = Vec::new();
        for (a, b) in [((0., 0.), (80., 0.)), ((80., 0.), (80., 50.)), ((80., 50.), (0., 50.)), ((0., 50.), (0., 1.))] {
            for k in 0..20 {
                let t = k as f64 / 20.0;
                pts.push(Pt::new(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            }
        }
        let r = recognize(&noisy(pts), false).unwrap();
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn triangle() {
        let mut pts = Vec::new();
        for (a, b) in [((0., 80.), (40., 0.)), ((40., 0.), (80., 80.)), ((80., 80.), (1., 80.))] {
            for k in 0..25 {
                let t = k as f64 / 25.0;
                pts.push(Pt::new(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            }
        }
        let r = recognize(&noisy(pts), false).unwrap();
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn rounded_rectangle_like_user_drawing() {
        // wie im Screenshot: oben links eckig, die anderen Ecken stark gerundet
        let mut pts = vec![];
        for k in 0..30 { pts.push(Pt::new(10.0, 10.0 + k as f64 * 6.0)); } // links runter
        for k in 0..=20 { let a = PI - k as f64 / 20.0 * PI / 2.0; pts.push(Pt::new(60.0 + 50.0 * a.cos(), 190.0 + 50.0 * a.sin() * 0.0 + 50.0 * (PI - a).sin())); }
        let mut q = vec![];
        for k in 0..=40 { let a = k as f64 / 40.0 * PI / 2.0; q.push(Pt::new(150.0 + 45.0 * a.sin(), 240.0 - 45.0 * (1.0 - a.cos()) - 0.0)); }
        pts.extend(q);
        for k in 0..30 { pts.push(Pt::new(195.0 - k as f64 * 0.3, 195.0 - k as f64 * 5.0)); }
        for k in 0..=20 { let a = k as f64 / 20.0 * PI / 2.0; pts.push(Pt::new(150.0 + 45.0 * a.cos(), 55.0 - 45.0 * a.sin())); }
        for k in 0..28 { pts.push(Pt::new(150.0 - k as f64 * 5.0, 10.0)); }
        let r = recognize(&noisy(pts), false).expect("Rechteck erwartet");
        assert_eq!(r.len(), 5, "als Rechteck erkannt");
    }

    #[test]
    fn line_and_scribble() {
        let line: Vec<Pt> = (0..50).map(|i| Pt::new(i as f64 * 2.0, i as f64 * 0.5 + (i % 3) as f64 * 0.2)).collect();
        assert_eq!(recognize(&line, false).unwrap().len(), 2);
        let s: Vec<Pt> = (0..80).map(|i| Pt::new(i as f64, (i as f64 * 0.4).sin() * 20.0)).collect();
        assert!(recognize(&s, false).is_none());
    }
}
