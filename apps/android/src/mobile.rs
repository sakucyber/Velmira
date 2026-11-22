use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use android_activity::AndroidApp;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, RichText};
use vm_core::{BookSummary, Loc};
use vm_store::{AnnotationColor, Bookmark, Command, CoverSize, Event, SearchHit, Store};

use crate::{
    comic::Comic,
    covers::ShelfCovers,
    opening::{self, Opening},
    preferences::{Preferences, SavedPreferences},
    reader::Reader,
    saf::{self, ImportOutcome},
    theme,
    workers::{self, ImportBook, LayoutWorker},
};

enum Drawer {
    Contents,
    Bookmarks,
    Search,
    Settings,
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
    opening: Option<Opening>,
    picking: bool,
    parsing: Option<Receiver<ParsedBook>>,
    importing: Option<PathBuf>,
    notice: Option<String>,
    drawer: Option<Drawer>,
    bookmarks: Vec<Bookmark>,
    more_bookmarks: bool,
    filter: String,
    search: String,
    hits: Vec<SearchHit>,
    mark: Option<(Loc, u32, String, AnnotationColor)>,
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
            opening: None,
            picking: false,
            parsing: None,
            importing: None,
            notice: None,
            drawer: None,
            bookmarks: Vec::new(),
            more_bookmarks: false,
            filter: String::new(),
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
            let _ = saf::set_reading_book(&self.android, true);
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
                self.send(Command::ListBooks { offset: 0 });
            }
            Event::Books {
                offset,
                books,
                has_more,
            } => {
                if offset == 0 {
                    self.books = books;
                } else if offset as usize == self.books.len() {
                    self.books.extend(books);
                }
                self.more = has_more;
            }
            Event::CoverRead {
                book_id,
                size: CoverSize::Shelf,
                bytes,
            } => self.covers.loaded(book_id, bytes),
            Event::BookInserted { book, .. } => {
                if let Some(path) = self.importing.take() {
                    saf::remove_import(&path);
                }
                self.send(Command::ListBooks { offset: 0 });
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
                            saf::remove_import(&path);
                        }
                    }
                    "read_chapter" => self.reader.reset_load(),
                    "read_comic_page" => self.comic.reset_load(),
                    "read_cover" => self.covers.read_failed(),
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
        self.comic.close();
        self.drawer = None;
        self.mark = None;
        self.notice = None;
        let _ = saf::set_reading_book(&self.android, false);
    }

    fn shelf(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(self.night);
        let importing = self.picking || self.parsing.is_some() || self.importing.is_some();
        ui.horizontal_wrapped(|ui| {
            ui.heading("书架");
            if ui
                .add_enabled(
                    self.ready && !importing,
                    egui::Button::new(RichText::new("导入书籍").color(p.on_accent))
                        .fill(p.accent)
                        .min_size(egui::vec2(84.0, 38.0)),
                )
                .clicked()
            {
                self.begin_import();
            }
        });
        ui.add_space(12.0);
        if importing {
            ui.spinner();
            ui.label(RichText::new("正在导入书籍…").color(p.success));
        }
        if self.books.is_empty() {
            ui.label(
                RichText::new(if self.ready {
                    "导入 TXT、EPUB 或 CBZ，在本机离线阅读。"
                } else {
                    "正在打开本机书库…"
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
        egui::ScrollArea::vertical().show_rows(
            ui,
            cover.y + 66.0,
            self.books.len().div_ceil(columns),
            |ui, rows| {
                for row in rows {
                    ui.horizontal_top(|ui| {
                        for book in self.books.iter().skip(row * columns).take(columns) {
                            ui.allocate_ui_with_layout(
                                egui::vec2(card, cover.y + 62.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    let cover = self.covers.show_at(ui, &self.store, book, cover);
                                    let title_clicked = ui
                                        .add_sized(
                                            [card, 30.0],
                                            egui::Button::new(&book.title).truncate().frame(false),
                                        )
                                        .clicked();
                                    if cover.clicked() || title_clicked {
                                        open = Some((
                                            book.id.clone(),
                                            cover.rect.intersect(ui.clip_rect()),
                                        ));
                                    }
                                    ui.label(
                                        RichText::new(book.format.as_str().to_ascii_uppercase())
                                            .size(11.0)
                                            .color(p.muted),
                                    );
                                },
                            );
                        }
                    });
                }
            },
        );
        if let Some((id, rect)) = open {
            self.open(id, Some(rect));
        }
    }

    fn drawer(&mut self, ctx: &egui::Context) {
        let Some(mut tab) = self.drawer.take() else {
            return;
        };
        let mut close = false;
        let mut jump = None;
        let response = egui::Modal::new(egui::Id::new("reader-drawer"))
            .area(
                egui::Modal::default_area(egui::Id::new("reader-drawer"))
                    .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, 52.0)),
            )
            .show(ctx, |ui| {
                let width = (ctx.screen_rect().width() - 34.0).clamp(175.0, 340.0);
                ui.set_width(width);
                ui.horizontal_wrapped(|ui| {
                    if self.reader.is_open() {
                        if ui
                            .selectable_label(matches!(tab, Drawer::Contents), "目录")
                            .clicked()
                        {
                            tab = Drawer::Contents;
                        }
                        if ui
                            .selectable_label(matches!(tab, Drawer::Bookmarks), "书签")
                            .clicked()
                        {
                            tab = Drawer::Bookmarks;
                            if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                                self.send(Command::ListBookmarks { book_id, offset: 0 });
                            }
                        }
                        if ui
                            .selectable_label(matches!(tab, Drawer::Search), "搜索")
                            .clicked()
                        {
                            tab = Drawer::Search;
                        }
                    }
                    if ui
                        .selectable_label(matches!(tab, Drawer::Settings), "设置")
                        .clicked()
                    {
                        tab = Drawer::Settings;
                    }
                    close = ui.button("关闭").clicked();
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 180.0).max(100.0))
                    .show(ui, |ui| match tab {
                        Drawer::Contents => {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.filter)
                                    .hint_text("筛选目录")
                                    .char_limit(100),
                            );
                            let filter = self.filter.trim().to_lowercase();
                            let current = self.reader.chapter_index();
                            for (index, title) in self.reader.toc().iter().enumerate() {
                                if (filter.is_empty() || title.to_lowercase().contains(&filter))
                                    && ui
                                        .add_sized(
                                            [width, 39.0],
                                            egui::Button::selectable(
                                                index as u32 == current,
                                                title,
                                            )
                                            .truncate(),
                                        )
                                        .clicked()
                                {
                                    jump = Some((
                                        Loc {
                                            chapter: index as u32,
                                            ..Loc::default()
                                        },
                                        false,
                                    ));
                                }
                            }
                        }
                        Drawer::Bookmarks => {
                            if let (Some(id), Some(loc)) =
                                (self.reader.book_id(), self.reader.loc())
                            {
                                if ui.button("收藏当前位置").clicked() {
                                    self.send(Command::AddBookmark {
                                        book_id: id.to_owned(),
                                        loc,
                                        label: None,
                                    });
                                }
                            }
                            for bookmark in &self.bookmarks {
                                if ui
                                    .button(format!(
                                        "第 {} 章 · 第 {} 段",
                                        bookmark.loc.chapter + 1,
                                        bookmark.loc.para + 1
                                    ))
                                    .clicked()
                                {
                                    jump = Some((bookmark.loc, false));
                                }
                            }
                            if self.more_bookmarks && ui.button("更多书签").clicked() {
                                if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                                    self.send(Command::ListBookmarks {
                                        book_id,
                                        offset: self.bookmarks.len() as u32,
                                    });
                                }
                            }
                        }
                        Drawer::Search => {
                            let input = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text("搜索正文")
                                    .char_limit(120),
                            );
                            if ui.button("搜索").clicked()
                                || input.lost_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                            {
                                if let Some(book_id) = self.reader.book_id().map(str::to_owned) {
                                    let query = self.search.trim().to_owned();
                                    let chapter = (query.chars().count() == 1)
                                        .then(|| self.reader.chapter_index());
                                    self.hits.clear();
                                    self.send(Command::Search {
                                        book_id,
                                        query,
                                        chapter,
                                        limit: 50,
                                    });
                                }
                            }
                            for hit in &self.hits {
                                if ui.button(&hit.excerpt).clicked() {
                                    jump = Some((hit.loc, true));
                                }
                            }
                        }
                        Drawer::Settings => {
                            if ui.checkbox(&mut self.night, "夜间模式").changed() {
                                theme::apply(ctx, self.night);
                            }
                            let (mut font, paper) = self.reader.preferences();
                            let mut paper = paper;
                            let previous = paper;
                            let change = ui
                                .add(
                                    egui::Slider::new(&mut font, 12.0..=48.0)
                                        .text("字号")
                                        .step_by(0.1),
                                )
                                .changed();
                            ui.horizontal_wrapped(|ui| {
                                ui.selectable_value(&mut paper, None, "随系统界面");
                                ui.selectable_value(&mut paper, Some("paper"), "纸色");
                                ui.selectable_value(&mut paper, Some("light"), "浅色");
                                ui.selectable_value(&mut paper, Some("dark"), "深色");
                            });
                            if change || previous != paper {
                                self.reader.apply_preferences(font, paper);
                            }
                        }
                    });
            });
        if let Some((loc, search)) = jump {
            if search {
                self.reader.highlight_search(loc, self.search.trim());
            }
            self.jump(loc);
        } else if !close && !response.should_close() {
            self.drawer = Some(tab);
        }
    }

    fn edit_mark(&mut self, ctx: &egui::Context) {
        let Some((loc, end_off, mut note, mut color)) = self.mark.take() else {
            return;
        };
        let Some(book_id) = self.reader.book_id().map(str::to_owned) else {
            return;
        };
        let (mut save, mut cancel) = (false, false);
        let response = egui::Modal::new(egui::Id::new("annotation")).show(ctx, |ui| {
            ui.set_width((ctx.screen_rect().width() - 58.0).clamp(170.0, 330.0));
            ui.heading("批注");
            ui.horizontal_wrapped(|ui| {
                for (value, name) in [
                    (AnnotationColor::Yellow, "黄"),
                    (AnnotationColor::Green, "绿"),
                    (AnnotationColor::Blue, "蓝"),
                    (AnnotationColor::Pink, "粉"),
                ] {
                    ui.selectable_value(&mut color, value, name);
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut note)
                    .desired_rows(3)
                    .char_limit(3000)
                    .hint_text("添加笔记"),
            );
            ui.horizontal(|ui| {
                save = ui.button("保存").clicked();
                cancel = ui.button("取消").clicked();
            });
        });
        if save {
            let value = (!note.trim().is_empty()).then(|| note.trim().to_owned());
            if !self.send(Command::AddAnnotation {
                book_id,
                loc,
                end_off,
                note: value,
                color,
            }) {
                self.mark = Some((loc, end_off, note, color));
            }
        } else if !cancel && !response.should_close() {
            self.mark = Some((loc, end_off, note, color));
        }
    }

    fn show_scene(&mut self, root: &mut egui::Ui, reading: bool) {
        let p = theme::palette(self.night);
        let mut back = false;
        egui::TopBottomPanel::top(if reading {
            "reader-toolbar"
        } else {
            "shelf-toolbar"
        })
        .frame(egui::Frame::new().fill(p.surface).inner_margin(9.0))
        .show_inside(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                if reading {
                    if ui.button("‹ 书架").clicked() {
                        back = true;
                    }
                    if self.reader.is_open() && ui.button("目录").clicked() {
                        self.drawer = Some(Drawer::Contents);
                    }
                } else {
                    ui.label(RichText::new("Velmira").size(20.0).strong());
                }
                if ui.button("设置").clicked() {
                    self.drawer = Some(Drawer::Settings);
                }
            });
            if let Some(text) = &self.notice {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(text).color(p.danger));
                });
            }
        });
        if !reading && self.more {
            egui::TopBottomPanel::bottom("shelf-more").show_inside(root, |ui| {
                if ui.button("加载更多").clicked() {
                    self.send(Command::ListBooks {
                        offset: self.books.len() as u32,
                    });
                }
            });
        }
        let dialog = !root.is_enabled() || self.drawer.is_some() || self.mark.is_some();
        egui::CentralPanel::default().show_inside(root, |ui| {
            if !reading {
                self.shelf(ui);
            } else if self.reader.is_open() {
                if let Some((chapter, last_page)) = self.reader.show(ui, true, false, dialog) {
                    if let Some((book_id, chapter)) =
                        self.reader.prepare_adjacent(chapter, last_page)
                    {
                        if !self.send(Command::ReadChapter { book_id, chapter }) {
                            self.reader.reset_load();
                        }
                    }
                }
                if let Some((loc, end)) = self.reader.take_mark() {
                    self.mark = Some((loc, end, String::new(), AnnotationColor::Yellow));
                }
            } else if self.comic.book_id().is_some() {
                if let Some((book_id, page_index)) = self.comic.show(ui, true) {
                    if !self.send(Command::ReadComicPage {
                        book_id,
                        page_index,
                    }) {
                        self.comic.reset_load();
                    }
                }
            }
        });
        if back {
            self.close_reader();
            root.ctx().request_repaint();
        }
    }
}

impl eframe::App for Library {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll_import(ctx);
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
        if self.opening.is_some() || self.reading() {
            if saf::take_reader_back(&self.android).unwrap_or_default() {
                if self.mark.take().is_none() && self.drawer.take().is_none() {
                    self.close_reader();
                }
            }
            ctx.request_repaint_after(Duration::from_millis(90));
        }
        let ready = self.reader.ready() || self.comic.ready();
        let preparing = self.reading() && !ready;
        let reveal = Opening::frame(&mut self.opening, ctx, ready);
        if !self.reading() || preparing || reveal.is_some() {
            let mut ui = opening::scene(ctx, egui::Id::new("shelf-scene"), reveal.is_none());
            self.show_scene(&mut ui, false);
        }
        if self.reading() {
            let mut ui = opening::scene(
                ctx,
                egui::Id::new("reader-scene"),
                !preparing && reveal.is_none(),
            );
            if preparing {
                ui.set_invisible();
            }
            if let Some(reveal) = reveal {
                reveal.paint(&mut ui, theme::palette(self.night).bg, |ui| {
                    self.show_scene(ui, true)
                });
            } else {
                self.show_scene(&mut ui, true);
            }
        }
        if let Some(command) = self.reader.inline_image_request() {
            if !self.send(command) {
                self.reader.inline_image_failed();
            }
        }
        self.drawer(ctx);
        self.edit_mark(ctx);
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
