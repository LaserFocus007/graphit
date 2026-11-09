//! Kachel-Rendering nach dem Rnote-Prinzip: Worker-Threads rastern Seitenausschnitte
//! einmalig per Cairo, der Haupt-Thread lädt sie als Texturen hoch und die GPU
//! setzt sie pro Frame nur noch zusammen. Scrollen und Zoomen kosten so fast keine CPU.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};

use gtk::{gdk, glib};

use crate::geom::Rect;
use crate::model::PageData;
use crate::render;

/// Kantenlänge einer Kachel in Geräte-Pixeln.
pub const TILE: i32 = 512;

/// Zähler für Messungen (GRAPHIT_PROFILE): neu hochgeladene Kacheln.
pub static UPLOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Zähler für Messungen: gerade rendernde Worker.
pub static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Kind {
    /// PDF-Hintergrund (ändert sich praktisch nie, daher getrennt gecacht).
    Pdf,
    /// Inhalt (bei Nicht-PDF-Seiten inklusive Papiermuster).
    Ink,
    /// Inhalt für dunkles Papier, schon umgefärbt (Fotos bleiben unverändert).
    InkDark,
    /// Vorschaubild für die Seitenleiste.
    Thumb,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Key {
    pub page: u64,
    pub kind: Kind,
    /// Rasterskala (Geräte-Pixel pro pt) * 1024, gerundet.
    pub level: u32,
    pub tx: i32,
    pub ty: i32,
}

pub fn level_of(scale: f64) -> u32 {
    (scale * 1024.0).round().max(1.0) as u32
}

pub fn scale_of(level: u32) -> f64 {
    level as f64 / 1024.0
}

impl Key {
    /// Bereich der Kachel in Seitenkoordinaten (pt).
    pub fn page_rect(&self) -> Rect {
        let s = scale_of(self.level);
        let t = TILE as f64 / s;
        Rect::new(self.tx as f64 * t, self.ty as f64 * t, (self.tx + 1) as f64 * t, (self.ty + 1) as f64 * t)
    }
}

pub struct Job {
    pub key: Key,
    pub ver: u64,
    pub data: Arc<PageData>,
}

pub struct Done {
    pub key: Key,
    pub ver: u64,
    pub w: i32,
    pub h: i32,
    pub stride: usize,
    pub pixels: cairo::ImageSurfaceDataOwned,
}

struct Shared {
    queue: Mutex<(Vec<Job>, HashSet<(Key, u64)>)>,
    cv: Condvar,
}

pub struct Pool {
    shared: Arc<Shared>,
}

impl Pool {
    pub fn new(threads: usize) -> (Pool, async_channel::Receiver<Done>) {
        let shared = Arc::new(Shared { queue: Mutex::new((Vec::new(), HashSet::new())), cv: Condvar::new() });
        let (tx, rx) = async_channel::unbounded();
        for i in 0..threads {
            let sh = shared.clone();
            let tx = tx.clone();
            std::thread::Builder::new()
                .name(format!("graphit-render-{i}"))
                .spawn(move || worker(sh, tx))
                .expect("Render-Thread");
        }
        (Pool { shared }, rx)
    }

    /// Ersetzt die Warteschlange (sortiert nach Priorität). Laufende Jobs werden
    /// nicht doppelt gestartet.
    pub fn submit(&self, jobs: Vec<Job>) {
        let mut q = self.shared.queue.lock().unwrap();
        let running = &q.1;
        let jobs: Vec<Job> = jobs.into_iter().filter(|j| !running.contains(&(j.key, j.ver))).collect();
        q.0 = jobs;
        drop(q);
        self.shared.cv.notify_all();
    }
}

fn worker(sh: Arc<Shared>, tx: async_channel::Sender<Done>) {
    loop {
        let job = {
            let mut q = sh.queue.lock().unwrap();
            while q.0.is_empty() {
                q = sh.cv.wait(q).unwrap();
            }
            let job = q.0.remove(0);
            q.1.insert((job.key, job.ver));
            job
        };
        let res = render_job(&job);
        sh.queue.lock().unwrap().1.remove(&(job.key, job.ver));
        if let Some(done) = res {
            if tx.send_blocking(done).is_err() {
                return;
            }
        }
    }
}

fn render_job(job: &Job) -> Option<Done> {
    BUSY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let r = render_job_inner(job);
    BUSY.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    r
}

fn render_job_inner(job: &Job) -> Option<Done> {
    let d = &job.data;
    let s = scale_of(job.key.level);
    let (w, h, ox, oy) = match job.key.kind {
        Kind::Thumb => ((d.width * s).ceil() as i32, (d.height * s).ceil() as i32, 0, 0),
        _ => {
            let pw = (d.width * s).ceil() as i32;
            let ph = (d.height * s).ceil() as i32;
            let ox = job.key.tx * TILE;
            let oy = job.key.ty * TILE;
            if ox >= pw || oy >= ph {
                return None;
            }
            ((pw - ox).min(TILE), (ph - oy).min(TILE), ox, oy)
        }
    };
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w.max(1), h.max(1)).ok()?;
    {
        let cr = cairo::Context::new(&surf).ok()?;
        cr.translate(-ox as f64, -oy as f64);
        cr.scale(s, s);
        let clip = Rect::new(ox as f64 / s, oy as f64 / s, (ox + w) as f64 / s, (oy + h) as f64 / s);
        match job.key.kind {
            Kind::Pdf => {
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.paint().ok()?;
                if let Some(pdf) = &d.pdf {
                    render::draw_pdf_background(&cr, pdf, &d.bg, d.width, d.height, false);
                }
            }
            Kind::Ink | Kind::InkDark => {
                if !d.bg.is_pdf() {
                    render::draw_background(&cr, &d.bg, d.width, d.height, clip, job.key.kind == Kind::InkDark);
                }
                render::draw_items_in(&cr, &d.layers, Some(clip.expand(2.0)), job.key.kind == Kind::InkDark);
            }
            Kind::Thumb => render::draw_page(&cr, d, false),
        }
    }
    let stride = surf.stride() as usize;
    let pixels = surf.take_data().ok()?;
    Some(Done { key: job.key, ver: job.ver, w, h, stride, pixels })
}

// ------------------------------------------------------------------------ Cache

pub struct Tile {
    pub tex: gdk::MemoryTexture,
    pub ver: u64,
    pub w: i32,
    pub h: i32,
    pub used: u64,
}

pub struct Cache {
    pub tiles: HashMap<Key, Tile>,
    bytes: usize,
    pub frame: u64,
    budget: usize,
}

impl Cache {
    pub fn new(budget_mb: usize) -> Cache {
        Cache { tiles: HashMap::new(), bytes: 0, frame: 0, budget: budget_mb << 20 }
    }

    pub fn insert(&mut self, d: Done) {
        UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.tiles.get(&d.key) {
            if t.ver > d.ver {
                return;
            }
        }
        let bytes = glib::Bytes::from_owned(d.pixels);
        let tex = gdk::MemoryTexture::new(d.w, d.h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, d.stride);
        let size = (d.w * d.h * 4) as usize;
        if let Some(old) = self.tiles.insert(d.key, Tile { tex, ver: d.ver, w: d.w, h: d.h, used: self.frame }) {
            self.bytes -= (old.w * old.h * 4) as usize;
        }
        self.bytes += size;
        self.evict();
    }

    fn evict(&mut self) {
        if self.bytes <= self.budget {
            return;
        }
        let mut v: Vec<(u64, Key)> =
            self.tiles.iter().filter(|(_, t)| t.used + 2 < self.frame).map(|(k, t)| (t.used, *k)).collect();
        v.sort_unstable_by_key(|e| e.0);
        for (_, k) in v {
            if self.bytes <= self.budget * 3 / 4 {
                break;
            }
            if let Some(t) = self.tiles.remove(&k) {
                self.bytes -= (t.w * t.h * 4) as usize;
            }
        }
    }

    #[allow(dead_code)]
    pub fn remove_page(&mut self, page: u64) {
        let keys: Vec<Key> = self.tiles.keys().filter(|k| k.page == page).copied().collect();
        for k in keys {
            if let Some(t) = self.tiles.remove(&k) {
                self.bytes -= (t.w * t.h * 4) as usize;
            }
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.bytes = 0;
    }
}
