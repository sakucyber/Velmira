#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[path = "../../android/src/automatic_manga.rs"]
mod automatic_manga;
mod chapter_analysis;
mod comic;
mod covers;
mod generate;
mod illustrations;
mod model_settings;
mod opening;
mod pairing_cli;
mod plugins;
mod preferences;
mod reader;
mod reading_ai;
mod reading_preferences;
mod selection_bar;
mod settings;
mod theme;
mod ui;
mod updates;
mod workers;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use opening::{Opening, Returning};
use reader::Reader;
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use vm_core::{BookFormat, BookSummary, Loc};
use vm_store::{
    Bookmark, Command, Event, ReadingStatus, SearchHit, ShelfFilter, ShelfSort, ShelfStatus, Store,
};
use vm_update::Channel;
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
                flag @ ("--pair-listen" | "--pair-connect" | "--paired-devices" | "--unpair-device"
                | "--sync-listen" | "--sync-connect"),
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
                    "--sync-listen" | "--sync-connect" => {
                        let address = socket_address(args.next(), flag)?;
                        let peer = args
                            .next()
                            .and_then(|value| value.into_string().ok())
                            .filter(|value| !value.is_empty())
                            .ok_or("Synchronization needs a paired device ID.")?;
                        if flag == "--sync-listen" {
                            pairing_cli::Command::SyncListen(address, peer)
                        } else {
                            pairing_cli::Command::SyncConnect(address, peer)
                        }
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
                println!("velmira-native [--data-dir PATH] [--sync-listen IP:PORT PEER_ID | --sync-connect IP:PORT PEER_ID]");
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
    data_dir: PathBuf,
    pairing_invite: Option<pairing_cli::Invitation>,
    importer: ImportWorker,
    localized: bool,
    night: bool,
    reader_sidebar_open: bool,
    compact_shelf: bool,
    saved_preferences: preferences::SavedPreferences,
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
    companion: Companion,
    generator: generate::Generator,
    reading_ai: reading_ai::ReadingAi,
    analysis: chapter_analysis::Analysis,
    automatic_manga: automatic_manga::AutomaticManga,
    automatic_import: Option<tempfile::TempPath>,
    automatic_style: String,
    updates: updates::Updates,
    settings: settings::SettingsState,
    settings_open: bool,
    preferences_open: bool,
    plugins_open: bool,
    plugins: plugins::Registry,
    about_open: bool,
    illustrations: illustrations::Illustrations,
    covers: covers::ShelfCovers,
    bookmarks: Vec<Bookmark>,
    more_bookmarks: bool,
    editing: Option<MarkEdit>,
    storage_ready: bool,
    opening: Option<Opening>,
    returning: Option<Returning>,
    importing: bool,
    import_novel: Option<String>,
    startup_import: Option<PathBuf>,
    startup_legacy: Option<PathBuf>,
    notice: Option<(String, bool)>,
    search: String,
    toc_filter: String,
    focus_search: bool,
    last_query: Option<String>,
    search_hits: Vec<SearchHit>,
    bench: Bench,
    idle_sample: Option<Instant>,
    first_ui: bool,
    ui_preview: bool,
    pending_progress: std::collections::HashMap<String, Loc>,
    closing: bool,
    allow_close: bool,
}

struct MarkEdit {
    id: Option<(String, u32)>,
    ranges: Vec<(Loc, u32)>,
    color: vm_store::AnnotationColor,
    note: String,
}

struct Companion {
    picker: bool,
    books: Vec<BookSummary>,
    has_more: bool,
    listing: bool,
    pending: Option<String>,
    keyboard_focus: bool,
    source: Option<vm_core::ComicSource>,
    follow_novel: bool,
    interval_seconds: u8,
    next_page_at: Option<f64>,
}

impl Default for Companion {
    fn default() -> Self {
        Self {
            picker: false,
            books: Vec::new(),
            has_more: false,
            listing: false,
            pending: None,
            keyboard_focus: false,
            source: None,
            follow_novel: false,
            interval_seconds: 12,
            next_page_at: None,
        }
    }
}

enum CardAction {
    Open(String, egui::Rect),
    Rename(String, String),
    Status(String, ShelfStatus),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Shelf,
    Novel,
    Comic,
    Preferences,
    Settings,
    Plugins,
    About,
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
) -> egui::Rect {
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
                actions.push(CardAction::Open(
                    book.id.clone(),
                    cover.rect.intersect(ui.clip_rect()),
                ));
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
                actions.push(CardAction::Open(
                    book.id.clone(),
                    cover.rect.intersect(ui.clip_rect()),
                ));
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
            cover.rect.intersect(ui.clip_rect())
        },
    )
    .inner
}

impl App {
    fn new(
        ctx: egui::Context,
        start: Instant,
        options: Options,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let data_dir = options.data_dir.clone();
        let (saved_preferences, prefs) = preferences::SavedPreferences::load(&options.data_dir);
        theme::apply(&ctx, prefs.night);
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
        let updates = updates::Updates::new(start, false);
        let idle_sample = bench
            .enabled()
            .then(|| Instant::now() + Duration::from_secs(300));
        let mut reader = Reader::new(layout);
        reader.apply_preferences(prefs.font_size, prefs.paper.as_deref());
        let (font_size, paper) = reader.preferences();
        let settings = settings::SettingsState::new(font_size, paper);
        let model_pool = Arc::new(model_settings::SharedPool::default());
        Ok(Self {
            store,
            data_dir,
            pairing_invite: None,
            importer,
            localized: true,
            night: prefs.night,
            reader_sidebar_open: prefs.contents,
            compact_shelf: prefs.compact,
            saved_preferences,
            view: View::Shelf,
            view_entered: f64::NEG_INFINITY,
            book_list: Vec::new(),
            has_more: false,
            list_pending: false,
            shelf_filter: Default::default(),
            shelf_query: String::new(),
            renaming: None,
            reader,
            storage_ready: false,
            comic,
            companion: Companion::default(),
            generator: generate::Generator::new(&options.data_dir, Arc::clone(&model_pool)),
            reading_ai: reading_ai::ReadingAi::new(&options.data_dir, model_pool),
            analysis: chapter_analysis::Analysis::new(&options.data_dir),
            automatic_manga: automatic_manga::AutomaticManga::new(&options.data_dir),
            automatic_import: None,
            automatic_style: String::new(),
            updates,
            settings,
            settings_open: false,
            preferences_open: false,
            plugins_open: false,
            plugins: plugins::Registry::load(&options.data_dir).unwrap_or_default(),
            about_open: false,
            illustrations: Default::default(),
            covers,
            bookmarks: Vec::new(),
            more_bookmarks: false,
            editing: None,
            opening: None,
            returning: None,
            importing: false,
            import_novel: None,
            startup_import: options.import,
            startup_legacy: options.legacy,
            notice: None,
            search: String::new(),
            toc_filter: String::new(),
            focus_search: false,
            last_query: None,
            search_hits: Vec::new(),
            bench,
            idle_sample,
            first_ui: true,
            ui_preview: false,
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
                Ok(()) => {
                    self.importing = true;
                    self.import_novel = self.reader.book_id().map(str::to_owned);
                }
                Err(error) => self.say(error.to_string(), true),
            }
        }
    }

    fn open(&mut self, book_id: String, origin: Option<egui::Rect>) {
        if self.returning.is_some() {
            self.close_reader();
        }
        if self.opening.is_none()
            && self.send(Command::OpenBook {
                book_id: book_id.clone(),
            })
        {
            self.opening = Some(Opening::new(book_id, origin));
        }
    }

    fn open_companion(&mut self, book_id: String) {
        if self.companion.pending.is_some() {
            return;
        }
        if self.comic.book_id() == Some(book_id.as_str()) {
            self.companion.picker = false;
            return;
        }
        if let Some(novel_id) = self.reader.book_id().map(str::to_owned) {
            if self.send(Command::OpenCompanion {
                novel_id,
                book_id: book_id.clone(),
            }) {
                self.companion.pending = Some(book_id);
            }
        }
    }

    fn list_comics(&mut self, offset: u32) {
        if !self.companion.listing && self.send(Command::ListComics { offset }) {
            self.companion.listing = true;
        }
    }

    fn comic_picker(&mut self, ctx: &egui::Context) {
        if !self.companion.picker || !self.reader.is_open() {
            return;
        }
        let mut open = true;
        let mut selected = None;
        let mut more = false;
        egui::Window::new(self.tr("选择并排漫画", "Choose companion comic"))
            .id(egui::Id::new("companion-picker"))
            .default_width(390.0)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if self.companion.books.is_empty() && !self.companion.listing {
                    ui.label(self.tr(
                        "书架还没有 CBZ 漫画。可用「导入书籍」添加。",
                        "Import a CBZ comic to read it alongside the novel.",
                    ));
                }
                egui::ScrollArea::vertical()
                    .max_height(330.0)
                    .show(ui, |ui| {
                        for book in &self.companion.books {
                            let button = egui::Button::new(&book.title)
                                .truncate()
                                .min_size(egui::vec2(ui.available_width(), 34.0));
                            if ui
                                .add_enabled(self.companion.pending.is_none(), button)
                                .clicked()
                            {
                                selected = Some(book.id.clone());
                            }
                        }
                    });
                if self.companion.listing || self.companion.pending.is_some() {
                    ui.spinner();
                } else if self.companion.has_more {
                    more = ui.button(self.tr("加载更多", "Load more")).clicked();
                }
            });
        self.companion.picker = open;
        if !open {
            self.companion.pending = None;
        } else if let Some(book_id) = selected {
            self.open_companion(book_id);
        } else if more {
            self.list_comics(self.companion.books.len() as u32);
        }
    }

    fn show_comic_pane(&mut self, root: &mut egui::Ui, dialog_open: bool) -> Option<f32> {
        if self.comic.book_id().is_none() {
            self.companion.keyboard_focus = false;
            return None;
        }
        let width = root.available_width();
        let p = theme::palette(self.night);
        let press = root.input(|input| {
            input
                .pointer
                .any_pressed()
                .then(|| input.pointer.press_origin())
                .flatten()
        });
        if root.is_enabled()
            && !dialog_open
            && press.is_some_and(|point| root.max_rect().contains(point))
        {
            self.companion.keyboard_focus = false;
        }
        let mut close = false;
        let mut flip = None;
        let mut jump = None;
        let panel = egui::SidePanel::right("companion-comic-pane")
            .default_width(width * 0.43)
            .width_range((width * 0.32).clamp(170.0, 260.0)..=(width * 0.56).max(190.0))
            .frame(egui::Frame::new().fill(p.bg).inner_margin(12))
            .show_inside(root, |ui| {
                ui.set_min_width(ui.available_width());
                if !dialog_open
                    && ui.is_enabled()
                    && press.is_some_and(|point| ui.max_rect().contains(point))
                {
                    self.companion.keyboard_focus = true;
                    ui.ctx().memory_mut(|memory| {
                        if let Some(id) = memory.focused() {
                            memory.surrender_focus(id);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    close = ui.button(self.tr("关闭漫画", "Close comic")).clicked();
                    ui.label(
                        RichText::new(self.tr("并排漫画", "Companion comic"))
                            .size(12.0)
                            .color(p.muted),
                    );
                });
                if let Some(chapter) = self
                    .companion
                    .source
                    .as_ref()
                    .and_then(|source| source.chapters.get(self.comic.page_index() as usize))
                    .copied()
                {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                (chapter as usize) < self.reader.toc().len(),
                                egui::Button::new(format!(
                                    "跳到原文第 {} 章",
                                    u64::from(chapter) + 1
                                )),
                            )
                            .clicked()
                        {
                            jump = Some(chapter);
                        }
                        ui.checkbox(&mut self.companion.follow_novel, "跟随小说");
                    });
                    let now = ui.input(|input| input.time);
                    ui.horizontal_wrapped(|ui| {
                        let playing = self.companion.next_page_at.is_some();
                        if ui
                            .button(if playing {
                                "暂停播放"
                            } else {
                                "自动播放"
                            })
                            .clicked()
                        {
                            self.companion.next_page_at = (!playing)
                                .then_some(now + f64::from(self.companion.interval_seconds));
                            self.companion.follow_novel |= !playing;
                        }
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.companion.interval_seconds)
                                    .range(3..=90)
                                    .suffix(" 秒/页"),
                            )
                            .changed()
                            && playing
                        {
                            self.companion.next_page_at =
                                Some(now + f64::from(self.companion.interval_seconds));
                        }
                    });
                }
                flip = self.comic.show(
                    ui,
                    self.localized,
                    self.companion.keyboard_focus && !dialog_open,
                );
            });
        if close {
            self.comic.close();
            self.companion.pending = None;
            self.companion.keyboard_focus = false;
            self.companion.source = None;
            self.companion.next_page_at = None;
        } else if let Some((book_id, page_index)) = flip {
            self.companion.follow_novel = false;
            self.companion.next_page_at = None;
            self.flip_comic(book_id, page_index);
        } else if let Some(chapter) = jump {
            self.companion.keyboard_focus = false;
            self.jump(Loc {
                chapter,
                para: 0,
                off: 0,
            });
        } else if self.companion.follow_novel {
            let target = self.companion.source.as_ref().and_then(|source| {
                source.page_for_chapter(self.reader.chapter_index(), self.comic.page_index())
            });
            if let Some((book_id, index)) = target.and_then(|index| self.comic.request_page(index))
            {
                self.flip_comic(book_id, index);
            }
        }
        Some(panel.response.rect.left())
    }

    fn flip_comic(&mut self, book_id: String, page_index: u32) {
        if !self.send(Command::ReadComicPage {
            book_id,
            page_index,
        }) {
            self.comic.reset_load();
        }
    }

    fn show_selection_actions(&mut self, ctx: &egui::Context) {
        use selection_bar::Action;
        let Some(action) = self
            .reader
            .selection_anchor()
            .and_then(|anchor| selection_bar::show(ctx, anchor, self.localized))
        else {
            return;
        };
        match action {
            Action::Highlight(color) => {
                let ranges = self.reader.selection_ranges();
                if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                    if !ranges.is_empty()
                        && self.send(Command::AddAnnotations {
                            book_id,
                            ranges,
                            note: None,
                            color,
                        })
                    {
                        self.reader.clear_selection();
                    }
                }
            }
            Action::Note => {
                let ranges = self.reader.selection_ranges();
                if !ranges.is_empty() {
                    self.editing = Some(MarkEdit {
                        id: None,
                        ranges,
                        note: String::new(),
                        color: vm_store::AnnotationColor::Yellow,
                    });
                }
            }
            Action::Copy => {
                if let Some(text) = self.reader.selected_text() {
                    ctx.copy_text(text);
                    self.reader.clear_selection();
                }
            }
            Action::Translate | Action::Explain => {
                if let Some(text) = self.reader.selected_text() {
                    let action = if matches!(action, Action::Translate) {
                        vm_ai::reading::Action::Translate
                    } else {
                        vm_ai::reading::Action::Explain
                    };
                    self.reading_ai
                        .prepare_selection(text, self.localized, action);
                }
            }
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
        self.automatic_manga.poll();
        if self.storage_ready && !self.importing {
            if let Some(export) = self.automatic_manga.take_export() {
                let path = export.file.to_path_buf();
                if self.send(Command::ImportCbz { path }) {
                    self.importing = true;
                    self.import_novel = Some(export.book);
                    self.automatic_import = Some(export.file);
                }
            }
        }
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
                Ok(None) => {
                    self.importing = false;
                    self.import_novel = None;
                }
                Err(error) => {
                    self.importing = false;
                    self.import_novel = None;
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
                Err(error) => {
                    if self.opening.as_ref().is_some_and(|opening| opening.loaded)
                        && !self.reader.ready()
                    {
                        self.close_reader();
                    }
                    self.say(error, true);
                }
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
                Err(error) => {
                    if self.opening.as_ref().is_some_and(|opening| opening.loaded)
                        && !self.comic.ready()
                    {
                        self.close_reader();
                    }
                    self.say(error, true);
                }
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
            Event::Comics {
                offset,
                books,
                has_more,
            } => {
                self.companion.listing = false;
                if offset == 0 {
                    self.companion.books = books;
                } else if offset as usize == self.companion.books.len() {
                    self.companion.books.extend(books);
                }
                self.companion.has_more = has_more;
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
                self.automatic_import = None;
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
                let origin = self.import_novel.take();
                if book.format == BookFormat::Cbz && origin.is_some() {
                    if self.reader.book_id() == origin.as_deref()
                        && self.returning.is_none()
                        && self.opening.is_none()
                    {
                        self.open_companion(book.id);
                    }
                } else {
                    self.open(book.id, None);
                }
            }
            Event::BookOpened {
                book,
                toc,
                chapter_index,
                chapter,
            } => {
                let Some(opening) = self
                    .opening
                    .as_mut()
                    .filter(|opening| opening.book_id == book.id)
                else {
                    return;
                };
                opening.loaded = true;
                self.comic.close();
                self.companion = Companion::default();
                self.illustrations.close();
                self.bookmarks.clear();
                self.more_bookmarks = false;
                self.editing = None;
                self.search.clear();
                self.toc_filter.clear();
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
                self.load_inline_illustrations();
            }
            Event::ComicOpened { book, page } => {
                let Some(opening) = self
                    .opening
                    .as_mut()
                    .filter(|opening| opening.book_id == book.id)
                else {
                    return;
                };
                opening.loaded = true;
                self.reader.close();
                self.companion = Companion::default();
                self.illustrations.close();
                self.editing = None;
                self.bench.mark("comic_opened", None);
                self.comic.open(book, page);
            }
            Event::CompanionOpened {
                novel_id,
                book,
                page,
                source,
            } => {
                if self.reader.book_id() == Some(novel_id.as_str())
                    && self.companion.pending.as_deref() == Some(book.id.as_str())
                    && self.returning.is_none()
                {
                    self.companion.pending = None;
                    self.companion.picker = false;
                    self.companion.keyboard_focus = false;
                    self.companion.follow_novel = source.is_some();
                    self.companion.next_page_at = None;
                    self.companion.source = source;
                    self.comic.open(book, page);
                }
            }
            Event::ComicPageRead { book_id, page } => self.comic.loaded(&book_id, page),
            Event::Illustrations {
                book_id,
                chapter,
                illustrations,
            } => {
                self.reader
                    .inline_illustrations(&book_id, chapter, &illustrations);
                self.illustrations.listed(&book_id, chapter, illustrations);
            }
            Event::IllustrationRead {
                book_id,
                chapter,
                ordinal,
                image,
            } => {
                self.reader
                    .receive_inline_image(ctx, &book_id, chapter, ordinal, &image);
                self.illustrations
                    .loaded(ctx, &book_id, chapter, ordinal, image);
            }
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
                    self.load_inline_illustrations();
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
            Event::AnnotationsAdded { book_id, chapter }
            | Event::AnnotationDeleted {
                book_id, chapter, ..
            } => self.refresh_annotations(book_id, chapter),
            Event::Error { operation, message } => {
                match operation {
                    "open_book" => self.opening = None,
                    "open_companion" => self.companion.pending = None,
                    "list_comics" => self.companion.listing = false,
                    "read_chapter" => self.reader.reset_load(),
                    "read_comic_page" => self.comic.reset_load(),
                    "read_cover" => self.covers.read_failed(),
                    "list_illustrations" | "read_illustration" => {
                        self.illustrations.fail(message.clone());
                        if operation == "list_illustrations" {
                            self.reader.inline_illustrations_failed();
                        } else {
                            self.reader.inline_image_failed();
                        }
                    }
                    "insert_book" => {
                        self.importing = false;
                        self.automatic_import = None;
                        self.import_novel = None;
                    }
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

    fn load_inline_illustrations(&mut self) {
        if self.reader.is_epub() {
            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                if !self.send(Command::ListIllustrations {
                    book_id,
                    chapter: self.reader.chapter_index(),
                }) {
                    self.reader.inline_illustrations_failed();
                }
            }
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
            let reading_options = [
                (ReadingStatus::All, self.tr("全部进度", "All progress")),
                (ReadingStatus::Unread, self.tr("未读", "Unread")),
                (ReadingStatus::Reading, self.tr("阅读中", "In progress")),
                (
                    ReadingStatus::LastChapter,
                    self.tr("读到末章", "At final chapter"),
                ),
            ];
            egui::ComboBox::from_id_salt("shelf-reading")
                .selected_text(
                    reading_options
                        .iter()
                        .find(|(status, _)| *status == filter.reading)
                        .map_or("", |(_, label)| *label),
                )
                .show_ui(ui, |ui| {
                    for (status, label) in reading_options {
                        ui.selectable_value(&mut filter.reading, status, label);
                    }
                });
            let compact = self.tr("紧凑书架", "Compact shelf");
            ui.checkbox(&mut self.compact_shelf, compact);
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
            if ui.is_enabled()
                && !ui.ctx().wants_keyboard_input()
                && ui.input(|input| input.key_pressed(egui::Key::Slash))
            {
                field.request_focus();
            }
            if (ui.is_enabled()
                && field.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
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
            let (title, message) = if self.list_pending {
                (
                    self.tr("正在加载书架", "Opening library"),
                    self.tr("正在读取本地书籍…", "Reading local books…"),
                )
            } else if self.storage_ready && self.shelf_filter != ShelfFilter::default() {
                (
                    self.tr("没有匹配结果", "No matches"),
                    self.tr(
                        "试试清除筛选或更换关键词。",
                        "Clear filters or try another title.",
                    ),
                )
            } else if self.storage_ready {
                (
                    self.tr("书架还是空的", "Your shelf is empty"),
                    self.tr(
                        "导入 TXT、EPUB 或 CBZ，开始本地阅读。",
                        "Import a TXT, EPUB, or CBZ to start reading.",
                    ),
                )
            } else {
                (
                    self.tr("正在打开书库", "Opening library"),
                    self.tr("正在初始化本地数据库…", "Initializing the local database…"),
                )
            };
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui::empty_state(ui, p, title, message);
                if self.storage_ready && self.shelf_filter == ShelfFilter::default() {
                    ui.add_space(12.0);
                    if ui::primary_button(
                        ui,
                        p,
                        theme::Tokens::desktop(),
                        self.tr("导入书籍", "Import book"),
                    )
                    .clicked()
                    {
                        self.start_import(ImportRequest::Pick);
                    }
                }
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
        let (card_w, cover_h, card_h, gap) = if self.compact_shelf {
            (112.0, 168.0, 282.0, 13.0)
        } else {
            (148.0, 222.0, 347.0, 19.0)
        };
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
                                    let cover = ui
                                        .push_id(&book.id, |ui| {
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
                                        })
                                        .inner;
                                    if let Some(returning) = &mut self.returning {
                                        returning.cover(&book.id, cover);
                                    }
                                }
                            })
                        });
                    }
                });
        });
        for action in actions {
            match action {
                CardAction::Open(id, rect) => self.open(id, Some(rect)),
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

    fn reader_sidebar(&mut self, root: &mut egui::Ui) -> bool {
        let ctx = root.ctx().clone();
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
        let mut search_jump = None;
        let mut chapter_search = None;
        let mut submit = false;
        let mut bookmark = false;
        let mut remove_bookmark = None;
        let mut load_bookmarks = false;
        let mut edit = None;
        let sidebar_frame = egui::Frame::new().fill(theme::palette(self.night).surface);
        let max_width = (root.available_width() * 0.5).clamp(220.0, 420.0);
        let stable_panel = egui::panel::PanelState::load(&ctx, id);
        let expanded_width = stable_panel
            .map_or(240.0, |state| state.rect.width())
            .clamp(220.0, max_width);
        let mut panel = egui::SidePanel::left(id)
            .resizable(root.is_enabled())
            .default_width(240.0)
            .width_range(220.0..=max_width)
            .frame(sidebar_frame);
        if animating {
            panel = panel
                .resizable(false)
                .exact_width(expansion * expanded_width);
        }
        panel.show_inside(root, |panel_ui| {
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
                if std::mem::take(&mut self.focus_search) {
                    field.request_focus();
                }
                let enter =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                submit = enter || ui.button(self.tr("搜全书", "Search book")).clicked();
                if ui.button(self.tr("搜本章", "Search chapter")).clicked() {
                    chapter_search = Some(self.reader.chapter_index());
                    submit = true;
                }
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
                                search_jump = Some(hit.loc);
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
            let toc_hint = if self.localized {
                "筛选目录"
            } else {
                "Filter contents"
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.toc_filter)
                    .hint_text(toc_hint)
                    .char_limit(120),
            );
            let query = self.toc_filter.trim().to_lowercase();
            let filtered: Vec<_> = if query.is_empty() {
                Vec::new()
            } else {
                self.reader
                    .toc()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, title)| {
                        title.to_lowercase().contains(&query).then_some(index)
                    })
                    .collect()
            };
            if !query.is_empty() && filtered.is_empty() {
                ui.label(self.tr("没有匹配章节", "No matching chapters"));
            }
            let length = if query.is_empty() {
                self.reader.toc().len()
            } else {
                filtered.len()
            };
            egui::ScrollArea::vertical()
                .id_salt(("toc", self.toc_filter.as_str()))
                .show_rows(ui, 32.0, length, |ui, rows| {
                    for row in rows {
                        let index = if query.is_empty() { row } else { filtered[row] };
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
                });
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
                ranges: Vec::new(),
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
                    let chapter = chapter_search.or_else(|| {
                        (query.chars().count() == 1).then(|| self.reader.chapter_index())
                    });
                    self.send(Command::Search {
                        query,
                        book_id,
                        chapter,
                        limit: 50,
                    });
                }
            }
        }
        if let Some(loc) = search_jump {
            if let Some(query) = &self.last_query {
                self.reader.highlight_search(loc, query);
            }
            self.jump(loc);
        } else if let Some(loc) = jump {
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
                Command::AddAnnotations {
                    book_id,
                    ranges: mark.ranges.clone(),
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
        } else if sent {
            self.reader.clear_selection();
        }
    }

    fn close_reader(&mut self) {
        self.close_overlays();
        self.reader.close();
        self.comic.close();
        self.companion = Companion::default();
        self.editing = None;
        self.opening = None;
        self.returning = None;
    }

    fn close_pages(&mut self) {
        self.settings_open = false;
        self.preferences_open = false;
        self.plugins_open = false;
        self.about_open = false;
    }

    fn close_overlays(&mut self) {
        self.reading_ai.close();
        self.generator.open = false;
        self.updates.open = false;
        self.analysis.close();
        self.automatic_manga.visible = false;
        self.illustrations.close();
        self.companion.picker = false;
    }

    fn overlay_open(&self) -> bool {
        self.reading_ai.is_open()
            || self.generator.open
            || self.updates.open
            || self.analysis.is_open()
            || self.automatic_manga.visible
            || self.illustrations.is_open()
            || self.companion.picker
    }

    fn return_to_shelf(&mut self, ctx: &egui::Context) {
        self.close_overlays();
        self.close_pages();
        let book_id = if self.reader.ready() {
            self.reader.book_id()
        } else if self.comic.ready() {
            self.comic.book_id()
        } else {
            None
        };
        if let Some(book_id) = book_id {
            self.opening = None;
            self.returning = Some(Returning::new(book_id.to_owned()));
        } else {
            self.close_reader();
        }
        ctx.request_repaint();
    }

    fn set_global_theme(&mut self, ctx: &egui::Context, night: bool) {
        self.night = night;
        theme::apply(ctx, night);
        let font = self.settings.font_size;
        self.reader.apply_preferences(font, None);
        self.settings.paper = None;
    }

    fn set_reading_theme(&mut self, ctx: &egui::Context, paper: String) {
        self.night = paper == "dark";
        theme::apply(ctx, self.night);
        self.reader
            .apply_preferences(self.settings.font_size, Some(&paper));
        self.settings.paper = Some(paper);
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        let action = settings::show(ui, &mut self.settings, self.night);
        match action {
            Some(settings::Action::Pair) => match pairing_cli::invite(&self.data_dir) {
                Ok(invite) => {
                    if let Err(error) = self
                        .settings
                        .set_invitation(&invite.payload, invite.expires)
                    {
                        self.say(error, true);
                    } else {
                        self.pairing_invite = Some(invite);
                    }
                }
                Err(error) => self.say(error.to_string(), true),
            },
            Some(settings::Action::Revoke(id)) => {
                if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    match pairing_cli::revoke(&self.data_dir, &id) {
                        Ok(()) => {
                            self.settings.refresh_devices(&self.data_dir);
                            self.say("设备已撤销。".into(), false);
                        }
                        Err(error) => self.say(error.to_string(), true),
                    }
                } else {
                    self.say("该设备尚未连接。".into(), false);
                }
            }
            None => {}
        }
    }

    fn show_preferences(&mut self, ui: &mut egui::Ui) {
        let action = reading_preferences::show(
            ui,
            self.night,
            self.localized,
            &mut self.settings.font_size,
            &mut self.settings.paper,
            &mut self.settings.page_style,
            &mut self.settings.fit_width,
        );
        match action {
            Some(reading_preferences::Action::Font(font)) => {
                self.reader
                    .apply_preferences(font, self.settings.paper.as_deref());
            }
            Some(reading_preferences::Action::Paper(paper)) => {
                self.set_reading_theme(ui.ctx(), paper);
            }
            Some(reading_preferences::Action::PageStyle(style)) => self.settings.page_style = style,
            Some(reading_preferences::Action::FitWidth(fit)) => {
                self.settings.fit_width = fit;
                self.comic.set_fit_width(fit);
            }
            None => {}
        }
    }

    fn show_plugins(&mut self, ui: &mut egui::Ui) {
        match plugins::show(ui, &mut self.plugins, &self.data_dir, self.night) {
            Some(plugins::PluginAction::Import) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Velmira 插件", &["json"])
                    .pick_file()
                {
                    match self.plugins.import(&self.data_dir, &path) {
                        Ok(()) => self.say("插件已导入并启用。".into(), false),
                        Err(error) => self.say(error, true),
                    }
                }
            }
            Some(plugins::PluginAction::Toggle(id, enabled)) => {
                match self.plugins.set_enabled(&self.data_dir, &id, enabled) {
                    Ok(()) => {}
                    Err(error) => self.say(error, true),
                }
            }
            Some(plugins::PluginAction::Remove(id)) => {
                match self.plugins.remove(&self.data_dir, &id) {
                    Ok(()) => self.say("插件已删除。".into(), false),
                    Err(error) => self.say(error, true),
                }
            }
            None => {}
        }
    }

    fn show_about(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        ui::section_header(ui, p, "关于 Velmira", Some("本地优先的小说与漫画阅读器"));
        ui.label(format!("版本 0.1.0 · {}", env!("CARGO_PKG_VERSION")));
        ui.label("Rust · egui · SQLite");
        ui.label("MIT 许可证");
        ui.separator();
        ui.label("书籍、阅读记录、模型配置和配对信息默认只保存在本机。");
        ui.label("AI 功能仅在发送请求时联系配置的服务。");
        ui.separator();
        ui::section_header(ui, p, "更新", Some("首启不联网；可随时手动检查。"));
        let mut channel = self.updates.channel();
        let mut automatic = self.updates.automatic();
        ui.horizontal(|ui| {
            if ui
                .selectable_value(&mut channel, Channel::Stable, "稳定版")
                .changed()
            {
                self.updates.set_channel(Channel::Stable);
            }
            if ui
                .selectable_value(&mut channel, Channel::Beta, "测试版")
                .changed()
            {
                self.updates.set_channel(Channel::Beta);
            }
        });
        if ui
            .checkbox(&mut automatic, "本次运行自动检查更新")
            .changed()
        {
            self.updates.set_automatic(automatic);
        }
        if ui.button("检查更新").clicked() {
            self.close_overlays();
            self.updates.open = true;
        }
    }

    fn show_scene(&mut self, root: &mut egui::Ui, view: View) {
        let ctx = root.ctx().clone();
        let wide_nav =
            root.available_width() >= 900.0 && self.opening.is_none() && self.returning.is_none();
        if wide_nav {
            let active = match view {
                View::Shelf | View::Novel => ui::NavAction::Library,
                View::Comic => ui::NavAction::Manga,
                View::Preferences => ui::NavAction::Preferences,
                View::Settings => ui::NavAction::Settings,
                View::Plugins => ui::NavAction::Plugins,
                View::About => ui::NavAction::About,
            };
            let mut selected = None;
            let mut open_ai = false;
            egui::SidePanel::left("velmira-navigation")
                .exact_width(196.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::palette(self.night).surface)
                        .inner_margin(egui::Margin::symmetric(10, 12)),
                )
                .show_inside(root, |ui| {
                    selected = ui::nav_rail(
                        ui,
                        theme::palette(self.night),
                        theme::Tokens::desktop(),
                        active,
                        self.localized,
                    );
                    ui.add_space(12.0);
                    open_ai = ui::secondary_button(
                        ui,
                        theme::palette(self.night),
                        theme::Tokens::desktop(),
                        "阅读 AI",
                    )
                    .clicked();
                });
            if open_ai {
                self.close_overlays();
                self.close_pages();
                let source = self
                    .reader
                    .selected_text()
                    .or_else(|| self.reader.current_paragraph())
                    .unwrap_or_default();
                self.reading_ai.prepare(source, self.localized);
            }
            match selected {
                Some(ui::NavAction::Library) => {
                    self.close_overlays();
                    self.close_pages();
                    self.return_to_shelf(&ctx);
                }
                Some(ui::NavAction::Manga) => {
                    self.close_overlays();
                    self.close_pages();
                    self.generator.open = true;
                }
                Some(ui::NavAction::Models) => {
                    self.close_overlays();
                    self.close_pages();
                    self.generator.open_config();
                }
                Some(ui::NavAction::Preferences) => {
                    self.close_overlays();
                    self.close_pages();
                    self.preferences_open = true;
                }
                Some(ui::NavAction::Settings) => {
                    self.close_overlays();
                    self.close_pages();
                    self.settings.refresh_devices(&self.data_dir);
                    self.settings_open = true;
                }
                Some(ui::NavAction::Plugins) => {
                    self.close_overlays();
                    self.close_pages();
                    self.plugins_open = true;
                }
                Some(ui::NavAction::About) => {
                    self.close_overlays();
                    self.close_pages();
                    self.about_open = true;
                }
                None => {}
            }
        }
        let p = theme::palette(self.night);
        let mut import_request = None;
        if self.generator.open {
            egui::SidePanel::right("manga-project-dock")
                .width_range(420.0..=680.0)
                .default_width(560.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show_inside(root, |ui| {
                    import_request = self.generator.show(ui);
                });
        } else if self.reading_ai.is_open() {
            egui::SidePanel::right("reading-ai-dock")
                .width_range(400.0..=560.0)
                .default_width(460.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show_inside(root, |ui| self.reading_ai.show(ui));
        } else if self.automatic_manga.visible {
            let mut style = self.automatic_style.clone();
            egui::SidePanel::right("automatic-manga-dock")
                .width_range(420.0..=620.0)
                .default_width(500.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.surface)
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show_inside(root, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("自动漫画项目").strong().size(18.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                self.automatic_manga.visible = false;
                            }
                        });
                    });
                    ui.label("画风");
                    ui.add(egui::TextEdit::singleline(&mut style).char_limit(400));
                    self.automatic_manga
                        .show(ui, vm_ai::manga::Layout::Automatic, &style, || {
                            self.reading_ai.text_service()
                        });
                });
            self.automatic_style = style;
        }
        let mut back = false;
        let mut toggle_theme = false;
        let mut generate_comic = false;
        let mut open_reading_ai = false;
        let mut open_preferences = false;
        let mut open_auto = false;
        let mut open_companion = false;
        let mut open_analysis = false;
        let mut show_illustrations = false;
        let mut toggle_sidebar = false;
        let mut close_all = false;
        let p = theme::palette(self.night);
        let contents_available = view == View::Novel
            && (self.comic.book_id().is_none() || root.available_width() >= 960.0);
        let toolbar_frame = egui::Frame::new()
            .fill(p.surface)
            .inner_margin(egui::Margin::symmetric(16, 10));
        egui::TopBottomPanel::top(if view == View::Shelf {
            "shelf-toolbar"
        } else {
            "reader-toolbar"
        })
        .frame(toolbar_frame)
        .show_inside(root, |ui| {
            if matches!(
                view,
                View::Preferences | View::Settings | View::Plugins | View::About
            ) {
                ui.horizontal(|ui| {
                    if ui.button(self.tr("返回书架", "Library")).clicked() {
                        back = true;
                    }
                    ui.separator();
                    let title = match view {
                        View::Preferences => "阅读设置",
                        View::Settings => "离线与同步",
                        View::Plugins => "插件",
                        View::About => "关于",
                        _ => unreachable!(),
                    };
                    ui.label(RichText::new(title).strong().color(p.ink));
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                let (logo, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                ui.painter().rect_filled(logo, 8.0, p.surface2);
                ui.painter().text(
                    logo.center(),
                    egui::Align2::CENTER_CENTER,
                    "V",
                    egui::FontId::proportional(16.0),
                    p.ink,
                );
                ui.add_space(2.0);
                ui.label(RichText::new("Velmira").size(20.0).strong().color(p.ink));
                ui.add_space(6.0);
                if view != View::Shelf && ui.button(self.tr("返回书架", "Library")).clicked() {
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
                if !wide_nav {
                    ui.menu_button(self.tr("设置", "Settings"), |ui| {
                        if ui.button(self.tr("阅读设置", "Reading settings")).clicked() {
                            self.close_overlays();
                            self.close_pages();
                            self.preferences_open = true;
                        }
                        if ui.button(self.tr("离线与同步", "Offline & sync")).clicked() {
                            self.close_overlays();
                            self.close_pages();
                            self.settings.refresh_devices(&self.data_dir);
                            self.settings_open = true;
                        }
                        if ui.button(self.tr("插件", "Plugins")).clicked() {
                            self.close_overlays();
                            self.close_pages();
                            self.plugins_open = true;
                        }
                        if ui.button(self.tr("关于", "About")).clicked() {
                            self.close_overlays();
                            self.close_pages();
                            self.about_open = true;
                        }
                    });
                }
                if view == View::Novel && ui.button(self.tr("阅读 AI", "Reading AI")).clicked() {
                    open_reading_ai = true;
                }
                if view == View::Novel {
                    ui.menu_button(self.tr("更多", "More"), |ui| {
                        if ui.button("新建漫画项目").clicked() {
                            generate_comic = true;
                        }
                        if ui.button("自动漫画项目").clicked() {
                            open_auto = true;
                        }
                        if ui
                            .button(self.tr("并排阅读", "Read side by side"))
                            .clicked()
                        {
                            open_companion = true;
                        }
                        if ui.button(self.tr("章节分析", "Analyze chapter")).clicked() {
                            open_analysis = true;
                        }
                        if self.reader.is_epub() && ui.button("插图").clicked() {
                            show_illustrations = true;
                        }
                        if contents_available {
                            let label = if self.reader_sidebar_open {
                                self.tr("收起目录", "Hide contents")
                            } else {
                                self.tr("展开目录", "Show contents")
                            };
                            if ui.button(label).clicked() {
                                toggle_sidebar = true;
                            }
                        }
                        if ui.button(self.tr("阅读设置", "Reading settings")).clicked() {
                            open_preferences = true;
                        }
                    });
                }
                if self.overlay_open() && ui.button(self.tr("收起全部", "Close all")).clicked()
                {
                    close_all = true;
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
                });
            });
            if let Some((notice, error)) = &self.notice {
                let color = if *error { p.danger } else { p.success };
                ui.add_space(2.0);
                ui.label(RichText::new(notice).color(color).size(13.0));
            }
        });
        if toggle_theme {
            self.set_global_theme(&ctx, !self.night);
            ctx.request_repaint();
        }
        if toggle_sidebar {
            self.reader_sidebar_open = !self.reader_sidebar_open;
        }
        if close_all {
            self.close_overlays();
        }
        if generate_comic {
            self.close_overlays();
            if let (Some(book), Some(source)) = (self.reader.book_id(), self.reader.comic_source())
            {
                self.generator
                    .prepare(book.to_owned(), self.reader.chapter_index(), source);
            }
        }
        if open_reading_ai {
            self.close_overlays();
            if let Some(source) = self
                .reader
                .selected_text()
                .or_else(|| self.reader.current_paragraph())
            {
                self.reading_ai.prepare(source, self.localized);
            }
        }
        if open_preferences {
            self.close_overlays();
            self.close_pages();
            self.preferences_open = true;
        }
        if open_auto {
            self.close_overlays();
            if let Some(book) = self.reader.book_id() {
                let index = self.reader.chapter_index();
                if self.automatic_manga.open(
                    &ctx,
                    book,
                    self.reader.book_title(),
                    index,
                    self.reader.toc().len() as u32,
                ) {
                    self.automatic_manga.visible = true;
                } else {
                    self.say("另一个自动漫画项目仍在运行。".into(), true);
                }
            }
        }
        if open_companion {
            self.close_overlays();
            self.companion.picker = true;
            self.list_comics(0);
        }
        if open_analysis {
            self.close_overlays();
            let index = self.reader.chapter_index();
            if let Some(book) = self.reader.book_id() {
                let title = self
                    .reader
                    .toc()
                    .get(index as usize)
                    .map_or("", String::as_str);
                self.analysis.prepare(
                    book,
                    self.reader.loc().unwrap_or(Loc {
                        chapter: index,
                        ..Loc::default()
                    }),
                    title,
                );
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
        if view == View::Preferences {
            egui::CentralPanel::default().show_inside(root, |ui| self.show_preferences(ui));
        } else if view == View::Settings {
            egui::CentralPanel::default().show_inside(root, |ui| self.show_settings(ui));
        } else if view == View::Plugins {
            egui::CentralPanel::default().show_inside(root, |ui| self.show_plugins(ui));
        } else if view == View::About {
            egui::CentralPanel::default().show_inside(root, |ui| self.show_about(ui));
        } else if view == View::Comic {
            let mut flip = None;
            egui::CentralPanel::default().show_inside(root, |ui| {
                flip = self.comic.show(ui, self.localized, true);
            });
            if let Some((book_id, page_index)) = flip {
                self.flip_comic(book_id, page_index);
            }
        } else if view == View::Novel {
            let dialog_open = !root.is_enabled()
                || self.editing.is_some()
                || self.generator.open
                || self.updates.open
                || self.illustrations.is_open()
                || self.automatic_manga.visible;
            let dialog_open = dialog_open
                || self.reading_ai.is_open()
                || self.companion.picker
                || self.analysis.is_open();
            let novel_right = self.show_comic_pane(root, dialog_open);
            let mut novel_ui;
            let root = if let Some(right) = novel_right {
                let mut bounds = root.available_rect_before_wrap();
                bounds.set_right(right);
                novel_ui = root.new_child(
                    egui::UiBuilder::new()
                        .id_salt("split-novel-pane")
                        .max_rect(bounds),
                );
                novel_ui.set_clip_rect(bounds.intersect(root.clip_rect()));
                &mut novel_ui
            } else {
                root
            };
            let reader_keys = !self.companion.keyboard_focus;
            if reader_keys
                && !dialog_open
                && !egui::Popup::is_any_open(&ctx)
                && (!ctx.wants_keyboard_input() || self.reader.navigation_has_focus(&ctx))
            {
                let (contents, search, bookmark) = ctx.input_mut(|input| {
                    let plain = egui::Modifiers::NONE;
                    (
                        input.consume_key(plain, egui::Key::T),
                        input.consume_key(plain, egui::Key::F),
                        input.consume_key(plain, egui::Key::B),
                    )
                });
                if contents {
                    self.reader_sidebar_open = !self.reader_sidebar_open;
                }
                if search {
                    self.reader_sidebar_open = true;
                    self.focus_search = true;
                }
                if bookmark {
                    if let (Some(book_id), Some(loc)) = (self.reader.book_id(), self.reader.loc()) {
                        self.send(Command::AddBookmark {
                            book_id: book_id.to_owned(),
                            loc,
                            label: None,
                        });
                    }
                }
            }
            let resizing = if novel_right.is_none() || root.available_width() >= 560.0 {
                self.reader_sidebar(root)
            } else {
                false
            };
            let mut chapter_step = None;
            egui::CentralPanel::default().show_inside(root, |ui| {
                chapter_step =
                    self.reader
                        .show(ui, self.localized, resizing, dialog_open, reader_keys);
            });
            if let Some((chapter, last_page)) = chapter_step {
                if let Some((book_id, chapter)) = self.reader.prepare_adjacent(chapter, last_page) {
                    if !self.send(Command::ReadChapter { book_id, chapter }) {
                        self.reader.reset_load();
                    }
                }
            }
            if !dialog_open && chapter_step.is_none() {
                let now = ctx.input(|input| input.time);
                if let Some(due) = self.companion.next_page_at {
                    if now >= due {
                        let (more, step) = self.reader.advance_automatically();
                        self.companion.next_page_at =
                            more.then_some(now + f64::from(self.companion.interval_seconds));
                        if let Some((chapter, last_page)) = step {
                            if let Some((book_id, chapter)) =
                                self.reader.prepare_adjacent(chapter, last_page)
                            {
                                if !self.send(Command::ReadChapter { book_id, chapter }) {
                                    self.reader.reset_load();
                                }
                            }
                        }
                    }
                    if let Some(next) = self.companion.next_page_at {
                        ctx.request_repaint_after(Duration::from_secs_f64((next - now).max(0.0)));
                    }
                }
            }
            if !dialog_open {
                self.show_selection_actions(&ctx);
            }
        } else {
            let shelf_frame = egui::Frame::new()
                .fill(p.bg)
                .inner_margin(egui::Margin::symmetric(20, 12));
            egui::CentralPanel::default()
                .frame(shelf_frame)
                .show_inside(root, |ui| {
                    self.shelf(ui);
                });
        }
        if let Some(request) = import_request {
            self.start_import(request);
        }
        if back {
            self.return_to_shelf(&ctx);
        }
    }
    fn draw(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F12)) {
            self.ui_preview = !self.ui_preview;
        }
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
        let pairing_result = self
            .pairing_invite
            .as_ref()
            .and_then(|invite| invite.poll());
        if let Some(result) = pairing_result {
            self.pairing_invite = None;
            match result {
                Ok(id) => {
                    self.settings.refresh_devices(&self.data_dir);
                    self.say(format!("已配对设备 {id}"), false);
                }
                Err(error) => self.say(error, true),
            }
        }
        let closing = ctx.input(|input| input.viewport().close_requested());
        let (font_size, paper) = self.reader.preferences();
        self.settings.font_size = font_size;
        self.settings.paper = paper.map(str::to_owned);
        self.saved_preferences.update(
            ctx,
            preferences::Preferences {
                night: self.night,
                contents: self.reader_sidebar_open,
                compact: self.compact_shelf,
                font_size,
                paper: paper.map(str::to_owned),
            },
            closing,
        );
        if closing
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
        let view = if self.preferences_open {
            View::Preferences
        } else if self.settings_open {
            View::Settings
        } else if self.plugins_open {
            View::Plugins
        } else if self.about_open {
            View::About
        } else if self.reader.is_open() {
            View::Novel
        } else if self.comic.book_id().is_some() {
            View::Comic
        } else {
            View::Shelf
        };
        let ready = self.reader.ready() || self.comic.ready();
        if self.opening.is_some()
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.opening.as_ref().is_some_and(|open| open.loaded) {
                self.close_reader();
                ctx.request_repaint();
                self.show_scene(
                    &mut opening::scene(ctx, egui::Id::new("shelf-scene"), true, ctx.screen_rect()),
                    View::Shelf,
                );
                return;
            }
            self.opening = None;
        }
        let preparing = !ready && self.opening.as_ref().is_some_and(|open| open.loaded);
        let reveal = Opening::frame(&mut self.opening, ctx, ready, ctx.screen_rect());
        let returning = self.returning.is_some();
        let shown = if preparing || reveal.is_some() || returning {
            View::Shelf
        } else {
            view
        };
        let now = ctx.input(|input| input.time);
        if self.view != shown {
            if matches!(
                shown,
                View::Shelf | View::Preferences | View::Settings | View::Plugins | View::About
            ) {
                self.close_overlays();
            }
            self.view_entered = now;
            self.view = shown;
        }
        if shown == View::Shelf {
            let mut ui = opening::scene(
                ctx,
                egui::Id::new("shelf-scene"),
                reveal.is_none() && !returning,
                ctx.screen_rect(),
            );
            self.show_scene(&mut ui, View::Shelf);
        }
        let return_frame = self
            .returning
            .as_mut()
            .and_then(|motion| motion.frame(ctx, ctx.screen_rect()));
        if returning && return_frame.is_none() {
            self.close_reader();
            ctx.request_repaint();
        }
        if view != View::Shelf && (!returning || return_frame.is_some()) {
            let mut ui = opening::scene(
                ctx,
                egui::Id::new("reader-scene"),
                !preparing && reveal.is_none() && !returning,
                ctx.screen_rect(),
            );
            if preparing {
                ui.set_invisible();
            }
            if let Some(reveal) = reveal.or(return_frame) {
                reveal.paint(&mut ui, theme::palette(self.night).bg, |ui| {
                    self.show_scene(ui, view)
                });
            } else {
                self.show_scene(&mut ui, view);
            }
        }
        self.annotation_editor(ctx);
        self.comic_picker(ctx);
        self.updates.show(ctx);
        if self.ui_preview {
            egui::Window::new("UI 验证页 · F12")
                .default_width(620.0)
                .show(ctx, |ui| ui::validation_page(ui, self.night));
        }
        if let Some((book, loc)) = self.analysis.show(ctx, || self.reading_ai.text_service()) {
            if self.reader.book_id() == Some(book.as_str()) {
                self.jump(loc);
            }
        }
        if let Some(request) = self.reader.inline_image_request() {
            if !self.send(request) {
                self.reader.inline_image_failed();
            }
        }
        if let Some(request) = self.illustrations.show(ctx) {
            if !self.send(request) {
                self.illustrations.fail("书库暂忙，请重试。".into());
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.draw(ctx);
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
