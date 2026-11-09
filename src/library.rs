//! Bibliothek als Startseite: Ordner durchblättern, zuletzt geöffnet, Suche.
//! Die Dateien bleiben ganz normale .xopp/.pdf im Dateisystem.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gtk::{gdk, glib};

const CARD_W: i32 = 150;

pub struct Library {
    pub root: gtk::Box,
    base: PathBuf,
    cwd: RefCell<PathBuf>,
    content: gtk::Box,
    crumbs: gtk::Box,
    search: gtk::SearchEntry,
    up: gtk::Button,
    on_open: RefCell<Option<Box<dyn Fn(&Path)>>>,
    thumb_tx: mpsc::Sender<(PathBuf, PathBuf)>,
    pending: Rc<RefCell<Vec<(PathBuf, gtk::Picture)>>>,
    all_files: RefCell<Option<Vec<PathBuf>>>,
}

fn is_doc(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with('.') || name.ends_with('~') || name.contains(".autosave.") || name.contains(".graphit-tmp") || name.contains(".federkiel-tmp") {
        return false;
    }
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("xopp") | Some("xoj") => true,
        // PDFs nur zeigen, wenn es keine Notiz dazu gibt (die öffnet sich ohnehin)
        Some("pdf") => !p.with_extension("xopp").exists(),
        _ => false,
    }
}

fn visible_dir(p: &Path) -> bool {
    p.is_dir() && !p.file_name().and_then(|n| n.to_str()).unwrap_or(".").starts_with('.')
}

/// Datei für das zwischengespeicherte Vorschaubild (abhängig von Pfad + Änderungszeit).
fn thumb_file(p: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    mtime.hash(&mut h);
    glib::user_cache_dir().join("graphit").join("thumbs").join(format!("{:016x}.png", h.finish()))
}

/// Erste Seite einer Notiz/PDF als PNG rendern (läuft im Hintergrund-Thread).
fn render_thumb(src: &Path, out: &Path) -> Option<()> {
    let ext = src.extension()?.to_str()?.to_ascii_lowercase();
    let page = if ext == "pdf" {
        let sizes = crate::render::pdf_page_sizes(src).ok()?;
        let (w, h) = *sizes.first()?;
        crate::model::PageData {
            width: w,
            height: h,
            bg: crate::model::Background::pdf(1),
            layers: Vec::new(),
            pdf: Some(src.to_path_buf()),
        }
    } else {
        let doc = crate::xopp::load(src).ok()?;
        let p = doc.pages.first()?.borrow();
        let d = p.snapshot(doc.page_pdf(&p));
        crate::model::PageData { width: d.width, height: d.height, bg: d.bg.clone(), layers: d.layers.clone(), pdf: d.pdf.clone() }
    };
    let s = (CARD_W as f64 * 1.5) / page.width;
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, (page.width * s) as i32, (page.height.min(page.width * 1.5) * s) as i32).ok()?;
    {
        let cr = cairo::Context::new(&surf).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(s, s);
        crate::render::draw_page(&cr, &page, false);
    }
    std::fs::create_dir_all(out.parent()?).ok()?;
    let tmp = out.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).ok()?;
    surf.write_to_png(&mut f).ok()?;
    std::fs::rename(&tmp, out).ok()
}

impl Library {
    pub fn new(base: PathBuf) -> Rc<Library> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.set_margin_top(12);
        top.set_margin_bottom(6);
        top.set_margin_start(18);
        top.set_margin_end(18);
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.add_css_class("flat");
        up.set_tooltip_text(Some(crate::i18n::tl("Übergeordneter Ordner")));
        top.append(&up);
        let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        crumbs.set_hexpand(true);
        top.append(&crumbs);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(crate::i18n::tl("In allen Notizen suchen")));
        search.set_width_chars(28);
        top.append(&search);
        root.append(&top);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(18);
        content.set_margin_end(18);
        content.set_margin_bottom(18);
        let scroller = gtk::ScrolledWindow::builder().child(&content).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        root.append(&scroller);

        // ein Hintergrund-Thread rendert Vorschaubilder nacheinander
        let (tx, rx) = mpsc::channel::<(PathBuf, PathBuf)>();
        let (done_tx, done_rx) = async_channel::unbounded::<PathBuf>();
        std::thread::Builder::new()
            .name("graphit-library".into())
            .spawn(move || {
                while let Ok((src, out)) = rx.recv() {
                    if out.exists() || render_thumb(&src, &out).is_some() {
                        let _ = done_tx.send_blocking(src);
                    }
                }
            })
            .ok();

        let lib = Rc::new(Library {
            root,
            base: base.clone(),
            cwd: RefCell::new(base),
            content,
            crumbs,
            search,
            up,
            on_open: RefCell::new(None),
            thumb_tx: tx,
            pending: Rc::new(RefCell::new(Vec::new())),
            all_files: RefCell::new(None),
        });
        let pending = lib.pending.clone();
        glib::spawn_future_local(async move {
            while let Ok(src) = done_rx.recv().await {
                let mut p = pending.borrow_mut();
                let (done, rest): (Vec<_>, Vec<_>) = p.drain(..).partition(|(s, _)| *s == src);
                *p = rest;
                for (s, pic) in done {
                    pic.set_filename(Some(thumb_file(&s)));
                }
            }
        });
        let w = Rc::downgrade(&lib);
        lib.up.connect_clicked(move |_| {
            if let Some(l) = w.upgrade() {
                let parent = l.cwd.borrow().parent().map(|p| p.to_path_buf());
                if let Some(p) = parent {
                    if p.starts_with(&l.base) {
                        l.navigate(p);
                    }
                }
            }
        });
        let w = Rc::downgrade(&lib);
        lib.search.connect_search_changed(move |_| {
            if let Some(l) = w.upgrade() {
                l.rebuild();
            }
        });
        lib
    }

    pub fn connect_open(&self, f: impl Fn(&Path) + 'static) {
        self.on_open.replace(Some(Box::new(f)));
    }

    pub fn navigate(self: &Rc<Self>, dir: PathBuf) {
        self.cwd.replace(dir);
        self.search.set_text("");
        self.rebuild();
    }

    /// Neu einlesen (z. B. beim Zurückkehren zur Bibliothek).
    pub fn reload(self: &Rc<Self>) {
        self.all_files.replace(None);
        self.rebuild();
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        self.pending.borrow_mut().clear();
        let cwd = self.cwd.borrow().clone();
        self.up.set_sensitive(cwd != self.base);
        // Brotkrumen
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
        let mut acc = self.base.clone();
        let mut parts = vec![(self.base.clone(), crate::i18n::tl("Bibliothek").to_string())];
        if let Ok(rel) = cwd.strip_prefix(&self.base) {
            for c in rel.components() {
                acc = acc.join(c);
                parts.push((acc.clone(), c.as_os_str().to_string_lossy().into_owned()));
            }
        }
        for (i, (p, name)) in parts.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Label::new(Some("›"));
                sep.add_css_class("dim-label");
                self.crumbs.append(&sep);
            }
            let b = gtk::Button::with_label(&name);
            b.add_css_class("flat");
            if p == cwd {
                b.add_css_class("heading");
            }
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(l) = w.upgrade() {
                    l.navigate(p.clone());
                }
            });
            self.crumbs.append(&b);
        }

        let query = self.search.text().to_lowercase();
        if !query.is_empty() {
            let files: Vec<PathBuf> = self
                .all_files()
                .into_iter()
                .filter(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase().contains(&query)).unwrap_or(false))
                .take(120)
                .collect();
            self.section(&crate::tr!("Suchergebnisse ({})", "Search results ({})", files.len()), &[], &files, true);
            return;
        }
        if cwd == self.base {
            // nur Notizen (.xopp), keine reinen PDFs
            let recent: Vec<PathBuf> = crate::recent::list()
                .into_iter()
                .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("xopp")).unwrap_or(false))
                .take(8)
                .collect();
            if !recent.is_empty() {
                self.section(crate::i18n::tl("Zuletzt geöffnet"), &[], &recent, true);
            }
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&cwd) {
            for e in rd.flatten() {
                let p = e.path();
                if visible_dir(&p) {
                    dirs.push(p);
                } else if is_doc(&p) {
                    files.push(p);
                }
            }
        }
        dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        if !dirs.is_empty() {
            self.section(crate::i18n::tl("Ordner"), &dirs, &[], false);
        }
        if !files.is_empty() {
            self.section(crate::i18n::tl("Notizen"), &[], &files, false);
        }
        if dirs.is_empty() && files.is_empty() {
            let s = adw::StatusPage::builder().icon_name("folder-symbolic").title("Leerer Ordner").build();
            self.content.append(&s);
        }
    }

    fn all_files(&self) -> Vec<PathBuf> {
        if self.all_files.borrow().is_none() {
            let mut out = Vec::new();
            let mut stack = vec![(self.base.clone(), 0)];
            while let Some((d, depth)) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if visible_dir(&p) && depth < 8 {
                        stack.push((p, depth + 1));
                    } else if is_doc(&p) {
                        out.push(p);
                    }
                }
            }
            out.sort();
            self.all_files.replace(Some(out));
        }
        self.all_files.borrow().clone().unwrap_or_default()
    }

    fn section(self: &Rc<Self>, title: &str, dirs: &[PathBuf], files: &[PathBuf], show_folder: bool) {
        let l = gtk::Label::new(Some(title));
        l.set_xalign(0.0);
        l.add_css_class("title-4");
        l.set_margin_top(12);
        self.content.append(&l);
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_homogeneous(true);
        flow.set_max_children_per_line(12);
        flow.set_column_spacing(12);
        flow.set_row_spacing(12);
        for d in dirs {
            let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let count = std::fs::read_dir(d).map(|r| r.flatten().filter(|e| is_doc(&e.path()) || visible_dir(&e.path())).count()).unwrap_or(0);
            let card = self.card(&name, &crate::tr!("{count} Einträge", "{count} items"), None);
            let w = Rc::downgrade(self);
            let d = d.clone();
            card.connect_clicked(move |_| {
                if let Some(l) = w.upgrade() {
                    l.navigate(d.clone());
                }
            });
            flow.insert(&card, -1);
        }
        for f in files {
            let name = f.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let sub = if show_folder {
                f.parent().and_then(|p| p.strip_prefix(&self.base).ok()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
            } else if f.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false) {
                "PDF".into()
            } else {
                String::new()
            };
            let card = self.card(&name, &sub, Some(f));
            let w = Rc::downgrade(self);
            let f = f.clone();
            card.connect_clicked(move |_| {
                if let Some(l) = w.upgrade() {
                    if let Some(cb) = l.on_open.borrow().as_ref() {
                        cb(&f);
                    }
                }
            });
            flow.insert(&card, -1);
        }
        self.content.append(&flow);
    }

    fn card(&self, title: &str, subtitle: &str, file: Option<&PathBuf>) -> gtk::Button {
        let b = gtk::Button::new();
        b.add_css_class("flat");
        b.add_css_class("fk-card");
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let pic = gtk::Picture::new();
        pic.set_size_request(CARD_W, (CARD_W as f64 * 1.3) as i32);
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_can_shrink(true);
        pic.add_css_class("fk-card-thumb");
        match file {
            Some(f) => {
                let t = thumb_file(f);
                if t.exists() {
                    pic.set_filename(Some(&t));
                } else {
                    self.pending.borrow_mut().push((f.clone(), pic.clone()));
                    let _ = self.thumb_tx.send((f.clone(), t));
                }
            }
            None => {
                let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
                let icon = theme.lookup_icon("folder", &[], 72, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
                pic.set_paintable(Some(&icon));
                pic.set_content_fit(gtk::ContentFit::ScaleDown);
                pic.remove_css_class("fk-card-thumb");
                pic.set_size_request(CARD_W, 84);
            }
        }
        bx.append(&pic);
        let l = gtk::Label::new(Some(title));
        l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        l.set_max_width_chars(18);
        l.set_tooltip_text(Some(title));
        bx.append(&l);
        if !subtitle.is_empty() {
            let s = gtk::Label::new(Some(subtitle));
            s.add_css_class("caption");
            s.add_css_class("dim-label");
            s.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            s.set_max_width_chars(18);
            bx.append(&s);
        }
        b.set_child(Some(&bx));
        b
    }
}
