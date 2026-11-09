//! Druck-Kalibrierung: misst, wie leicht und wie fest jemand schreibt, und streckt
//! diesen Bereich in Graphit auf die volle Strichbreite (pressure_lo/hi).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, gdk};

use crate::canvas::Canvas;

#[derive(Default)]
struct Data {
    /// 0 = leicht schreiben, 1 = fest schreiben, 2 = Ergebnis
    step: usize,
    light: Vec<f64>,
    hard: Vec<f64>,
    /// Striche zur Anzeige: (x, y, Druck)
    strokes: Vec<Vec<(f64, f64, f64)>>,
}

fn percentile(v: &[f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(s[((s.len() - 1) as f64 * q).round() as usize])
}

/// Bereich aus den Messungen: unteres Ende knapp unter dem leichten Schreiben, oberes
/// beim festen Schreiben (Ausreißer abgeschnitten).
fn range(light: &[f64], hard: &[f64]) -> Option<(f64, f64)> {
    let lo = percentile(light, 0.1)? * 0.8;
    let hi = percentile(hard, 0.9)?;
    (hi - lo >= 0.15).then_some((lo.clamp(0.0, 0.8), hi.clamp(0.2, 1.0)))
}

pub fn show(parent: &impl IsA<gtk::Widget>, canvas: &Canvas, on_done: impl Fn(f64, f64) + 'static) {
    let data = Rc::new(RefCell::new(Data::default()));
    let dialog = adw::Dialog::builder().title(crate::i18n::tl("Druck kalibrieren")).content_width(620).build();
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.set_margin_top(12);
    bx.set_margin_bottom(18);
    bx.set_margin_start(18);
    bx.set_margin_end(18);

    let header = adw::HeaderBar::new();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);

    let info = gtk::Label::new(None);
    info.set_wrap(true);
    info.set_xalign(0.0);
    bx.append(&info);

    let area = gtk::DrawingArea::new();
    area.set_content_height(260);
    area.set_hexpand(true);
    area.add_css_class("card");
    bx.append(&area);

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    bx.append(&status);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let again = gtk::Button::with_label(crate::i18n::tl("Neu beginnen"));
    let next = gtk::Button::with_label(crate::i18n::tl("Weiter"));
    next.add_css_class("suggested-action");
    buttons.append(&again);
    buttons.append(&next);
    bx.append(&buttons);
    tv.set_content(Some(&bx));
    dialog.set_child(Some(&tv));

    let refresh = {
        let (data, info, status, next) = (data.clone(), info.clone(), status.clone(), next.clone());
        Rc::new(move || {
            let d = data.borrow();
            match d.step {
                0 => {
                    info.set_text(crate::i18n::tl("1/2 – Schreib ein paar Wörter so leicht, wie du beim zügigen Mitschreiben schreibst."));
                    status.set_text(&crate::tr!("{} Messpunkte", "{} samples", d.light.len()));
                    next.set_label(crate::i18n::tl("Weiter"));
                    next.set_sensitive(d.light.len() >= 40);
                }
                1 => {
                    info.set_text(crate::i18n::tl("2/2 – Jetzt ein paar Striche richtig fest, so wie du dicke Linien ziehen würdest."));
                    status.set_text(&crate::tr!("{} Messpunkte", "{} samples", d.hard.len()));
                    next.set_label(crate::i18n::tl("Auswerten"));
                    next.set_sensitive(d.hard.len() >= 40);
                }
                _ => {
                    match range(&d.light, &d.hard) {
                        Some((lo, hi)) => {
                            info.set_text(&crate::tr!(
                                "Leicht ≈ {:.2}, fest ≈ {:.2}. Graphit nutzt künftig den Bereich {:.2}–{:.2} für die volle Strichbreite.", "Light ≈ {:.2}, firm ≈ {:.2}. Graphit will now use the range {:.2}–{:.2} for the full stroke width.",
                                percentile(&d.light, 0.5).unwrap_or(0.0),
                                percentile(&d.hard, 0.5).unwrap_or(0.0),
                                lo,
                                hi
                            ));
                            next.set_label(crate::i18n::tl("Übernehmen"));
                            next.set_sensitive(true);
                        }
                        None => {
                            info.set_text(crate::i18n::tl("Leichtes und festes Schreiben lagen zu nah beieinander. Bitte noch einmal, mit deutlicherem Unterschied."));
                            next.set_sensitive(false);
                        }
                    }
                    status.set_text("");
                }
            }
        })
    };
    refresh();

    // Striche zeichnen (Breite nach Druck) als Rückmeldung
    let d2 = data.clone();
    area.set_draw_func(move |_, cr, _, _| {
        cr.set_source_rgb(0.2, 0.4, 0.85);
        cr.set_line_cap(cairo::LineCap::Round);
        for s in &d2.borrow().strokes {
            for w in s.windows(2) {
                cr.set_line_width(1.0 + 10.0 * w[1].2);
                cr.move_to(w[0].0, w[0].1);
                cr.line_to(w[1].0, w[1].1);
                let _ = cr.stroke();
            }
        }
    });

    let stylus = gtk::GestureStylus::new();
    let record = {
        let (data, area, refresh) = (data.clone(), area.clone(), refresh.clone());
        move |g: &gtk::GestureStylus, x: f64, y: f64, new: bool| {
            let Some(p) = g.axis(gdk::AxisUse::Pressure) else { return };
            if p <= 0.0 {
                return;
            }
            {
                let mut d = data.borrow_mut();
                match d.step {
                    0 => d.light.push(p),
                    1 => d.hard.push(p),
                    _ => return,
                }
                if new || d.strokes.is_empty() {
                    d.strokes.push(Vec::new());
                }
                d.strokes.last_mut().unwrap().push((x, y, p));
            }
            area.queue_draw();
            refresh();
        }
    };
    let r = record.clone();
    stylus.connect_down(move |g, x, y| r(g, x, y, true));
    stylus.connect_motion(move |g, x, y| record(g, x, y, false));
    area.add_controller(stylus);

    let (d3, area3, refresh3) = (data.clone(), area.clone(), refresh.clone());
    again.connect_clicked(move |_| {
        *d3.borrow_mut() = Data::default();
        area3.queue_draw();
        refresh3();
    });
    let (d4, area4, refresh4, dlg) = (data.clone(), area.clone(), refresh, dialog.clone());
    let on_done = Rc::new(on_done);
    let canvas = canvas.clone();
    next.connect_clicked(move |_| {
        let step = d4.borrow().step;
        if step >= 2 {
            let range = {
                let d = d4.borrow();
                range(&d.light, &d.hard)
            };
            if let Some((lo, hi)) = range {
                let mut cfg = canvas.st().cfg.clone();
                cfg.pressure_lo = lo;
                cfg.pressure_hi = hi;
                canvas.apply_settings(cfg);
                on_done(lo, hi);
            }
            dlg.close();
            return;
        }
        {
            let mut d = d4.borrow_mut();
            d.step += 1;
            d.strokes.clear();
        }
        area4.queue_draw();
        refresh4();
    });

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_from_light_and_hard() {
        let light: Vec<f64> = (0..50).map(|i| 0.05 + i as f64 * 0.002).collect();
        let hard: Vec<f64> = (0..50).map(|i| 0.5 + i as f64 * 0.006).collect();
        let (lo, hi) = range(&light, &hard).unwrap();
        assert!(lo > 0.03 && lo < 0.07, "lo {lo}");
        assert!(hi > 0.7 && hi < 0.8, "hi {hi}");
        assert!(range(&light, &light).is_none());
    }
}
