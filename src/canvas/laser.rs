//! Laserpointer: eine rote Leuchtspur über der Seite, die nie in der Notiz landet.
//! Nur das Aufsetzen zeichnet; Schweben zeigt den Leuchtpunkt und hält die Spur am Leben.
//! Verlässt der Stift die Reichweite, verschwindet die Spur sofort.

use std::time::{Duration, Instant};

use super::*;

/// Kurzes Ausblenden, damit die Spur nicht hart wegspringt.
const FADE: Duration = Duration::from_millis(150);
/// Melden manche Geräte das Verlassen der Reichweite nicht: so lange ohne jedes Signal gilt der Stift als weg.
const SILENT: Duration = Duration::from_millis(500);
/// Obergrenze je Linie, damit eine sehr lange Spur nicht ausufert.
const MAX_POINTS: usize = 6000;

#[derive(Default)]
pub struct Laser {
    /// Linien in Layout-Koordinaten, eine je Aufsetzen: die Spur bleibt beim Scrollen auf dem Inhalt
    lines: Vec<Vec<Pt>>,
    /// Stiftspitze für den Leuchtpunkt
    head: Option<Pt>,
    /// letztes Signal vom Stift, schwebend oder aufliegend
    last: Option<Instant>,
    /// seit wann der Stift außer Reichweite ist
    gone: Option<Instant>,
    ticking: bool,
}

impl Laser {
    /// Deckkraft: voll, solange der Stift in Reichweite ist, danach kurz gegen null.
    fn alpha(&self, now: Instant) -> f64 {
        let Some(last) = self.last else { return 0.0 };
        let since = self.gone.unwrap_or(last + SILENT);
        if now < since {
            return 1.0;
        }
        1.0 - (now.saturating_duration_since(since).as_secs_f64() / FADE.as_secs_f64()).min(1.0)
    }
}

/// Weiche Linie: Punkte unter `min` Abstand zusammenfassen, dann zweimal nach Chaikin glätten.
fn smooth(pts: &[(f32, f32)], min: f32) -> Vec<(f32, f32)> {
    let mut thin: Vec<(f32, f32)> = Vec::with_capacity(pts.len());
    for &p in pts {
        if thin.last().is_none_or(|q: &(f32, f32)| (p.0 - q.0).hypot(p.1 - q.1) >= min) {
            thin.push(p);
        }
    }
    if let (Some(&last), Some(&end)) = (thin.last(), pts.last()) {
        if last != end {
            thin.push(end);
        }
    }
    let mut out = thin;
    for _ in 0..2 {
        if out.len() < 3 {
            break;
        }
        let mut next = Vec::with_capacity(out.len() * 2);
        next.push(out[0]);
        for w in out.windows(2) {
            let (a, b) = (w[0], w[1]);
            next.push((0.75 * a.0 + 0.25 * b.0, 0.75 * a.1 + 0.25 * b.1));
            next.push((0.25 * a.0 + 0.75 * b.0, 0.25 * a.1 + 0.75 * b.1));
        }
        next.push(out[out.len() - 1]);
        out = next;
    }
    out
}

fn red(a: f32) -> gdk::RGBA {
    gdk::RGBA::new(1.0, 0.11, 0.08, a)
}

impl Canvas {
    /// Aufsetzen: neue Linie der Spur.
    pub(super) fn laser_down(&self, x: f64, y: f64) {
        self.laser_update(x, y, Some(true));
    }

    /// Aufliegend bewegt: Linie verlängern.
    pub(super) fn laser_draw(&self, x: f64, y: f64) {
        self.laser_update(x, y, Some(false));
    }

    /// Schwebend: nur der Leuchtpunkt folgt, die Spur bleibt.
    pub(super) fn laser_hover(&self, x: f64, y: f64) {
        self.laser_update(x, y, None);
    }

    /// Stift außer Reichweite: Spur sofort ausblenden.
    pub(super) fn laser_away(&self) {
        let mut st = self.st();
        if st.laser.last.is_some() && st.laser.gone.is_none() {
            st.laser.gone = Some(Instant::now());
            drop(st);
            self.queue_draw();
        }
    }

    /// `draw`: Some(true) neue Linie, Some(false) Linie verlängern, None nur schweben.
    fn laser_update(&self, x: f64, y: f64, draw: Option<bool>) {
        let start = {
            let mut st = self.st();
            if st.tools.tool != Tool::Laser {
                return;
            }
            let p = st.to_layout(x, y);
            let now = Instant::now();
            let l = &mut st.laser;
            // ganz verblasst: neu anfangen
            if l.alpha(now) <= 0.0 {
                l.lines.clear();
            }
            match draw {
                Some(true) => l.lines.push(vec![p]),
                Some(false) => match l.lines.last_mut() {
                    Some(line) => line.push(p),
                    None => l.lines.push(vec![p]),
                },
                None => {}
            }
            if let Some(line) = l.lines.last_mut() {
                if line.len() > MAX_POINTS {
                    let extra = line.len() - MAX_POINTS;
                    line.drain(..extra);
                }
            }
            l.head = Some(p);
            l.last = Some(now);
            l.gone = None;
            !std::mem::replace(&mut l.ticking, true)
        };
        if start {
            // achtet auf Stille und blendet aus; neu gezeichnet wird nur beim Ausblenden
            self.add_tick_callback(|c, _| {
                let alpha = c.st().laser.alpha(Instant::now());
                if alpha <= 0.0 {
                    c.st().laser = Laser::default();
                    c.queue_draw();
                    return glib::ControlFlow::Break;
                }
                if alpha < 1.0 {
                    c.queue_draw();
                }
                glib::ControlFlow::Continue
            });
        }
        self.queue_draw();
    }

    /// Spur zeichnen: weicher Lichthof aus Unschärfe, darüber ein klarer, heller Kern.
    pub(super) fn draw_laser(&self, st: &State, snap: &gtk::Snapshot) {
        let l = &st.laser;
        let alpha = l.alpha(Instant::now());
        if alpha <= 0.0 || (l.lines.is_empty() && l.head.is_none()) {
            return;
        }
        let (z, cam) = (st.zoom, st.cam);
        let scr = |p: &Pt| (((p.x - cam.x) * z) as f32, ((p.y - cam.y) * z) as f32);
        let paths: Vec<gsk::Path> = l
            .lines
            .iter()
            .filter(|line| line.len() >= 2)
            .map(|line| {
                let pts = smooth(&line.iter().map(scr).collect::<Vec<_>>(), 1.5);
                let b = gsk::PathBuilder::new();
                b.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    b.line_to(p.0, p.1);
                }
                b.to_path()
            })
            .collect();
        let stroke = |snap: &gtk::Snapshot, width: f32, color: &gdk::RGBA| {
            let pen = gsk::Stroke::new(width);
            pen.set_line_cap(gsk::LineCap::Round);
            pen.set_line_join(gsk::LineJoin::Round);
            for path in &paths {
                snap.append_stroke(path, &pen, color);
            }
        };
        let dot = |snap: &gtk::Snapshot, at: (f32, f32), r: f32, color: &gdk::RGBA| {
            let b = gsk::PathBuilder::new();
            b.add_circle(&graphene::Point::new(at.0, at.1), r);
            snap.append_fill(&b.to_path(), gsk::FillRule::Winding, color);
        };
        let head = l.head.as_ref().map(scr);
        snap.push_opacity(alpha);
        // schmaler, zarter Lichthof: hebt die Linie ab, ohne den Text darunter zu überstrahlen
        snap.push_blur(3.0);
        stroke(snap, 5.0, &red(0.35));
        if let Some(h) = head {
            dot(snap, h, 5.5, &red(0.4));
        }
        snap.pop();
        // klarer, satter Kern
        stroke(snap, 3.0, &gdk::RGBA::new(0.93, 0.1, 0.07, 1.0));
        stroke(snap, 1.0, &gdk::RGBA::new(1.0, 0.72, 0.68, 0.9));
        if let Some(h) = head {
            dot(snap, h, 3.6, &gdk::RGBA::new(0.93, 0.1, 0.07, 1.0));
            dot(snap, h, 1.5, &gdk::RGBA::new(1.0, 0.85, 0.82, 1.0));
        }
        snap.pop();
    }
}
