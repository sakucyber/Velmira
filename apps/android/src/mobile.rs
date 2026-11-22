use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use android_activity::AndroidApp;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, RichText};
use vm_core::{BookFormat, BookSummary, Loc};
use vm_store::{
    Annotation, AnnotationColor, Bookmark, Command, CoverSize, Event, ReadingStatus, SearchHit,
    ShelfFilter, ShelfSort, ShelfStatus, Store,
};

use crate::{
    automatic_manga::AutomaticManga,
    comic::Comic,
    covers::ShelfCovers,
    mobile_ui::{self, BOTTOM_BAR_HEIGHT, TOP_BAR_HEIGHT},
    models::Models,
    opening::{self, Opening},
    peer_sync::PeerSync,
    preferences::{Preferences, SavedPreferences},
    reader::Reader,
    saf::{self, ImportOutcome},
    theme,
    workers::{self, ImportBook, LayoutWorker},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Drawer {
    Contents,
    Bookmarks,
    Search,
    Settings,
    Models,
    Assistant,
    Annotation,
    Marks,
    ShelfFilters,
    ManageBook,
    ModelLibrary,
    Prompt,
    Memory,
    Translation,
    WebSearch,
    Network,
    Manga,
    AutomaticManga,
}

#[derive(Clone, Copy)]
enum Field {
    Contents,
    Search,
    Note,
    ShelfSearch,
    Rename,
}

struct MarkEditor {
    original: Option<Annotation>,
    ranges: Vec<(Loc, u32)>,
    note: String,
    color: AnnotationColor,
}

type ParsedBook = (PathBuf, Result<ImportBook, String>);

pub(crate) struct Library {
    android: AndroidApp,
    store: Store,
    reader: Reader,
    comic: Comic,
    covers: ShelfCovers,
    preferences: SavedPreferences,
    night: bool,
    ready: bool,
    books: Vec<BookSummary>,
    more: bool,
    listing: bool,
    shelf_filter: ShelfFilter,
    managed_book: Option<BookSummary>,
    opening: Option<Opening>,
    picking: bool,
    parsing: Option<Receiver<ParsedBook>>,
    importing: Option<PathBuf>,
    generated_comic: Option<tempfile::TempPath>,
    notice: Option<String>,
    drawer: Option<Drawer>,
    drawn_drawer: Option<Drawer>,
    drawer_parent: Option<Drawer>,
    controls: bool,
    handles_back: bool,
    dark_bar_icons: Option<bool>,
    insets: Option<[u16; 4]>,
    text_field: Option<Field>,
    models: Models,
    automatic_manga: AutomaticManga,
    peers: PeerSync,
    bookmarks: Vec<Bookmark>,
    more_bookmarks: bool,
    filter: String,
    focus_chapter: bool,
    search: String,
    hits: Vec<SearchHit>,
    mark: Option<MarkEditor>,
    comic_progress: Option<(String, Loc)>,
}

impl Library {
    pub(crate) fn new(
        ctx: egui::Context,
        android: AndroidApp,
        data_dir: &Path,
    ) -> std::io::Result<Self> {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "velmira-cjk".into(),
            FontData::from_static(workers::CJK_FONT).into(),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "velmira-cjk".into());
        ctx.set_fonts(fonts);
        let (preferences, prefs) = SavedPreferences::load(data_dir);
        theme::apply(&ctx, prefs.night);
        let wake = ctx.clone();
        let store = Store::spawn(data_dir.join("velmira.db"), move || wake.request_repaint())
            .map_err(std::io::Error::other)?;
        let mut reader = Reader::new(LayoutWorker::spawn(ctx.clone())?);
        reader.apply_preferences(prefs.font_size, prefs.paper.as_deref());
        Ok(Self {
            android,
            store,
            reader,
            comic: Comic::spawn(ctx.clone())?,
            covers: ShelfCovers::spawn(ctx)?,
            preferences,
            night: prefs.night,
            ready: false,
            books: Vec::new(),
            more: false,
            listing: false,
            shelf_filter: ShelfFilter::default(),
            managed_book: None,
            opening: None,
            picking: false,
            parsing: None,
            importing: None,
            generated_comic: None,
            notice: None,
            drawer: None,
            drawn_drawer: None,
            drawer_parent: None,
            controls: false,
            handles_back: false,
            dark_bar_icons: None,
            insets: None,
            text_field: None,
            models: Models::new(data_dir),
            automatic_manga: AutomaticManga::new(data_dir),
            peers: PeerSync::new(data_dir),
            bookmarks: Vec::new(),
            more_bookmarks: false,
            filter: String::new(),
            focus_chapter: false,
            search: String::new(),
            hits: Vec::new(),
            mark: None,
            comic_progress: None,
        })
    }

    fn send(&mut self, command: Command) -> bool {
        match self.store.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(error.to_string());
                false
            }
        }
    }

    fn list_shelf(&mut self, offset: u32) {
        if offset == 0 || !self.listing {
            self.listing = self.send(Command::ListShelf {
                offset,
                filter: self.shelf_filter.clone(),
            });
        }
    }

    fn reading(&self) -> bool {
        self.reader.is_open() || self.comic.book_id().is_some()
    }

    fn begin_import(&mut self) {
        match saf::begin(&self.android) {
            Ok(()) => {
                self.picking = true;
                self.notice = None;
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn parse_import(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (send, receive) = bounded(1);
        let copy = path.clone();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("velmira-import".into())
            .spawn(move || {
                let result = workers::import_book(copy.clone());
                if send.send((copy.clone(), result)).is_err() {
                    saf::remove_import(&copy);
                }
                ctx.request_repaint();
            }) {
            Ok(_) => self.parsing = Some(receive),
            Err(error) => {
                saf::remove_import(&path);
                self.notice = Some(error.to_string());
            }
        }
    }

    fn poll_import(&mut self, ctx: &egui::Context) {
        if self.picking {
            match saf::take(&self.android) {
                Ok(None) => ctx.request_repaint_after(Duration::from_millis(80)),
                Ok(Some(outcome)) => {
                    self.picking = false;
                    match outcome {
                        ImportOutcome::File(path) => self.parse_import(ctx, path),
                        ImportOutcome::Cancelled => {}
                        ImportOutcome::Error(error) => self.notice = Some(error),
                    }
                }
                Err(error) => {
                    self.picking = false;
                    self.notice = Some(error);
                }
            }
        }
        if let Some(receiver) = &self.parsing {
            match receiver.try_recv() {
                Ok((path, result)) => {
                    self.parsing = None;
                    let sent = match result {
                        Ok(ImportBook::Text(book)) => self.send(Command::InsertBook { book }),
                        Ok(ImportBook::Epub(path)) => self.send(Command::ImportEpub { path }),
                        Ok(ImportBook::Comic(path)) => self.send(Command::ImportCbz { path }),
                        Err(error) => {
                            self.notice = Some(error);
                            false
                        }
                    };
                    if sent {
                        self.importing = Some(path);
                    } else {
                        saf::remove_import(&path);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.parsing = None;
                    self.notice = Some("导入任务已停止。".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn open(&mut self, book_id: String, origin: Option<egui::Rect>) {
        if self.opening.is_none()
            && self.send(Command::OpenBook {
                book_id: book_id.clone(),
            })
        {
            self.opening = Some(Opening::new(book_id, origin));
            self.controls = false;
            self.notice = None;
        }
    }

    fn chapter_tools(&mut self, book_id: String, chapter: u32) {
        self.send(Command::ListAnnotations {
            book_id: book_id.clone(),
            chapter,
        });
        if self.reader.is_epub() && !self.send(Command::ListIllustrations { book_id, chapter }) {
            self.reader.inline_illustrations_failed();
        }
    }

    fn jump(&mut self, loc: Loc) {
        if let Some((book_id, chapter)) = self.reader.prepare_jump(loc) {
            if !self.send(Command::ReadChapter { book_id, chapter }) {
                self.reader.reset_load();
            }
        }
    }

    fn event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Ready => {
                self.ready = true;
                self.list_shelf(0);
            }
            Event::Shelf {
                offset,
                filter,
                books,
                has_more,
            } if filter == self.shelf_filter => {
                self.listing = false;
                if offset == 0 {
                    self.books = books;
                } else if offset as usize == self.books.len() {
                    self.books.extend(books);
                }
                self.more = has_more;
            }
            Event::BookUpdated { .. } => self.list_shelf(0),
            Event::CoverRead {
                book_id,
                size: CoverSize::Shelf,
                bytes,
            } => self.covers.loaded(book_id, bytes),
            Event::BookInserted { book, .. } => {
                if let Some(path) = self.importing.take() {
                    if self.generated_comic.is_none() {
                        saf::remove_import(&path);
                    }
                }
                self.generated_comic = None;
                self.list_shelf(0);
                if self.drawer == Some(Drawer::AutomaticManga) {
                    self.dismiss_drawer();
                }
                self.open(book.id, None);
            }
            Event::BookOpened {
                book,
                toc,
                chapter_index,
                chapter,
            } if self
                .opening
                .as_ref()
                .is_some_and(|opening| opening.book_id == book.id) =>
            {
                if let Some(opening) = &mut self.opening {
                    opening.loaded = true;
                }
                let id = book.id.clone();
                self.comic.close();
                self.reader.open(book, toc, chapter_index, chapter);
                self.bookmarks.clear();
                self.more_bookmarks = false;
                self.filter.clear();
                self.search.clear();
                self.hits.clear();
                self.chapter_tools(id, chapter_index);
            }
            Event::ComicOpened { book, page }
                if self
                    .opening
                    .as_ref()
                    .is_some_and(|opening| opening.book_id == book.id) =>
            {
                if let Some(opening) = &mut self.opening {
                    opening.loaded = true;
                }
                self.reader.close();
                self.comic.open(book, page);
            }
            Event::ChapterRead {
                book_id,
                chapter_index,
                chapter,
            } => {
                if self.reader.chapter_read(&book_id, chapter_index, chapter) {
                    self.chapter_tools(book_id, chapter_index);
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
            }
            Event::IllustrationRead {
                book_id,
                chapter,
                ordinal,
                image,
            } => {
                self.reader
                    .receive_inline_image(ctx, &book_id, chapter, ordinal, &image);
            }
            Event::Annotations {
                book_id,
                chapter,
                annotations,
            } if self.reader.book_id() == Some(&book_id)
                && self.reader.chapter_index() == chapter =>
            {
                self.reader.annotations = annotations;
            }
            Event::AnnotationSaved {
                book_id,
                annotation,
            } => {
                self.send(Command::ListAnnotations {
                    book_id,
                    chapter: annotation.loc.chapter,
                });
            }
            Event::AnnotationsAdded { book_id, chapter }
            | Event::AnnotationDeleted {
                book_id, chapter, ..
            } => {
                self.send(Command::ListAnnotations { book_id, chapter });
            }
            Event::Bookmarks {
                book_id,
                offset,
                bookmarks,
                has_more,
            } if self.reader.book_id() == Some(&book_id) => {
                if offset == 0 {
                    self.bookmarks = bookmarks;
                } else if offset as usize == self.bookmarks.len() {
                    self.bookmarks.extend(bookmarks);
                }
                self.more_bookmarks = has_more;
            }
            Event::BookmarkAdded { book_id, .. } | Event::BookmarkDeleted { book_id, .. } => {
                self.send(Command::ListBookmarks { book_id, offset: 0 });
            }
            Event::SearchResults {
                book_id,
                query,
                hits,
            } if self.reader.book_id() == Some(&book_id) && self.search.trim() == query => {
                self.hits = hits;
            }
            Event::ProgressSaved { book_id, loc } => {
                if let Some(book) = self.books.iter_mut().find(|book| book.id == book_id) {
                    book.progress = Some(loc);
                }
            }
            Event::Error { operation, message } => {
                match operation {
                    "open_book" => self.close_reader(),
                    "insert_book" => {
                        if let Some(path) = self.importing.take() {
                            if self.generated_comic.is_none() {
                                saf::remove_import(&path);
                            }
                        }
                        self.generated_comic = None;
                    }
                    "read_chapter" => self.reader.reset_load(),
                    "read_comic_page" => self.comic.reset_load(),
                    "read_cover" => self.covers.read_failed(),
                    "list_books" => self.listing = false,
                    "list_illustrations" => self.reader.inline_illustrations_failed(),
                    "read_illustration" => self.reader.inline_image_failed(),
                    _ => {}
                }
                self.notice = Some(message);
            }
            _ => {}
        }
    }

    fn close_reader(&mut self) {
        self.opening = None;
        self.reader.close();
        self.models.book_context(None);
        self.comic.close();
        self.drawer = None;
        self.drawer_parent = None;
        self.drawn_drawer = None;
        self.controls = false;
        self.mark = None;
        self.notice = None;
    }

    fn shelf(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        let importing = self.picking || self.parsing.is_some() || self.importing.is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new("书架").size(29.0).strong().color(p.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([54.0, 48.0], egui::Button::new("设置").frame(false))
                    .clicked()
                {
                    self.set_drawer(Drawer::Settings);
                }
                if ui
                    .add_enabled(
                        self.ready && !importing,
                        egui::Button::new(RichText::new("＋ 导入").color(p.on_accent))
                            .fill(p.accent)
                            .min_size(egui::vec2(80.0, 48.0)),
                    )
                    .clicked()
                {
                    self.begin_import();
                }
            });
        });
        if let Some(notice) = &self.notice {
            ui.label(RichText::new(notice).color(p.danger));
        }
        ui.add_space(12.0);
        if importing {
            ui.spinner();
            ui.label(RichText::new("正在导入书籍…").color(p.success));
        }
        ui.horizontal_wrapped(|ui| {
            let search = if self.shelf_filter.query.is_empty() {
                "搜索书名"
            } else {
                self.shelf_filter.query.as_str()
            };
            if ui
                .add_sized([130.0, 44.0], egui::Button::new(search).truncate())
                .clicked()
            {
                self.edit_field(Field::ShelfSearch);
            }
            if ui
                .add_sized([100.0, 44.0], egui::Button::new("筛选与排序").frame(false))
                .clicked()
            {
                self.set_drawer(Drawer::ShelfFilters);
            }
            if !self.shelf_filter.query.is_empty() && ui.button("清空搜索").clicked() {
                self.shelf_filter.query.clear();
                self.list_shelf(0);
            }
        });
        if self.books.is_empty() {
            ui.label(
                RichText::new(if self.listing || !self.ready {
                    "正在打开书库…"
                } else if self.shelf_filter.status == ShelfStatus::Removed {
                    "没有移出的书籍"
                } else if self.shelf_filter != ShelfFilter::default() {
                    "没有匹配的书籍"
                } else {
                    "导入 TXT、EPUB 或 CBZ，在本机离线阅读。"
                })
                .color(p.muted),
            );
        }

        let width = ui.available_width();
        let columns = ((width + 14.0) / 126.0).floor().max(1.0) as usize;
        let card = ((width - 14.0 * (columns.saturating_sub(1) as f32)) / columns as f32)
            .clamp(88.0, 160.0);
        let cover = egui::vec2(card, card * 1.37);
        let mut open = None;
        let mut manage = None;
        egui::ScrollArea::vertical().show_rows(
            ui,
            cover.y + 73.0,
            self.books.len().div_ceil(columns),
            |ui, rows| {
                for row in rows {
                    ui.horizontal_top(|ui| {
                        for book in self.books.iter().skip(row * columns).take(columns) {
                            ui.allocate_ui_with_layout(
                                egui::vec2(card, cover.y + 73.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    let cover = self.covers.show_at(ui, &self.store, book, cover);
                                    let title_clicked = ui
                                        .add_sized(
                                            [card, 30.0],
                                            egui::Button::new(&book.title).truncate().frame(false),
                                        )
                                        .clicked();
                                    if cover.secondary_clicked()
                                        || (self.shelf_filter.status == ShelfStatus::Removed
                                            && (cover.clicked() || title_clicked))
                                    {
                                        manage = Some(book.clone());
                                    } else if cover.clicked() || title_clicked {
                                        open = Some((
                                            book.id.clone(),
                                            cover.rect.intersect(ui.clip_rect()),
                                        ));
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(
                                                book.format.as_str().to_ascii_uppercase(),
                                            )
                                            .size(11.0)
                                            .color(p.muted),
                                        );
                                        if ui
                                            .add_sized(
                                                [43.0, 38.0],
                                                egui::Button::new("管理").frame(false),
                                            )
                                            .clicked()
                                        {
                                            manage = Some(book.clone());
                                        }
                                    });
                                },
                            );
                        }
                    });
                }
            },
        );
        if let Some(book) = manage {
            self.managed_book = Some(book);
            self.set_drawer(Drawer::ManageBook);
        } else if let Some((id, rect)) = open {
            self.open(id, Some(rect));
        }
    }

    fn shelf_filters(&mut self, ui: &mut egui::Ui) {
        let old = self.shelf_filter.clone();
        ui.label(RichText::new("书架").strong());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.shelf_filter.status, ShelfStatus::Active, "在架");
            ui.selectable_value(
                &mut self.shelf_filter.status,
                ShelfStatus::Removed,
                "已移出",
            );
        });
        ui.add_space(8.0);
        ui.label(RichText::new("格式").strong());
        ui.horizontal_wrapped(|ui| {
            for (value, label) in [
                (None, "全部"),
                (Some(BookFormat::Txt), "TXT"),
                (Some(BookFormat::Epub), "EPUB"),
                (Some(BookFormat::Cbz), "CBZ"),
            ] {
                ui.selectable_value(&mut self.shelf_filter.format, value, label);
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("阅读进度").strong());
        ui.horizontal_wrapped(|ui| {
            for (value, label) in [
                (ReadingStatus::All, "全部"),
                (ReadingStatus::Unread, "未读"),
                (ReadingStatus::Reading, "在读"),
                (ReadingStatus::LastChapter, "已到末章"),
            ] {
                ui.selectable_value(&mut self.shelf_filter.reading, value, label);
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("排序").strong());
        ui.horizontal_wrapped(|ui| {
            for (value, label) in [
                (ShelfSort::Newest, "最近导入"),
                (ShelfSort::LastRead, "最近阅读"),
                (ShelfSort::Title, "书名"),
            ] {
                ui.selectable_value(&mut self.shelf_filter.sort, value, label);
            }
        });
        if old != self.shelf_filter {
            self.books.clear();
            self.more = false;
            self.list_shelf(0);
        }
    }

    fn manage_book(&mut self, ui: &mut egui::Ui) {
        let Some(book) = self.managed_book.as_ref() else {
            return;
        };
        let p = theme::palette(self.night);
        ui.label(RichText::new(&book.title).strong().size(17.0));
        let rename = mobile_ui::list_row(ui, "rename", "修改书名", p.ink, p.surface2).clicked();
        let removed = self.shelf_filter.status == ShelfStatus::Removed;
        let action = if removed {
            "恢复到书架"
        } else {
            "移出书架"
        };
        if mobile_ui::list_row(ui, "move", action, p.accent, p.accent_weak).clicked()
            && self.send(Command::SetBookStatus {
                book_id: book.id.clone(),
                status: if removed {
                    ShelfStatus::Active
                } else {
                    ShelfStatus::Removed
                },
            })
        {
            self.dismiss_drawer();
        } else if rename {
            self.edit_field(Field::Rename);
        }
    }

    fn set_drawer(&mut self, tab: Drawer) {
        self.models.book_context(self.reader.book_id());
        if tab == Drawer::Contents && self.drawer != Some(tab) {
            self.focus_chapter = true;
        }
        if tab == Drawer::Bookmarks && self.drawer != Some(Drawer::Bookmarks) {
            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                self.send(Command::ListBookmarks { book_id, offset: 0 });
            }
        }
        if tab == Drawer::Assistant {
            self.models.prepare(
                self.reader
                    .selected_text()
                    .or_else(|| self.reader.current_paragraph()),
            );
        }
        self.drawer = Some(tab);
        self.drawer_parent = None;
    }

    fn dismiss_drawer(&mut self) {
        if self.drawer == Some(Drawer::Annotation) {
            self.mark = None;
        }
        self.drawer = self.drawer_parent.take();
    }

    fn edit_field(&mut self, field: Field) {
        let (title, value, mode, limit) = match field {
            Field::Contents => ("筛选目录", self.filter.as_str(), 0, 100),
            Field::Search => ("搜索正文", self.search.as_str(), 0, 120),
            Field::ShelfSearch => ("搜索书名", self.shelf_filter.query.as_str(), 0, 120),
            Field::Rename => (
                "修改书名",
                self.managed_book
                    .as_ref()
                    .map_or("", |book| book.title.as_str()),
                0,
                240,
            ),
            Field::Note => (
                "添加笔记",
                self.mark.as_ref().map_or("", |mark| mark.note.as_str()),
                3,
                3000,
            ),
        };
        match saf::edit_field(&self.android, title, value, mode, limit) {
            Ok(()) => self.text_field = Some(field),
            Err(error) => self.notice = Some(error),
        }
    }

    fn poll_field(&mut self, ctx: &egui::Context) {
        let Some(field) = self.text_field else {
            return;
        };
        match saf::take_field(&self.android) {
            Ok(None) => ctx.request_repaint_after(Duration::from_millis(80)),
            Ok(Some(value)) => {
                self.text_field = None;
                if let Some(value) = value {
                    match field {
                        Field::Contents => {
                            self.filter = value;
                            self.focus_chapter = true;
                        }
                        Field::Search => {
                            self.search = value;
                            self.search_book();
                        }
                        Field::ShelfSearch => {
                            self.shelf_filter.query = value.trim().to_owned();
                            self.list_shelf(0);
                        }
                        Field::Rename => {
                            let title = value.trim();
                            if let Some(book) =
                                self.managed_book.as_ref().filter(|_| !title.is_empty())
                            {
                                if self.send(Command::RenameBook {
                                    book_id: book.id.clone(),
                                    title: title.to_owned(),
                                }) {
                                    self.dismiss_drawer();
                                }
                            }
                        }
                        Field::Note => {
                            if let Some(mark) = &mut self.mark {
                                mark.note = value;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                self.text_field = None;
                self.notice = Some(error);
            }
        }
    }

    fn search_book(&mut self) {
        let query = self.search.trim().to_owned();
        self.hits.clear();
        if !query.is_empty() {
            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                let chapter = (query.chars().count() == 1).then(|| self.reader.chapter_index());
                self.send(Command::Search {
                    book_id,
                    query,
                    chapter,
                    limit: 50,
                });
            }
        }
    }

    fn drawer(&mut self, ctx: &egui::Context, safe: egui::Rect) {
        let active = self.drawer.is_some();
        let progress = ctx.animate_bool_with_time_and_easing(
            egui::Id::new("reader-sheet-motion"),
            active,
            0.23,
            egui::emath::easing::cubic_out,
        );
        if active || progress == 0.0 {
            self.drawn_drawer = self.drawer;
        }
        let Some(tab) = self.drawn_drawer else {
            return;
        };
        let p = theme::palette(self.night);
        let close = mobile_ui::sheet(ctx, safe, progress, active, p.surface, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let (handle, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
            ui.painter().rect_filled(
                egui::Rect::from_center_size(handle.center(), egui::vec2(32.0, 3.0)),
                2,
                p.line,
            );
            let tabs = self.reader.is_open()
                && matches!(
                    tab,
                    Drawer::Contents | Drawer::Bookmarks | Drawer::Search | Drawer::Settings
                );
            let mut close = false;
            ui.horizontal(|ui| {
                if tabs {
                    let width =
                        ((ui.available_width() - 4.0 * ui.spacing().item_spacing.x) / 5.0).max(1.0);
                    for (candidate, title) in [
                        (Drawer::Contents, "目录"),
                        (Drawer::Bookmarks, "书签"),
                        (Drawer::Search, "搜索"),
                        (Drawer::Settings, "设置"),
                    ] {
                        let active = candidate == tab;
                        let text = RichText::new(title).size(14.0).color(if active {
                            p.accent
                        } else {
                            p.ink
                        });
                        let button = if active {
                            egui::Button::new(text.strong())
                                .fill(p.accent_weak)
                                .stroke(egui::Stroke::NONE)
                        } else {
                            egui::Button::new(text).frame(false)
                        };
                        if ui.add_sized([width, 46.0], button).clicked() {
                            self.set_drawer(candidate);
                        }
                    }
                    close = ui
                        .add_sized(
                            [width, 46.0],
                            egui::Button::new(RichText::new("完成").size(14.0).color(p.accent))
                                .frame(false),
                        )
                        .clicked();
                } else {
                    let title = match tab {
                        Drawer::Models => "模型池",
                        Drawer::Assistant => "阅读 AI",
                        Drawer::Annotation => "批注",
                        Drawer::Marks => "本章批注与高亮",
                        Drawer::ShelfFilters => "筛选与排序",
                        Drawer::ManageBook => "书籍管理",
                        Drawer::ModelLibrary => "模型库",
                        Drawer::Prompt => "提示词",
                        Drawer::Memory => "长期记忆",
                        Drawer::Translation => "翻译服务",
                        Drawer::WebSearch => "联网搜索",
                        Drawer::Network => "局域网同步",
                        Drawer::Manga => "漫画与画风",
                        Drawer::AutomaticManga => "自动漫画",
                        _ => "设置",
                    };
                    ui.label(RichText::new(title).size(21.0).strong().color(p.ink));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.drawer_parent.is_some() {
                            "返回"
                        } else {
                            "完成"
                        };
                        close = ui
                            .add_sized(
                                [64.0, 46.0],
                                egui::Button::new(RichText::new(label).color(p.accent))
                                    .frame(false),
                            )
                            .clicked();
                    });
                }
            });
            ui.separator();
            if tab == Drawer::Contents {
                self.contents(ui);
            } else if tab == Drawer::Marks {
                self.marks(ui);
            } else {
                egui::ScrollArea::vertical()
                    .id_salt(("drawer-body", tab))
                    .max_height(ui.available_height().max(0.0))
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.drawer_content(ui, tab));
            }
            close
        });
        if close {
            self.dismiss_drawer();
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        let label = if self.filter.trim().is_empty() {
            "筛选目录"
        } else {
            self.filter.as_str()
        };
        if mobile_ui::list_row(ui, "filter", label, p.muted, p.surface2).clicked() {
            self.edit_field(Field::Contents);
        }
        let query = self.filter.trim().to_lowercase();
        let toc = self.reader.toc();
        let current = self.reader.chapter_index() as usize;
        let filtered = (!query.is_empty()).then(|| {
            toc.iter()
                .enumerate()
                .filter_map(|(index, title)| title.to_lowercase().contains(&query).then_some(index))
                .collect::<Vec<_>>()
        });
        let count = filtered.as_ref().map_or(toc.len(), Vec::len);
        let mut jump = None;
        let mut list = egui::ScrollArea::vertical()
            .id_salt("chapter-list")
            .max_height(ui.available_height().max(0.0))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false]);
        if ui.is_enabled() && std::mem::take(&mut self.focus_chapter) {
            let row = filtered.as_ref().map_or(current, |indices| {
                indices.partition_point(|index| *index < current)
            });
            list = list.vertical_scroll_offset(
                row.saturating_sub(2) as f32 * (48.0 + ui.spacing().item_spacing.y),
            );
        }
        if count == 0 {
            ui.label(RichText::new("没有匹配章节").color(p.muted));
        }
        list.show_rows(ui, 48.0, count, |ui, rows| {
            for row in rows {
                let index = filtered.as_ref().map_or(row, |indices| indices[row]);
                let selected = index == current;
                if mobile_ui::list_row(
                    ui,
                    ("chapter", index),
                    &toc[index],
                    if selected { p.accent } else { p.ink },
                    if selected {
                        p.accent_weak
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                )
                .clicked()
                {
                    jump = Some(Loc {
                        chapter: index as u32,
                        ..Loc::default()
                    });
                }
            }
        });
        if let Some(loc) = jump {
            self.jump(loc);
            self.dismiss_drawer();
            self.controls = false;
        }
    }

    fn drawer_content(&mut self, ui: &mut egui::Ui, tab: Drawer) {
        let p = theme::palette(self.night);
        match tab {
            Drawer::Contents => self.contents(ui),
            Drawer::Bookmarks => {
                if mobile_ui::list_row(ui, "marks", "本章批注与高亮   ›", p.ink, p.surface2)
                    .clicked()
                {
                    self.drawer_parent = self.drawer;
                    self.drawer = Some(Drawer::Marks);
                }
                if mobile_ui::list_row(
                    ui,
                    "new-bookmark",
                    "＋ 收藏当前位置",
                    p.accent,
                    p.accent_weak,
                )
                .clicked()
                {
                    if let (Some(book_id), Some(loc)) =
                        (self.reader.book_id().map(str::to_owned), self.reader.loc())
                    {
                        self.send(Command::AddBookmark {
                            book_id,
                            loc,
                            label: None,
                        });
                    }
                }
                let mut jump = None;
                if self.bookmarks.is_empty() {
                    ui.label(RichText::new("还没有书签").color(p.muted));
                }
                for (index, bookmark) in self.bookmarks.iter().enumerate() {
                    let title = format!(
                        "第 {} 章 · 第 {} 段",
                        bookmark.loc.chapter + 1,
                        bookmark.loc.para + 1
                    );
                    if mobile_ui::list_row(
                        ui,
                        ("bookmark", index),
                        &title,
                        p.ink,
                        egui::Color32::TRANSPARENT,
                    )
                    .clicked()
                    {
                        jump = Some(bookmark.loc);
                    }
                }
                if self.more_bookmarks
                    && mobile_ui::list_row(
                        ui,
                        "more-bookmarks",
                        "加载更多书签",
                        p.accent,
                        p.surface2,
                    )
                    .clicked()
                {
                    if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                        self.send(Command::ListBookmarks {
                            book_id,
                            offset: self.bookmarks.len() as u32,
                        });
                    }
                }
                if let Some(loc) = jump {
                    self.jump(loc);
                    self.dismiss_drawer();
                    self.controls = false;
                }
            }
            Drawer::Search => {
                let label = if self.search.trim().is_empty() {
                    "输入要搜索的文字"
                } else {
                    self.search.as_str()
                };
                if mobile_ui::list_row(ui, "search", label, p.muted, p.surface2).clicked() {
                    self.edit_field(Field::Search);
                }
                if !self.search.is_empty() && self.hits.is_empty() {
                    ui.label(RichText::new("暂无匹配结果").color(p.muted));
                }
                let mut jump = None;
                for (index, hit) in self.hits.iter().enumerate() {
                    if mobile_ui::list_row(
                        ui,
                        ("search-hit", index),
                        &hit.excerpt,
                        p.ink,
                        egui::Color32::TRANSPARENT,
                    )
                    .clicked()
                    {
                        jump = Some(hit.loc);
                    }
                }
                if let Some(loc) = jump {
                    self.reader.highlight_search(loc, self.search.trim());
                    self.jump(loc);
                    self.dismiss_drawer();
                    self.controls = false;
                }
            }
            Drawer::Settings => self.reader_settings(ui),
            Drawer::Models => self.models.configure(ui, &self.android, false),
            Drawer::ModelLibrary => self.models.configure(ui, &self.android, true),
            Drawer::Translation => self.models.translation_settings(ui),
            Drawer::Prompt => self.models.prompt_settings(ui, &self.android, false),
            Drawer::Memory => self.models.prompt_settings(ui, &self.android, true),
            Drawer::WebSearch => self.models.web_search(ui, &self.android),
            Drawer::Manga => self.models.manga(ui, &self.android),
            Drawer::AutomaticManga => self.automatic_manga.show(ui, &self.models),
            Drawer::Network => self.peers.show(ui, &self.android),
            Drawer::Assistant => {
                if self.models.assistant(ui) {
                    self.drawer_parent = self.drawer;
                    self.drawer = Some(Drawer::Models);
                }
            }
            Drawer::Annotation => self.annotation(ui),
            Drawer::Marks => self.marks(ui),
            Drawer::ShelfFilters => self.shelf_filters(ui),
            Drawer::ManageBook => self.manage_book(ui),
        }
    }

    fn reader_settings(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        let mode = if self.night {
            "界面主题 · 夜间"
        } else {
            "界面主题 · 日间"
        };
        if mobile_ui::list_row(ui, "night", mode, p.ink, p.surface2).clicked() {
            self.night = !self.night;
            theme::apply(ui.ctx(), self.night);
        }
        if self.reader.is_open() {
            let (mut font, mut paper) = self.reader.preferences();
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!("字号  {:.0}", font))
                    .strong()
                    .color(p.ink),
            );
            let mut changed = false;
            ui.horizontal(|ui| {
                if ui.add_sized([48.0, 48.0], egui::Button::new("−")).clicked() {
                    font -= 1.0;
                    changed = true;
                }
                let width = (ui.available_width() - 52.0).max(1.0);
                ui.spacing_mut().slider_width = width;
                changed |= ui
                    .add_sized(
                        [width, 48.0],
                        egui::Slider::new(&mut font, 12.0..=48.0)
                            .step_by(1.0)
                            .show_value(false)
                            .trailing_fill(true),
                    )
                    .changed();
                if ui
                    .add_sized([48.0, 48.0], egui::Button::new("＋"))
                    .clicked()
                {
                    font += 1.0;
                    changed = true;
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new("阅读纸色").strong().color(p.ink));
            let old = paper;
            ui.horizontal(|ui| {
                let width =
                    ((ui.available_width() - 3.0 * ui.spacing().item_spacing.x) / 4.0).max(1.0);
                for (value, label) in [
                    (None, "自动"),
                    (Some("paper"), "暖纸"),
                    (Some("light"), "浅色"),
                    (Some("dark"), "深色"),
                ] {
                    let selected = paper == value;
                    let fill = if selected { p.accent_weak } else { p.surface2 };
                    let tint = if selected { p.accent } else { p.ink };
                    if ui
                        .add_sized(
                            [width, 48.0],
                            egui::Button::new(RichText::new(label).color(tint))
                                .fill(fill)
                                .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        paper = value;
                    }
                }
            });
            if changed || old != paper {
                self.reader.apply_preferences(font, paper);
            }
        }
        ui.add_space(12.0);
        ui.separator();
        for (label, entries) in [
            (
                "AI 引擎",
                &[
                    ("模型库", Drawer::ModelLibrary),
                    ("模型池", Drawer::Models),
                    ("提示词", Drawer::Prompt),
                    ("长期记忆", Drawer::Memory),
                ][..],
            ),
            (
                "扩展功能",
                &[
                    ("自动漫画", Drawer::AutomaticManga),
                    ("漫画与画风", Drawer::Manga),
                    ("翻译服务", Drawer::Translation),
                    ("联网搜索", Drawer::WebSearch),
                ][..],
            ),
            ("系统与网络", &[("局域网同步", Drawer::Network)][..]),
        ] {
            ui.label(RichText::new(label).strong().color(p.ink));
            for &(name, tab) in entries {
                if mobile_ui::list_row(ui, name, name, p.ink, p.surface2).clicked() {
                    if tab == Drawer::AutomaticManga {
                        if let Some(book_id) = self.reader.book_id() {
                            if !self.automatic_manga.open(
                                ui.ctx(),
                                book_id,
                                self.reader.book_title(),
                                self.reader.chapter_index(),
                                self.reader.toc().len() as u32,
                            ) {
                                self.notice =
                                    Some("另一本书正在生成，请等待当前任务结束后再切换。".into());
                                return;
                            }
                        } else {
                            self.notice = Some("请先打开一本小说。".into());
                            return;
                        }
                    }
                    if tab == Drawer::Network {
                        self.peers.refresh(ui.ctx());
                    }
                    if tab == Drawer::Manga {
                        self.models.prepare_manga(
                            self.reader
                                .selected_text()
                                .or_else(|| self.reader.current_paragraph()),
                        );
                    }
                    self.drawer_parent = self.drawer;
                    self.drawer = Some(tab);
                }
            }
            ui.add_space(8.0);
        }
    }

    fn selection_toolbar(&mut self, root: &mut egui::Ui, bounds: egui::Rect) {
        let Some(anchor) = self.reader.selection_anchor() else {
            return;
        };
        let p = theme::palette(self.night);
        let Some(rect) = mobile_ui::selection_menu_rect(bounds, anchor) else {
            return;
        };
        let width = rect.width();
        let area = egui::Area::new(egui::Id::new("android-selection-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .default_size(rect.size())
            .constrain_to(bounds)
            .fade_in(false);
        let result = area
            .show(root.ctx(), |ui| {
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0_f32, p.line))
                    .corner_radius(12)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 12,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(36),
                    })
                    .inner_margin(3)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.horizontal(|ui| {
                            let mut chosen = None;
                            for (index, label) in
                                ["高亮", "批注", "翻译", "复制"].into_iter().enumerate()
                            {
                                if ui
                                    .add_sized(
                                        [(width - 8.0) / 4.0, 42.0],
                                        egui::Button::new(
                                            RichText::new(label).size(14.0).color(p.ink),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(index);
                                }
                            }
                            chosen
                        })
                        .inner
                    })
                    .inner
            })
            .inner;
        match result {
            Some(0) => {
                let ranges = self.reader.selection_ranges();
                if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                    if !ranges.is_empty()
                        && self.send(Command::AddAnnotations {
                            book_id,
                            ranges,
                            note: None,
                            color: AnnotationColor::Yellow,
                        })
                    {
                        self.reader.clear_selection();
                    }
                }
            }
            Some(1) => {
                let ranges = self.reader.selection_ranges();
                if !ranges.is_empty() {
                    self.mark = Some(MarkEditor {
                        original: None,
                        ranges,
                        note: String::new(),
                        color: AnnotationColor::Yellow,
                    });
                    self.set_drawer(Drawer::Annotation);
                }
            }
            Some(2) => {
                if let Some(text) = self.reader.selected_text() {
                    self.set_drawer(Drawer::Assistant);
                    self.models.prepare_translation(text);
                }
            }
            Some(3) => {
                if let Some(text) = self.reader.selected_text() {
                    match saf::copy_text(&self.android, &text) {
                        Ok(()) => {
                            self.reader.clear_selection();
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            _ => {}
        }
    }

    fn edit_annotation(&mut self, mark: Annotation, parent: Option<Drawer>) {
        self.mark = Some(MarkEditor {
            ranges: Vec::new(),
            note: mark.note.clone().unwrap_or_default(),
            color: mark.color,
            original: Some(mark),
        });
        self.set_drawer(Drawer::Annotation);
        self.drawer_parent = parent;
    }

    fn marks(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        if self.reader.annotations.is_empty() {
            ui.label(RichText::new("这一章还没有批注或高亮").color(p.muted));
        }
        let mut edit = None;
        egui::ScrollArea::vertical()
            .id_salt("chapter-marks")
            .max_height(ui.available_height().max(0.0))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .show_rows(ui, 48.0, self.reader.annotations.len(), |ui, rows| {
                for index in rows {
                    let mark = &self.reader.annotations[index];
                    let caption = mark
                        .note
                        .as_deref()
                        .filter(|note| !note.is_empty())
                        .unwrap_or(&mark.text);
                    if mobile_ui::list_row(ui, &mark.id, caption, p.ink, p.surface2).clicked() {
                        edit = Some(mark.clone());
                    }
                }
            });
        if let Some(mark) = edit {
            self.edit_annotation(mark, Some(Drawer::Marks));
        }
    }

    fn annotation(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        let Some(mark) = &mut self.mark else {
            return;
        };
        if let Some(original) = &mark.original {
            ui.label(RichText::new(&original.text).color(p.muted));
        }
        ui.horizontal_wrapped(|ui| {
            for (value, label) in [
                (AnnotationColor::Yellow, "黄色"),
                (AnnotationColor::Green, "绿色"),
                (AnnotationColor::Blue, "蓝色"),
                (AnnotationColor::Pink, "粉色"),
            ] {
                ui.selectable_value(&mut mark.color, value, label);
            }
        });
        let edit = mobile_ui::list_row(
            ui,
            "note-text",
            if mark.note.is_empty() {
                "添加笔记"
            } else {
                &mark.note
            },
            p.ink,
            p.surface2,
        )
        .clicked();
        let mut save = false;
        let mut delete = false;
        ui.horizontal_wrapped(|ui| {
            save = ui
                .add_sized([90.0, 48.0], egui::Button::new("保存"))
                .clicked();
            if mark.original.is_some() {
                delete = ui
                    .add_sized(
                        [90.0, 48.0],
                        egui::Button::new(RichText::new("删除").color(p.danger)).frame(false),
                    )
                    .clicked();
            }
        });
        let command = if save || delete {
            self.reader.book_id().map(|book_id| {
                let book_id = book_id.to_owned();
                let note = (!mark.note.trim().is_empty()).then(|| mark.note.trim().to_owned());
                match &mark.original {
                    Some(annotation) if delete => Command::DeleteAnnotation {
                        book_id,
                        id: annotation.id.clone(),
                        expected_revision: annotation.revision,
                    },
                    Some(annotation) => Command::UpdateAnnotation {
                        book_id,
                        id: annotation.id.clone(),
                        expected_revision: annotation.revision,
                        note,
                        color: mark.color,
                    },
                    None => Command::AddAnnotations {
                        book_id,
                        ranges: mark.ranges.clone(),
                        note,
                        color: mark.color,
                    },
                }
            })
        } else {
            None
        };
        if let Some(command) = command {
            if self.send(command) {
                self.reader.clear_selection();
                self.dismiss_drawer();
            }
        } else if edit {
            self.edit_field(Field::Note);
        }
    }

    fn reader_controls(&mut self, root: &mut egui::Ui, bounds: egui::Rect, opacity: f32) {
        let p = theme::palette(self.night);
        let mut top = bounds;
        top.max.y = (top.min.y + TOP_BAR_HEIGHT).min(top.max.y);
        let mut bottom = bounds;
        bottom.min.y = (bottom.max.y - BOTTOM_BAR_HEIGHT).max(bottom.min.y);
        let mut back = false;
        for (is_top, rect) in [(true, top), (false, bottom)] {
            let mut ui = root.new_child(
                egui::UiBuilder::new()
                    .id_salt(("reader-actions", is_top))
                    .max_rect(rect.shrink2(egui::vec2(8.0, 3.0))),
            );
            ui.set_clip_rect(rect);
            ui.set_opacity(opacity);
            if !self.controls || self.drawer.is_some() || self.drawn_drawer.is_some() {
                ui.disable();
            }
            let painter = ui.painter();
            painter.rect_filled(rect, 0, p.surface);
            if is_top {
                painter.line_segment(
                    [rect.left_bottom(), rect.right_bottom()],
                    egui::Stroke::new(1.0_f32, p.line),
                );
                ui.horizontal(|ui| {
                    back = ui
                        .add_sized([70.0, 48.0], egui::Button::new("‹ 书架").frame(false))
                        .clicked();
                    let title = if self.reader.is_open() {
                        self.reader.book_title()
                    } else {
                        self.comic.title()
                    };
                    ui.add(
                        egui::Label::new(RichText::new(title).size(17.0).strong().color(p.ink))
                            .truncate(),
                    );
                });
            } else {
                painter.line_segment(
                    [rect.left_top(), rect.right_top()],
                    egui::Stroke::new(1.0_f32, p.line),
                );
                if self.reader.is_open() {
                    ui.horizontal(|ui| {
                        let width = ((ui.available_width() - 4.0 * ui.spacing().item_spacing.x)
                            / 5.0)
                            .max(1.0);
                        for (tab, label) in [
                            (Drawer::Contents, "目录"),
                            (Drawer::Bookmarks, "书签"),
                            (Drawer::Search, "搜索"),
                            (Drawer::Settings, "设置"),
                            (Drawer::Assistant, "AI"),
                        ] {
                            let mut button =
                                egui::Button::new(RichText::new(label).size(14.0).color(p.ink))
                                    .frame(false);
                            if tab == Drawer::Assistant {
                                button = button.fill(p.accent_weak).stroke(egui::Stroke::NONE);
                            }
                            if ui.add_sized([width, 48.0], button).clicked() {
                                self.set_drawer(tab);
                            }
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized(
                                [84.0, 48.0],
                                egui::Button::new(self.comic.fit_mode_label()).frame(false),
                            )
                            .clicked()
                        {
                            self.comic.toggle_fit_mode();
                        }
                        if ui
                            .add_sized([72.0, 48.0], egui::Button::new("设置").frame(false))
                            .clicked()
                        {
                            self.set_drawer(Drawer::Settings);
                        }
                    });
                }
            }
        }
        if back {
            self.close_reader();
        }
    }

    fn show_scene(&mut self, root: &mut egui::Ui, reading: bool) {
        let p = theme::palette(self.night);
        if !reading {
            if self.more {
                egui::TopBottomPanel::bottom("shelf-more")
                    .frame(egui::Frame::new().fill(p.bg).inner_margin(8))
                    .show_inside(root, |ui| {
                        if ui
                            .add_sized(
                                [ui.available_width(), 44.0],
                                egui::Button::new("加载更多").frame(false),
                            )
                            .clicked()
                        {
                            self.list_shelf(self.books.len() as u32);
                        }
                    });
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(p.bg).inner_margin(16))
                .show_inside(root, |ui| self.shelf(ui));
            return;
        }
        let bounds = root.max_rect();
        let opacity = root.ctx().animate_bool_with_time_and_easing(
            egui::Id::new("android-reader-controls"),
            self.controls,
            0.16,
            egui::emath::easing::cubic_out,
        );
        let paper = if self.reader.is_open() {
            self.reader.paper_color(self.night)
        } else {
            p.bg
        };
        let blocked = !root.is_enabled() || self.drawer.is_some() || self.drawn_drawer.is_some();
        let tap_bounds = mobile_ui::tap_rect(bounds, opacity > 0.0);
        let selection_menu = self
            .reader
            .selection_anchor()
            .and_then(|anchor| mobile_ui::selection_menu_rect(bounds, anchor));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(paper))
            .show_inside(root, |ui| {
                if self.reader.is_open() {
                    if let Some((chapter, last_page)) =
                        self.reader
                            .show(ui, true, false, blocked, tap_bounds, selection_menu)
                    {
                        if let Some((book_id, chapter)) =
                            self.reader.prepare_adjacent(chapter, last_page)
                        {
                            if !self.send(Command::ReadChapter { book_id, chapter }) {
                                self.reader.reset_load();
                            }
                        }
                    }
                    if self.reader.take_tools_request() {
                        self.controls = !self.controls;
                    }
                    if let Some(mark) = self.reader.take_tapped_annotation() {
                        self.edit_annotation(mark, None);
                    }
                } else {
                    if let Some((book_id, page_index)) =
                        self.comic.show(ui, true, blocked, tap_bounds)
                    {
                        if !self.send(Command::ReadComicPage {
                            book_id,
                            page_index,
                        }) {
                            self.comic.reset_load();
                        }
                    }
                    if self.comic.take_tools_request() {
                        self.controls = !self.controls;
                    }
                }
            });
        if opacity > 0.0 {
            self.reader_controls(root, bounds, opacity);
        }
        if !blocked {
            self.selection_toolbar(root, bounds);
        }
        if let Some(text) = &self.notice {
            root.painter().text(
                bounds.center_top() + egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_TOP,
                text,
                egui::FontId::proportional(13.0),
                p.danger,
            );
        }
    }
}

impl eframe::App for Library {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll_import(ctx);
        self.poll_field(ctx);
        self.models.poll(ctx, &self.android);
        self.automatic_manga.poll();
        if self.importing.is_none() {
            if let Some(comic) = self
                .automatic_manga
                .take_export()
                .or_else(|| self.models.take_comic())
            {
                let path = comic.to_path_buf();
                if self.send(Command::ImportCbz { path: path.clone() }) {
                    self.importing = Some(path);
                    self.generated_comic = Some(comic);
                }
            }
        }
        if self.peers.poll(ctx, &self.android) {
            self.list_shelf(0);
            if let Some(id) = self.reader.book_id().map(str::to_owned) {
                self.send(Command::ListBookmarks {
                    book_id: id.clone(),
                    offset: 0,
                });
                self.send(Command::ListAnnotations {
                    book_id: id,
                    chapter: self.reader.chapter_index(),
                });
            }
        }
        while let Ok(event) = self.store.events().try_recv() {
            self.event(ctx, event);
        }
        self.covers.poll();
        if let Some(result) = self.reader.poll(ctx) {
            match result {
                Ok(shown) => {
                    let _ = (shown.first, shown.layout_ms, shown.raster_ms);
                }
                Err(error) => {
                    if self.opening.as_ref().is_some_and(|opening| opening.loaded)
                        && !self.reader.ready()
                    {
                        self.close_reader();
                    }
                    self.notice = Some(error);
                }
            }
        }
        if let Some((book_id, loc)) = self.reader.unsaved() {
            if self.send(Command::SaveProgress { book_id, loc }) {
                self.reader.progress_sent(loc);
            } else {
                ctx.request_repaint_after(Duration::from_millis(35));
            }
        }
        if let Some(result) = self.comic.poll(ctx) {
            match result {
                Ok(progress) => self.comic_progress = Some(progress),
                Err(error) => {
                    if self.opening.as_ref().is_some_and(|opening| opening.loaded)
                        && !self.comic.ready()
                    {
                        self.close_reader();
                    }
                    self.notice = Some(error);
                }
            }
        }
        if let Some((book_id, loc)) = self.comic_progress.take() {
            if !self.send(Command::SaveProgress {
                book_id: book_id.clone(),
                loc,
            }) {
                self.comic_progress = Some((book_id, loc));
                ctx.request_repaint_after(Duration::from_millis(35));
            }
        }
        if self.handles_back && saf::take_reader_back(&self.android).unwrap_or_default() {
            if self.drawer.is_some() {
                self.dismiss_drawer();
            } else if !self.reader.clear_selection() {
                if self.controls {
                    self.controls = false;
                } else {
                    self.close_reader();
                }
            }
        }
        let screen = ctx.screen_rect();
        if let Ok(Some(insets)) = saf::safe_insets(&self.android) {
            self.insets = Some(insets);
        }
        let scale = ctx.pixels_per_point();
        let fallback = (24.0 * scale) as u16;
        let safe = mobile_ui::content_rect(
            screen,
            self.insets.unwrap_or([0, fallback, 0, fallback]),
            scale,
        );
        if self.insets.is_none() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let ready = self.reader.ready() || self.comic.ready();
        let preparing = self.reading() && !ready;
        let reveal = Opening::frame(&mut self.opening, ctx, ready, safe);
        let library_visible = !self.reading() || preparing || reveal.is_some();
        let p = theme::palette(self.night);
        let background = if !library_visible && self.reader.is_open() {
            self.reader.paper_color(self.night)
        } else {
            p.bg
        };
        ctx.layer_painter(egui::LayerId::background())
            .rect_filled(screen, 0, background);
        let dark_icons =
            u16::from(background.r()) + u16::from(background.g()) + u16::from(background.b())
                > 3 * 145;
        if self.dark_bar_icons != Some(dark_icons)
            && saf::set_dark_system_bar_icons(&self.android, dark_icons).is_ok()
        {
            self.dark_bar_icons = Some(dark_icons);
        }
        if safe.is_positive() {
            if library_visible {
                let mut ui =
                    opening::scene(ctx, egui::Id::new("shelf-scene"), reveal.is_none(), safe);
                self.show_scene(&mut ui, false);
            }
            if self.reading() {
                let mut ui = opening::scene(
                    ctx,
                    egui::Id::new("reader-scene"),
                    !preparing && reveal.is_none(),
                    safe,
                );
                if preparing {
                    ui.set_invisible();
                }
                if let Some(reveal) = reveal {
                    reveal.paint(&mut ui, p.bg, |ui| self.show_scene(ui, true));
                } else {
                    self.show_scene(&mut ui, true);
                }
            }
            self.drawer(ctx, safe);
        }
        if let Some(command) = self.reader.inline_image_request() {
            if !self.send(command) {
                self.reader.inline_image_failed();
            }
        }
        let handles_back = self.opening.is_some()
            || self.reading()
            || self.drawer.is_some()
            || self.drawn_drawer.is_some();
        if self.handles_back != handles_back
            && saf::set_handles_back(&self.android, handles_back).is_ok()
        {
            self.handles_back = handles_back;
        }
        if handles_back {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let (font_size, paper) = self.reader.preferences();
        self.preferences.update(
            ctx,
            Preferences {
                night: self.night,
                contents: false,
                compact: false,
                font_size,
                paper: paper.map(str::to_owned),
            },
            ctx.input(|input| !input.focused),
        );
    }
}
