#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod comic;
mod covers;
mod generate;
mod illustrations;
mod pairing_cli;
mod reader;
mod theme;
mod updates;
mod workers;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use reader::Reader;
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use vm_core::{BookFormat, BookSummary, Loc};
use vm_store::{Bookmark, Command, Event, SearchHit, ShelfFilter, ShelfSort, ShelfStatus, Store};
use workers::{ImportBook, ImportRequest, ImportWorker, LayoutWorker};

struct Options {
    data_dir: PathBuf,
    import: Option<PathBuf>,
    legacy: Option<PathBuf>,
    bench: Option<PathBuf>,
    pairing: Option<pairing_cli::Command>,
}

fn path_value(value: Option<OsString>, flag: &str) -> Result<PathBuf, String> {
    value
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} needs a path"))
}

fn options() -> Result<Option<Options>, String> {
    let mut data_dir = std::env::var_os("VELMIRA_NATIVE_DATA_DIR").map(PathBuf::from);
    let mut import = None;
    let mut legacy = None;
    let mut bench = None;
    let mut pairing = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--data-dir") => data_dir = Some(path_value(args.next(), "--data-dir")?),
            Some("--import") => import = Some(path_value(args.next(), "--import")?),
            Some("--migrate-legacy") => legacy = Some(path_value(args.next(), "--migrate-legacy")?),
            Some("--bench-startup") => bench = Some(path_value(args.next(), "--bench-startup")?),
            Some(
                flag
                @ ("--pair-listen" | "--pair-connect" | "--paired-devices" | "--unpair-device"),
            ) => {
                if pairing.is_some() {
                    return Err("Choose one pairing command.".into());
                }
                pairing = Some(match flag {
                    "--pair-listen" => {
                        pairing_cli::Command::Listen(socket_address(args.next(), flag)?)
                    }
                    "--pair-connect" => {
                        pairing_cli::Command::Connect(socket_address(args.next(), flag)?)
                    }
                    "--paired-devices" => pairing_cli::Command::List,
                    _ => pairing_cli::Command::Revoke(
                        args.next()
                            .and_then(|value| value.into_string().ok())
                            .filter(|value| !value.is_empty())
                            .ok_or("--unpair-device needs a device ID.")?,
                    ),
                });
            }
            Some("--help" | "-h") => {
                pairing_cli::attach_console();
                println!("velmira-native [--data-dir PATH] [--import BOOK] [--migrate-legacy OLD_DATA_DIR] [--bench-startup LOG.jsonl]");
                println!("velmira-native [--data-dir PATH] [--pair-listen IP:PORT | --pair-connect IP:PORT | --paired-devices | --unpair-device ID]");
                return Ok(None);
            }
            _ => return Err(format!("Unknown option: {}", arg.to_string_lossy())),
        }
    }
    if pairing.is_some() && (import.is_some() || legacy.is_some() || bench.is_some()) {
        return Err("Pairing cannot run together with import, migration, or benchmarking.".into());
    }
    let data_dir = data_dir
        .or_else(|| {
            let executable = std::env::current_exe().ok()?;
            portable_data_dir(&executable)
        })
        .or_else(|| {
            directories::ProjectDirs::from("dev", "Velmira", "Velmira Native")
                .map(|dirs| dirs.data_local_dir().to_path_buf())
        })
        .ok_or("No application data directory. Pass --data-dir PATH.")?;
    Ok(Some(Options {
        data_dir,
        import,
        legacy,
        bench,
        pairing,
    }))
}

fn portable_data_dir(executable: &std::path::Path) -> Option<PathBuf> {
    let dir = executable.parent()?;
    let data = dir.join("data");
    (dir.join("portable.txt").is_file() || data.is_dir()).then_some(data)
}

fn socket_address(value: Option<OsString>, flag: &str) -> Result<std::net::SocketAddr, String> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{flag} needs a numeric IP:PORT. Use [IP]:PORT for IPv6."))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let options = match options() {
        Ok(Some(options)) => options,
        Ok(None) => return Ok(()),
        Err(error) => {
            pairing_cli::attach_console();
            return Err(error.into());
        }
    };
    if let Some(command) = options.pairing {
        pairing_cli::attach_console();
        return pairing_cli::run(command, &options.data_dir);
    }
    let window = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([560.0, 460.0])
            .with_app_id("dev.velmira.native"),
        ..Default::default()
    };
    eframe::run_native(
        "Velmira Native",
        window,
        Box::new(move |cc| Ok(Box::new(App::new(cc.egui_ctx.clone(), start, options)?))),
    )?;
    Ok(())
}

struct App {
    store: Store,
    importer: ImportWorker,
    localized: bool,
    night: bool,
    reader_sidebar_open: bool,
    view: View,
    view_entered: f64,
    book_list: Vec<BookSummary>,
    has_more: bool,
    list_pending: bool,
    shelf_filter: ShelfFilter,
    shelf_query: String,
    renaming: Option<(String, String)>,
    reader: Reader,
    comic: comic::Comic,
    generator: generate::Generator,
    updates: updates::Updates,
    illustrations: illustrations::Illustrations,
    covers: covers::ShelfCovers,
    bookmarks: Vec<Bookmark>,
    more_bookmarks: bool,
    editing: Option<MarkEdit>,
    storage_ready: bool,
    opening: Option<String>,
    importing: bool,
    startup_import: Option<PathBuf>,
    startup_legacy: Option<PathBuf>,
    notice: Option<(String, bool)>,
    search: String,
    last_query: Option<String>,
    search_hits: Vec<SearchHit>,
    bench: Bench,
    idle_sample: Option<Instant>,
    first_ui: bool,
    pending_progress: std::collections::HashMap<String, Loc>,
    closing: bool,
    allow_close: bool,
}

struct MarkEdit {
    id: Option<(String, u32)>,
    loc: Loc,
    end_off: u32,
    color: vm_store::AnnotationColor,
    note: String,
}

enum CardAction {
    Open(String),
    Rename(String, String),
    Status(String, ShelfStatus),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Shelf,
    Novel,
    Comic,
}

#[allow(clippy::too_many_arguments)]
fn book_card(
    ui: &mut egui::Ui,
    covers: &mut covers::ShelfCovers,
    store: &Store,
    book: &BookSummary,
    card_w: f32,
    cover_h: f32,
    card_h: f32,
    is_active: bool,
    chinese: bool,
    rename_label: &str,
    status_label: &str,
    target_status: ShelfStatus,
    actions: &mut Vec<CardAction>,
) {
    let p = theme::palette(ui.visuals().dark_mode);
    ui.allocate_ui_with_layout(
        egui::vec2(card_w, card_h),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_min_height(card_h);
            ui.set_max_width(card_w);
            ui.spacing_mut().item_spacing.y = 5.0;
            let cover = covers.show_at(ui, store, book, egui::vec2(card_w, cover_h));
            if !is_active {
                ui.painter().rect_filled(
                    cover.rect,
                    10.0,
                    Color32::from_black_alpha(if ui.visuals().dark_mode { 110 } else { 70 }),
                );
            } else if cover.hovered() {
                ui.painter().rect_stroke(
                    cover.rect.expand(2.0),
                    12,
                    egui::Stroke::new(2.0_f32, p.accent),
                    egui::StrokeKind::Middle,
                );
            }
            if cover.clicked() && is_active {
                actions.push(CardAction::Open(book.id.clone()));
            }
            let title_color = if is_active { p.ink } else { p.muted };
            let title = ui
                .add_enabled_ui(is_active, |ui| {
                    ui.add_sized(
                        [card_w, 28.0],
                        egui::Button::new(
                            RichText::new(&book.title)
                                .size(15.0)
                                .strong()
                                .color(title_color),
                        )
                        .truncate()
                        .frame(false),
                    )
                })
                .inner
                .on_hover_text_at_pointer(&book.title);
            if title.clicked() && is_active {
                actions.push(CardAction::Open(book.id.clone()));
            }
            let (position_unit, total_unit) = match (chinese, book.format) {
                (true, BookFormat::Cbz) => ("页", "页"),
                (true, _) => ("章", "章"),
                (false, BookFormat::Cbz) => ("Page", "pages"),
                (false, _) => ("Chapter", "chapters"),
            };
            let meta = format!(
                "{} · {} {}",
                book.format.as_str().to_ascii_uppercase(),
                book.chapter_count,
                total_unit
            );
            ui.label(RichText::new(meta).size(12.0).color(p.muted));
            let (bar, _) =
                ui.allocate_exact_size(egui::vec2(card_w - 12.0, 4.0), egui::Sense::hover());
            ui.painter().rect_filled(bar, 2.0, p.line);
            if let Some(loc) = book.progress {
                let fraction = (loc.chapter + 1) as f32 / book.chapter_count.max(1) as f32;
                let fill = egui::Rect::from_min_size(
                    bar.min,
                    egui::vec2(bar.width() * fraction.clamp(0.02, 1.0), bar.height()),
                );
                ui.painter().rect_filled(fill, 2.0, p.accent);
            }
            let progress_text = match book.progress {
                Some(loc) => {
                    if chinese {
                        format!("读到第 {} {position_unit}", loc.chapter + 1)
                    } else {
                        format!("{position_unit} {}", loc.chapter + 1)
                    }
                }
                None => {
                    if chinese {
                        "未开始".to_owned()
                    } else {
                        "Not started".to_owned()
                    }
                }
            };
            ui.label(RichText::new(progress_text).size(11.0).color(p.muted));
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(RichText::new(rename_label).size(12.0)).small())
                    .clicked()
                {
                    actions.push(CardAction::Rename(book.id.clone(), book.title.clone()));
                }
                let color = if target_status == ShelfStatus::Removed {
                    p.danger
                } else {
                    p.success
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(status_label).size(12.0).color(color))
                            .small(),
                    )
                    .clicked()
                {
                    actions.push(CardAction::Status(book.id.clone(), target_status));
                }
            });
        },
    );
}

impl App {
    fn new(
        ctx: egui::Context,
        start: Instant,
        options: Options,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        theme::apply(&ctx, false);
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "velmira-cjk".into(),
            Arc::new(FontData::from_static(workers::CJK_FONT)),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let list = fonts.families.entry(family.clone()).or_default();
            if family == FontFamily::Proportional {
                list.insert(0, "velmira-cjk".into());
            } else {
                list.push("velmira-cjk".into());
            }
        }
        ctx.set_fonts(fonts);
        let store_context = ctx.clone();
        let store = Store::spawn(options.data_dir.join("velmira.db"), move || {
            store_context.request_repaint()
        })?;
        let importer = ImportWorker::spawn(ctx.clone())?;
        let layout = LayoutWorker::spawn(ctx.clone())?;
        let comic = comic::Comic::spawn(ctx.clone())?;
        let covers = covers::ShelfCovers::spawn(ctx.clone())?;
        let bench = Bench::new(start, options.bench);
        let updates = updates::Updates::new(start, !bench.enabled());
        let idle_sample = bench
            .enabled()
            .then(|| Instant::now() + Duration::from_secs(300));
        Ok(Self {
            store,
            importer,
            localized: true,
            night: false,
            reader_sidebar_open: true,
            view: View::Shelf,
            view_entered: f64::NEG_INFINITY,
            book_list: Vec::new(),
            has_more: false,
            list_pending: false,
            shelf_filter: Default::default(),
            shelf_query: String::new(),
            renaming: None,
            reader: Reader::new(layout),
            storage_ready: false,
            comic,
            generator: generate::Generator::new(&options.data_dir),
            updates,
            illustrations: Default::default(),
            covers,
            bookmarks: Vec::new(),
            more_bookmarks: false,
            editing: None,
            opening: None,
            importing: false,
            startup_import: options.import,
            startup_legacy: options.legacy,
            notice: None,
            search: String::new(),
            last_query: None,
            search_hits: Vec::new(),
            bench,
            idle_sample,
            first_ui: true,
            pending_progress: Default::default(),
            closing: false,
            allow_close: false,
        })
    }

    fn tr<'a>(&self, chinese: &'a str, english: &'a str) -> &'a str {
        if self.localized {
            chinese
        } else {
            english
        }
    }

    fn say(&mut self, message: String, error: bool) {
        self.notice = Some((message, error));
    }

    fn send(&mut self, command: Command) -> bool {
        match self.store.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.say(error.to_string(), true);
                false
            }
        }
    }

    fn list(&mut self, offset: u32) {
        if !self.list_pending || offset == 0 {
            self.list_pending = self.send(Command::ListShelf {
                offset,
                filter: self.shelf_filter.clone(),
            });
        }
    }

    fn start_import(&mut self, request: ImportRequest) {
        if self.storage_ready && !self.importing {
            match self.importer.send.try_send(request) {
                Ok(()) => self.importing = true,
                Err(error) => self.say(error.to_string(), true),
            }
        }
    }

    fn open(&mut self, book_id: String) {
        if self.opening.is_none()
            && self.send(Command::OpenBook {
                book_id: book_id.clone(),
            })
        {
            self.opening = Some(book_id);
        }
    }

    fn jump(&mut self, loc: Loc) {
        if let Some((book_id, chapter)) = self.reader.prepare_jump(loc) {
            if !self.send(Command::ReadChapter { book_id, chapter }) {
                self.reader.reset_load();
            }
        }
    }

    fn events(&mut self, ctx: &egui::Context) {
        if let Ok(result) = self.importer.receive.try_recv() {
            match result {
                Ok(Some(ImportBook::Text(book))) => {
                    self.importing = self.send(Command::InsertBook { book })
                }
                Ok(Some(ImportBook::Epub(path))) => {
                    self.importing = self.send(Command::ImportEpub { path });
                }
                Ok(Some(ImportBook::Comic(path))) => {
                    self.importing = self.send(Command::ImportCbz { path })
                }
                Ok(None) => self.importing = false,
                Err(error) => {
                    self.importing = false;
                    self.say(error, true);
                }
            }
        }
        while let Ok(event) = self.store.events().try_recv() {
            self.store_event(ctx, event);
        }
        self.covers.poll();
        if let Some(result) = self.reader.poll(ctx) {
            match result {
                Ok(shown) => {
                    if let Some(ms) = shown.layout_ms {
                        self.bench.mark("chapter_layout", Some(ms));
                    }
                    self.bench.mark(
                        if shown.first {
                            "first_page"
                        } else {
                            "page_flip"
                        },
                        Some(shown.raster_ms),
                    );
                }
                Err(error) => self.say(error, true),
            }
        }
        if let Some((book_id, loc)) = self.reader.unsaved() {
            if self.save_progress(book_id, loc) {
                self.reader.progress_sent(loc);
            } else {
                ctx.request_repaint_after(Duration::from_millis(35));
            }
        }
        if let Some(result) = self.comic.poll(ctx) {
            match result {
                Ok((book_id, loc)) => {
                    self.bench.mark("comic_page", None);
                    self.save_progress(book_id, loc);
                }
                Err(error) => self.say(error, true),
            }
        }
    }

    fn save_progress(&mut self, book_id: String, loc: Loc) -> bool {
        if self.send(Command::SaveProgress {
            book_id: book_id.clone(),
            loc,
        }) {
            self.pending_progress.insert(book_id, loc);
            true
        } else {
            false
        }
    }

    fn store_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Ready => {
                self.storage_ready = true;
                self.bench.mark("database_ready", None);
                self.list(0);
                if let Some(data_dir) = self.startup_legacy.take() {
                    self.send(Command::MigrateLegacy { data_dir });
                }
                if let Some(path) = self.startup_import.take() {
                    self.start_import(ImportRequest::File(path));
                }
            }
            Event::Shelf {
                offset,
                filter,
                books,
                has_more,
            } => {
                if filter != self.shelf_filter {
                    return;
                }
                self.list_pending = false;
                self.has_more = has_more;
                if offset == 0 {
                    self.book_list = books;
                } else if offset as usize == self.book_list.len() {
                    self.book_list.extend(books);
                }
                self.bench.mark("shelf_loaded", None);
            }
            Event::Books { .. } => {}
            Event::BookUpdated { .. } => self.list(0),
            Event::CoverRead {
                book_id,
                size: vm_store::CoverSize::Shelf,
                bytes,
            } => self.covers.loaded(book_id, bytes),
            Event::CoverRead { .. } => {}
            Event::BookInserted { book, duplicate } => {
                self.importing = false;
                self.say(
                    if duplicate {
                        self.tr("已在书库", "Already in your library")
                    } else {
                        self.tr("导入完成", "Imported")
                    }
                    .to_owned(),
                    false,
                );
                self.bench.mark("book_imported", None);
                self.list_pending = false;
                self.list(0);
                self.open(book.id);
            }
            Event::BookOpened {
                book,
                toc,
                chapter_index,
                chapter,
            } => {
                if self.opening.as_ref() != Some(&book.id) {
                    return;
                }
                self.opening = None;
                self.comic.close();
                self.illustrations.close();
                self.bookmarks.clear();
                self.more_bookmarks = false;
                self.editing = None;
                self.search.clear();
                self.search_hits.clear();
                self.last_query = None;
                self.bench.mark("book_opened", None);
                self.send(Command::ListBookmarks {
                    book_id: book.id.clone(),
                    offset: 0,
                });
                self.send(Command::ListAnnotations {
                    book_id: book.id.clone(),
                    chapter: chapter_index,
                });
                self.reader.open(book, toc, chapter_index, chapter);
            }
            Event::ComicOpened { book, page } => {
                if self.opening.as_ref() != Some(&book.id) {
                    return;
                }
                self.opening = None;
                self.reader.close();
                self.illustrations.close();
                self.editing = None;
                self.bench.mark("comic_opened", None);
                self.comic.open(book, page);
            }
            Event::ComicPageRead { book_id, page } => self.comic.loaded(&book_id, page),
            Event::Illustrations {
                book_id,
                chapter,
                illustrations,
            } => self.illustrations.listed(&book_id, chapter, illustrations),
            Event::IllustrationRead {
                book_id,
                chapter,
                ordinal,
                image,
            } => self
                .illustrations
                .loaded(ctx, &book_id, chapter, ordinal, image),
            Event::ChapterRead {
                book_id,
                chapter_index,
                chapter,
            } => {
                if self.reader.chapter_read(&book_id, chapter_index, chapter) {
                    self.editing = None;
                    self.illustrations.close();
                    self.send(Command::ListAnnotations {
                        book_id,
                        chapter: chapter_index,
                    });
                }
            }
            Event::ProgressSaved { book_id, loc } => {
                if self.pending_progress.get(&book_id) == Some(&loc) {
                    self.pending_progress.remove(&book_id);
                }
                if let Some(book) = self.book_list.iter_mut().find(|book| book.id == book_id) {
                    book.progress = Some(loc);
                }
            }
            Event::SearchResults {
                query,
                book_id,
                hits,
            } => {
                if Some(&query) == self.last_query.as_ref()
                    && self.reader.book_id() == Some(book_id.as_str())
                {
                    self.search_hits = hits;
                }
            }
            Event::MigrationFinished(report) => {
                self.list_pending = false;
                self.list(0);
                let message = if report.already_migrated {
                    self.tr("此旧书库已迁移", "This library has already been migrated")
                        .to_owned()
                } else if self.localized {
                    format!("已迁移 {} 本书、{} 个进度、{} 个书签、{} 条批注，恢复 {} 张封面。跳过 {} 本书和 {} 条定位，其中 {} 本漫画可补齐原文件后重试。", report.books, report.progress, report.bookmarks, report.annotations, report.covers, report.skipped_books, report.skipped_positions, report.pending_comics)
                } else {
                    format!("Migrated {} books, {} progress entries, {} bookmarks and {} annotations. Recovered {} covers. Skipped {} books and {} positions. Retry {} comics after restoring their archives.", report.books, report.progress, report.bookmarks, report.annotations, report.covers, report.skipped_books, report.skipped_positions, report.pending_comics)
                };
                self.say(message, false);
            }
            Event::Bookmarks {
                book_id,
                offset,
                bookmarks,
                has_more,
            } => {
                if self.reader.book_id() == Some(&book_id) {
                    self.more_bookmarks = has_more;
                    if offset == 0 {
                        self.bookmarks = bookmarks;
                    } else if offset as usize == self.bookmarks.len() {
                        self.bookmarks.extend(bookmarks);
                    }
                }
            }
            Event::BookmarkAdded { book_id, .. } | Event::BookmarkDeleted { book_id, .. } => {
                if self.reader.book_id() == Some(&book_id) {
                    self.send(Command::ListBookmarks { book_id, offset: 0 });
                }
            }
            Event::Annotations {
                book_id,
                chapter,
                annotations,
            } => {
                if self.reader.book_id() == Some(&book_id) && self.reader.chapter_index() == chapter
                {
                    self.reader.annotations = annotations;
                }
            }
            Event::AnnotationSaved {
                book_id,
                annotation,
            } => self.refresh_annotations(book_id, annotation.loc.chapter),
            Event::AnnotationDeleted {
                book_id, chapter, ..
            } => self.refresh_annotations(book_id, chapter),
            Event::Error { operation, message } => {
                match operation {
                    "open_book" => self.opening = None,
                    "read_chapter" => self.reader.reset_load(),
                    "read_comic_page" => self.comic.reset_load(),
                    "read_cover" => self.covers.read_failed(),
                    "list_illustrations" | "read_illustration" => {
                        self.illustrations.fail(message.clone());
                    }
                    "insert_book" => self.importing = false,
                    "list_books" => self.list_pending = false,
                    "save_progress" => {
                        self.pending_progress.clear();
                        self.closing = false;
                    }
                    _ => {}
                }
                self.say(format!("{operation}: {message}"), true);
            }
        }
    }

    fn refresh_annotations(&mut self, book_id: String, chapter: u32) {
        if self.reader.book_id() == Some(&book_id) && self.reader.chapter_index() == chapter {
            self.send(Command::ListAnnotations { book_id, chapter });
        }
    }

    fn shelf(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        ui.add_space(9.0);
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new(self.tr("书架", "Library"))
                    .size(29.0)
                    .strong()
                    .color(p.ink),
            );
            if self.storage_ready && !self.book_list.is_empty() {
                let count = match (self.localized, self.has_more) {
                    (true, true) => format!("已加载 {} 本", self.book_list.len()),
                    (true, false) => format!("{} 本", self.book_list.len()),
                    (false, true) => format!("{} loaded", self.book_list.len()),
                    (false, false) => format!("{} books", self.book_list.len()),
                };
                ui.label(RichText::new(count).size(13.0).color(p.muted));
            }
        });
        ui.add_space(8.0);
        let mut filter = self.shelf_filter.clone();
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut filter.status,
                ShelfStatus::Active,
                self.tr("书库", "Library"),
            );
            ui.selectable_value(
                &mut filter.status,
                ShelfStatus::Removed,
                self.tr("已移出", "Removed"),
            );
            egui::ComboBox::from_id_salt("shelf-format")
                .selected_text(
                    filter
                        .format
                        .map(|f| f.as_str().to_ascii_uppercase())
                        .unwrap_or_else(|| self.tr("所有格式", "All formats").into()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut filter.format,
                        None,
                        self.tr("所有格式", "All formats"),
                    );
                    for format in [BookFormat::Txt, BookFormat::Epub, BookFormat::Cbz] {
                        ui.selectable_value(
                            &mut filter.format,
                            Some(format),
                            format.as_str().to_ascii_uppercase(),
                        );
                    }
                });
            let sort_options = [
                (ShelfSort::Newest, self.tr("最近导入", "Recently imported")),
                (ShelfSort::Title, self.tr("书名", "Title")),
                (ShelfSort::LastRead, self.tr("最近阅读", "Recently read")),
            ];
            egui::ComboBox::from_id_salt("shelf-sort")
                .selected_text(
                    sort_options
                        .iter()
                        .find(|(sort, _)| *sort == filter.sort)
                        .map_or("", |(_, label)| *label),
                )
                .show_ui(ui, |ui| {
                    for (sort, label) in sort_options {
                        ui.selectable_value(&mut filter.sort, sort, label);
                    }
                });
            let placeholder = self.tr("搜索书名", "Search titles");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.shelf_query)
                    .desired_width(135.0)
                    .char_limit(120)
                    .hint_text(placeholder),
            );
            if (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button(self.tr("搜索", "Find")).clicked()
            {
                filter.query = self.shelf_query.trim().to_owned();
            }
            if !self.shelf_query.is_empty() && ui.button(self.tr("清空", "Clear")).clicked() {
                self.shelf_query.clear();
                filter.query.clear();
            }
        });
        if self.storage_ready && self.shelf_filter != filter {
            self.shelf_filter = filter;
            self.book_list.clear();
            self.has_more = false;
            self.list(0);
        }
        ui.add_space(10.0);
        if self.book_list.is_empty() {
            let message = if self.list_pending {
                self.tr("正在加载…", "Loading…")
            } else if self.storage_ready && self.shelf_filter != ShelfFilter::default() {
                self.tr("没有符合条件的书籍。", "No matching books.")
            } else if self.storage_ready {
                self.tr(
                    "拖入 TXT、EPUB 或 CBZ，也可以点击「导入书籍」。",
                    "Drop a TXT, EPUB, or CBZ file, or choose Import book.",
                )
            } else {
                self.tr("正在打开本地书库…", "Opening the local library…")
            };
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(72.0, 92.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 8.0, p.surface);
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(12.0, rect.height())),
                    egui::CornerRadius {
                        nw: 8,
                        sw: 8,
                        ne: 0,
                        se: 0,
                    },
                    p.accent_weak,
                );
                for i in 0..3 {
                    let y = rect.min.y + 28.0 + i as f32 * 16.0;
                    painter.line_segment(
                        [
                            egui::pos2(rect.min.x + 24.0, y),
                            egui::pos2(rect.max.x - 14.0, y),
                        ],
                        egui::Stroke::new(2.0_f32, p.line),
                    );
                }
                painter.rect_stroke(
                    rect,
                    8,
                    egui::Stroke::new(1.5_f32, p.line),
                    egui::StrokeKind::Middle,
                );
                ui.add_space(16.0);
                ui.label(RichText::new(message).size(14.0).color(p.muted));
            });
        }
        if self.has_more
            && ui
                .add_enabled(
                    !self.list_pending,
                    egui::Button::new(self.tr("加载更多", "Load more")),
                )
                .clicked()
        {
            self.list(self.book_list.len() as u32);
        }
        let rename_label = self.tr("改名", "Rename");
        let is_active = self.shelf_filter.status == ShelfStatus::Active;
        let (target_status, status_label) = if is_active {
            (ShelfStatus::Removed, self.tr("移出", "Remove"))
        } else {
            (ShelfStatus::Active, self.tr("恢复", "Restore"))
        };
        let chinese = self.localized;
        let mut actions: Vec<CardAction> = Vec::new();
        let (card_w, cover_h, card_h, gap) = (148.0, 222.0, 347.0, 19.0);
        let columns = (((ui.available_width() + gap) / (card_w + gap)).floor() as usize).max(1);
        let rows = self.book_list.len().div_ceil(columns);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            egui::ScrollArea::vertical()
                .id_salt((
                    "shelf-grid",
                    self.shelf_filter.status == ShelfStatus::Removed,
                ))
                .show_rows(ui, card_h, rows, |ui, range| {
                    for row in range {
                        ui.push_id(row, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.set_min_height(card_h);
                                for col in 0..columns {
                                    let Some(book) = self.book_list.get(row * columns + col) else {
                                        break;
                                    };
                                    ui.push_id(&book.id, |ui| {
                                        book_card(
                                            ui,
                                            &mut self.covers,
                                            &self.store,
                                            book,
                                            card_w,
                                            cover_h,
                                            card_h,
                                            is_active,
                                            chinese,
                                            rename_label,
                                            status_label,
                                            target_status,
                                            &mut actions,
                                        )
                                    });
                                }
                            })
                        });
                    }
                });
        });
        for action in actions {
            match action {
                CardAction::Open(id) => self.open(id),
                CardAction::Rename(id, title) => self.renaming = Some((id, title)),
                CardAction::Status(book_id, status) => {
                    self.send(Command::SetBookStatus { book_id, status });
                }
            }
        }
        if let Some((book_id, mut title)) = self.renaming.take() {
            let (mut open, mut save) = (true, false);
            egui::Window::new(self.tr("修改书名", "Rename book"))
                .collapsible(false)
                .default_width(340.0)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.add_space(4.0);
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut title)
                            .char_limit(200)
                            .desired_width(300.0),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(6.0);
                    save = !title.trim().is_empty()
                        && (enter || ui.button(self.tr("保存", "Save")).clicked());
                });
            if save {
                self.send(Command::RenameBook {
                    book_id,
                    title: title.trim().to_owned(),
                });
            } else if open {
                self.renaming = Some((book_id, title));
            }
        }
    }

    fn reader_sidebar(&mut self, ctx: &egui::Context) -> bool {
        let id = egui::Id::new("reader_contents");
        let expansion = ctx.animate_bool_with_time_and_easing(
            id.with("expansion"),
            self.reader_sidebar_open,
            0.18,
            egui::emath::easing::quadratic_in_out,
        );
        if expansion <= 0.0 {
            return false;
        }
        let animating = expansion < 1.0;
        let mut collapse = false;
        let mut jump = None;
        let mut submit = false;
        let mut bookmark = false;
        let mut remove_bookmark = None;
        let mut load_bookmarks = false;
        let mut edit = None;
        let sidebar_frame = egui::Frame::new().fill(theme::palette(self.night).surface);
        let max_width = (ctx.available_rect().width() * 0.5).clamp(220.0, 420.0);
        let stable_panel = egui::panel::PanelState::load(ctx, id);
        let expanded_width = stable_panel
            .map_or(240.0, |state| state.rect.width())
            .clamp(220.0, max_width);
        let mut panel = egui::SidePanel::left(id)
            .resizable(true)
            .default_width(240.0)
            .width_range(220.0..=max_width)
            .frame(sidebar_frame);
        if animating {
            panel = panel
                .resizable(false)
                .exact_width(expansion * expanded_width);
        }
        panel.show(ctx, |panel_ui| {
            let bounds = panel_ui.max_rect();
            panel_ui.set_min_width(bounds.width());
            let mut content = bounds.shrink2(egui::vec2(14.0, 12.0));
            if animating {
                // Slide the full-width contents behind the moving panel edge.
                content.min.x -= expanded_width - bounds.width();
            }
            let mut builder = egui::UiBuilder::new().id_salt("contents").max_rect(content);
            if animating {
                builder = builder.disabled();
            }
            let ui = &mut panel_ui.new_child(builder);
            ui.horizontal(|ui| {
                ui.heading(RichText::new(self.tr("目录", "Contents")).size(18.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = ui.button(self.tr("收起侧栏", "Hide sidebar"));
                    if button.clicked() {
                        collapse = true;
                        if button.has_focus() {
                            ui.input_mut(|input| {
                                input.consume_key(input.modifiers, egui::Key::Space);
                            });
                        }
                        button.surrender_focus();
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let hint = if self.localized {
                    "本书搜索"
                } else {
                    "Search this book"
                };
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(hint)
                        .desired_width((ui.available_width() - 62.0).clamp(90.0, 180.0))
                        .char_limit(120),
                );
                let enter =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                submit = enter || ui.button(self.tr("搜索", "Find")).clicked();
            });
            if !self.search_hits.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("hits")
                    .max_height(165.0)
                    .show_rows(ui, 32.0, self.search_hits.len(), |ui, rows| {
                        for index in rows {
                            let hit = &self.search_hits[index];
                            if ui
                                .add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Button::new(egui::RichText::new(&hit.excerpt).size(13.0))
                                        .truncate(),
                                )
                                .on_hover_text(&hit.excerpt)
                                .clicked()
                            {
                                jump = Some(hit.loc);
                            }
                        }
                    });
                ui.separator();
            }
            ui.collapsing(self.tr("书签", "Bookmarks"), |ui| {
                bookmark = ui
                    .button(self.tr("标记当前位置", "Bookmark this position"))
                    .clicked();
                egui::ScrollArea::vertical()
                    .id_salt("bookmarks")
                    .max_height(150.0)
                    .show_rows(ui, 32.0, self.bookmarks.len(), |ui, rows| {
                        for index in rows {
                            let mark = &self.bookmarks[index];
                            ui.horizontal(|ui| {
                                ui.set_min_height(32.0);
                                let label =
                                    mark.label.as_deref().map(str::to_owned).unwrap_or_else(|| {
                                        format!("{} · {}", mark.loc.chapter + 1, mark.loc.para + 1)
                                    });
                                if ui
                                    .add_sized(
                                        [(ui.available_width() - 40.0).max(24.0), 32.0],
                                        egui::Button::new(&label).truncate(),
                                    )
                                    .on_hover_text(label)
                                    .clicked()
                                {
                                    jump = Some(mark.loc);
                                }
                                if ui.small_button("×").clicked() {
                                    remove_bookmark = Some(mark.loc);
                                }
                            });
                        }
                    });
                load_bookmarks =
                    self.more_bookmarks && ui.button(self.tr("更多", "More")).clicked();
            });
            ui.collapsing(self.tr("本章批注", "Chapter notes"), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("annotations")
                    .max_height(150.0)
                    .show_rows(ui, 32.0, self.reader.annotations.len(), |ui, rows| {
                        for index in rows {
                            let annotation = &self.reader.annotations[index];
                            ui.horizontal(|ui| {
                                ui.set_min_height(32.0);
                                let excerpt: String = annotation.text.chars().take(14).collect();
                                if ui
                                    .add_sized(
                                        [(ui.available_width() - 54.0).max(24.0), 32.0],
                                        egui::Button::new(excerpt).truncate(),
                                    )
                                    .on_hover_text(
                                        annotation.note.as_deref().unwrap_or(&annotation.text),
                                    )
                                    .clicked()
                                {
                                    jump = Some(annotation.loc);
                                }
                                if ui.small_button(self.tr("编辑", "Edit")).clicked() {
                                    edit = Some(annotation.clone());
                                }
                            });
                        }
                    });
            });
            egui::ScrollArea::vertical().id_salt("toc").show_rows(
                ui,
                32.0,
                self.reader.toc().len(),
                |ui, rows| {
                    for index in rows {
                        if ui
                            .add_sized(
                                [ui.available_width(), 32.0],
                                egui::Button::selectable(
                                    self.reader.chapter_index() == index as u32,
                                    &self.reader.toc()[index],
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.reader.toc()[index])
                            .clicked()
                        {
                            jump = Some(Loc {
                                chapter: index as u32,
                                ..Loc::default()
                            });
                        }
                    }
                },
            );
        });
        if animating {
            // The animation must not replace our last user-resized panel width.
            if let Some(state) = stable_panel {
                ctx.data_mut(|data| data.insert_persisted(id, state));
            } else {
                ctx.data_mut(|data| data.remove::<egui::panel::PanelState>(id));
            }
        }
        if collapse {
            self.reader_sidebar_open = false;
            ctx.request_repaint();
        }
        if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
            if let Some(loc) = remove_bookmark {
                self.send(Command::DeleteBookmark {
                    book_id: book_id.clone(),
                    loc,
                });
            }
            if bookmark {
                if let Some(loc) = self.reader.loc() {
                    self.send(Command::AddBookmark {
                        book_id: book_id.clone(),
                        loc,
                        label: None,
                    });
                }
            }
            if load_bookmarks {
                self.send(Command::ListBookmarks {
                    book_id,
                    offset: self.bookmarks.len() as u32,
                });
            }
        }
        if let Some(annotation) = edit {
            self.editing = Some(MarkEdit {
                id: Some((annotation.id, annotation.revision)),
                loc: annotation.loc,
                end_off: annotation.end_off,
                color: annotation.color,
                note: annotation.note.unwrap_or_default(),
            });
        }
        if submit {
            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                let query = self.search.trim().to_owned();
                self.search_hits.clear();
                self.last_query = (!query.is_empty()).then(|| query.clone());
                if !query.is_empty() {
                    self.send(Command::Search {
                        query,
                        book_id,
                        chapter: Some(self.reader.chapter_index()),
                        limit: 50,
                    });
                }
            }
        }
        if let Some(loc) = jump {
            self.jump(loc);
        }
        animating || collapse
    }

    fn annotation_editor(&mut self, ctx: &egui::Context) {
        let Some(mut mark) = self.editing.take() else {
            return;
        };
        let Some(book_id) = self.reader.book_id().map(str::to_owned) else {
            return;
        };
        let (mut open, mut save, mut delete) = (true, false, false);
        use vm_store::AnnotationColor::*;
        egui::Window::new(self.tr("批注", "Note"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let night = ui.visuals().dark_mode;
                    let chip = |color: vm_store::AnnotationColor| match color {
                        Yellow => {
                            if night {
                                Color32::from_rgb(226, 186, 80)
                            } else {
                                Color32::from_rgb(176, 132, 16)
                            }
                        }
                        Green => {
                            if night {
                                Color32::from_rgb(139, 205, 164)
                            } else {
                                Color32::from_rgb(63, 127, 82)
                            }
                        }
                        Blue => {
                            if night {
                                Color32::from_rgb(128, 186, 232)
                            } else {
                                Color32::from_rgb(51, 98, 140)
                            }
                        }
                        Pink => {
                            if night {
                                Color32::from_rgb(235, 148, 184)
                            } else {
                                Color32::from_rgb(190, 80, 120)
                            }
                        }
                    };
                    for (color, title) in [
                        (Yellow, self.tr("黄", "Yellow")),
                        (Green, self.tr("绿", "Green")),
                        (Blue, self.tr("蓝", "Blue")),
                        (Pink, self.tr("粉", "Pink")),
                    ] {
                        ui.selectable_value(
                            &mut mark.color,
                            color,
                            RichText::new(title).color(chip(color)),
                        );
                    }
                });
                ui.add(
                    egui::TextEdit::multiline(&mut mark.note)
                        .desired_rows(4)
                        .char_limit(3000),
                );
                ui.horizontal(|ui| {
                    save = ui.button(self.tr("保存", "Save")).clicked();
                    delete = mark.id.is_some() && ui.button(self.tr("删除", "Delete")).clicked();
                });
            });
        let sent = if save {
            let note = (!mark.note.trim().is_empty()).then(|| mark.note.clone());
            let command = if let Some((id, expected_revision)) = mark.id.clone() {
                Command::UpdateAnnotation {
                    book_id,
                    id,
                    expected_revision,
                    note,
                    color: mark.color,
                }
            } else {
                Command::AddAnnotation {
                    book_id,
                    loc: mark.loc,
                    end_off: mark.end_off,
                    note,
                    color: mark.color,
                }
            };
            self.send(command)
        } else if delete {
            if let Some((id, expected_revision)) = mark.id.clone() {
                self.send(Command::DeleteAnnotation {
                    book_id,
                    id,
                    expected_revision,
                })
            } else {
                false
            }
        } else {
            false
        };
        if open && !sent {
            self.editing = Some(mark);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.first_ui {
            self.first_ui = false;
            self.bench.mark("first_ui_frame", None);
        }
        if let Some(due) = self.idle_sample {
            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                ctx.request_repaint_after(delay);
            } else {
                self.idle_sample = None;
                if !self.reader.is_open() && self.comic.book_id().is_none() && !self.importing {
                    self.bench.mark("idle_5min", None);
                }
            }
        }
        self.events(ctx);
        if ctx.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && (!self.pending_progress.is_empty() || self.reader.unsaved().is_some())
        {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.closing {
            if self.pending_progress.is_empty() && self.reader.unsaved().is_none() {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if let Some(path) = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .and_then(|file| file.path.clone())
        }) {
            self.start_import(ImportRequest::File(path));
        }
        let mut back = false;
        let mut toggle_theme = false;
        let mut generate_comic = false;
        let mut show_illustrations = false;
        let mut toggle_sidebar = false;
        let p = theme::palette(self.night);
        let toolbar_frame = egui::Frame::new()
            .fill(p.surface)
            .inner_margin(egui::Margin::symmetric(16, 10));
        egui::TopBottomPanel::top("toolbar")
            .frame(toolbar_frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (logo, _) =
                        ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                    ui.painter().rect_filled(logo, 8.0, p.accent);
                    ui.painter().text(
                        logo.center(),
                        egui::Align2::CENTER_CENTER,
                        "V",
                        egui::FontId::proportional(16.0),
                        p.on_accent,
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("Velmira").size(20.0).strong().color(p.ink));
                    ui.add_space(6.0);
                    if (self.reader.is_open() || self.comic.book_id().is_some())
                        && ui.button(self.tr("返回书架", "Library")).clicked()
                    {
                        back = true;
                    }
                    ui.separator();
                    let enabled = self.storage_ready && !self.importing;
                    let label = if self.importing {
                        self.tr("正在导入…", "Importing…")
                    } else {
                        self.tr("导入书籍", "Import book")
                    };
                    let button = if enabled {
                        egui::Button::new(RichText::new(label).strong().color(p.on_accent))
                            .fill(p.accent)
                            .stroke(egui::Stroke::NONE)
                    } else {
                        egui::Button::new(label)
                    };
                    if ui.add_enabled(enabled, button).clicked() {
                        self.start_import(ImportRequest::Pick);
                    }
                    if self.reader.is_open() && ui.button("小说转漫画").clicked() {
                        generate_comic = true;
                    }
                    if self.reader.is_epub() && ui.button("插图").clicked() {
                        show_illustrations = true;
                    }
                    if self.reader.is_open() {
                        let label = if self.reader_sidebar_open {
                            self.tr("收起目录", "Hide contents")
                        } else {
                            self.tr("展开目录", "Show contents")
                        };
                        toggle_sidebar = ui
                            .button(label)
                            .on_hover_text(
                                self.tr("显示或隐藏阅读侧栏", "Show or hide the reader sidebar"),
                            )
                            .clicked();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.night {
                            self.tr("日间模式", "Day mode")
                        } else {
                            self.tr("夜间模式", "Night mode")
                        };
                        if ui.button(label).clicked() {
                            toggle_theme = true;
                        }
                        if ui.button("更新").clicked() {
                            self.updates.open = true;
                        }
                    });
                });
                if let Some((notice, error)) = &self.notice {
                    let color = if *error { p.danger } else { p.success };
                    ui.add_space(2.0);
                    ui.label(RichText::new(notice).color(color).size(13.0));
                }
            });
        if toggle_theme {
            self.night = !self.night;
            theme::apply(ctx, self.night);
        }
        if toggle_sidebar {
            self.reader_sidebar_open = !self.reader_sidebar_open;
        }
        if generate_comic {
            if let Some(source) = self.reader.comic_source() {
                self.generator.prepare(source);
            }
        }
        if show_illustrations {
            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                let request = self
                    .illustrations
                    .open(book_id, self.reader.chapter_index());
                if !self.send(request) {
                    self.illustrations.fail("书库暂忙，请关闭后重试。".into());
                }
            }
        }
        let p = theme::palette(self.night);
        if back {
            self.reader.close();
            self.comic.close();
            self.illustrations.close();
            self.editing = None;
            self.opening = None;
        }
        let view = if self.comic.book_id().is_some() {
            View::Comic
        } else if self.reader.is_open() {
            View::Novel
        } else {
            View::Shelf
        };
        let time = ctx.input(|input| input.time);
        if self.view != view || toggle_theme {
            self.view = view;
            self.view_entered = time;
        }
        let fade = ((time - self.view_entered) / 0.16).clamp(0.0, 1.0) as f32;
        let opacity = 1.0 - 0.45 * (1.0 - fade).powi(3);
        if fade < 1.0 {
            ctx.request_repaint();
        }
        if view == View::Comic {
            let mut flip = None;
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                flip = self.comic.show(ui, self.localized);
            });
            if let Some((book_id, page_index)) = flip {
                if !self.send(Command::ReadComicPage {
                    book_id,
                    page_index,
                }) {
                    self.comic.reset_load();
                }
            }
        } else if view == View::Novel {
            let resizing = self.reader_sidebar(ctx);
            let dialog_open = self.editing.is_some()
                || self.generator.open
                || self.updates.open
                || self.illustrations.is_open();
            let mut chapter_step = None;
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                chapter_step = self.reader.show(ui, self.localized, resizing, dialog_open);
            });
            if let Some((chapter, last_page)) = chapter_step {
                if let Some((book_id, chapter)) = self.reader.prepare_adjacent(chapter, last_page) {
                    if !self.send(Command::ReadChapter { book_id, chapter }) {
                        self.reader.reset_load();
                    }
                }
            }
            if let Some((loc, end_off)) = self.reader.take_mark() {
                self.editing = Some(MarkEdit {
                    id: None,
                    loc,
                    end_off,
                    color: vm_store::AnnotationColor::Yellow,
                    note: String::new(),
                });
            }
            self.annotation_editor(ctx);
        } else {
            let shelf_frame = egui::Frame::new()
                .fill(p.bg)
                .inner_margin(egui::Margin::symmetric(20, 12));
            egui::CentralPanel::default()
                .frame(shelf_frame)
                .show(ctx, |ui| {
                    ui.multiply_opacity(opacity);
                    self.shelf(ui);
                });
        }
        if !self.importing && self.storage_ready {
            if let Some(request) = self.generator.show(ctx) {
                self.start_import(request);
            }
        }
        self.updates.show(ctx);
        if let Some(request) = self.illustrations.show(ctx) {
            if !self.send(request) {
                self.illustrations.fail("书库暂忙，请重试。".into());
            }
        }
    }
}

struct Bench {
    start: Instant,
    sender: Option<crossbeam_channel::Sender<(&'static str, f64, Option<f64>)>>,
}

impl Bench {
    fn new(start: Instant, path: Option<PathBuf>) -> Self {
        let sender =
            path.and_then(|path| {
                let (send, receive) =
                    crossbeam_channel::bounded::<(&'static str, f64, Option<f64>)>(32);
                std::thread::Builder::new().name("velmira-benchmark".into()).spawn(move || {
                use std::io::Write;
                let mut file = match std::fs::File::create(path) {
                    Ok(file) => file,
                    Err(error) => { eprintln!("Cannot write benchmark log: {error}"); return; }
                };
                for (phase, since_entry_ms, elapsed_ms) in receive {
                    let value = serde_json::json!({
                        "phase":phase, "since_entry_ms":since_entry_ms, "elapsed_ms":elapsed_ms,
                        "memory_bytes":memory::read(), "memory_metric":memory::METRIC,
                    });
                    if writeln!(file, "{value}").is_err() { break; }
                }
            }).ok()?;
                Some(send)
            });
        Self { start, sender }
    }

    fn enabled(&self) -> bool {
        self.sender.is_some()
    }

    fn mark(&self, phase: &'static str, elapsed_ms: Option<f64>) {
        if let Some(sender) = &self.sender {
            let since_entry_ms = self.start.elapsed().as_secs_f64() * 1000.0;
            let _ = sender.try_send((phase, since_entry_ms, elapsed_ms));
        }
    }
}

#[cfg(target_os = "windows")]
mod memory {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2},
        Threading::GetCurrentProcess,
    };
    pub const METRIC: &str = "private_working_set";
    pub fn read() -> Option<u64> {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&counters) as u32;
        counters.cb = size;
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2).cast(),
                size,
            )
        };
        (ok != 0).then_some(counters.PrivateWorkingSetSize as u64)
    }
}

#[cfg(not(target_os = "windows"))]
mod memory {
    #[cfg(target_os = "linux")]
    pub const METRIC: &str = "rss";
    #[cfg(not(target_os = "linux"))]
    pub const METRIC: &str = "unavailable";

    pub fn read() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}
