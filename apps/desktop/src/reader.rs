use crate::workers::{Flip, LayoutCommand, LayoutWorker};
use crossbeam_channel::{bounded, Receiver, TryRecvError, TrySendError};
use eframe::egui::{self, Color32, ColorImage, Key, PointerButton, TextureHandle, TextureOptions};
use std::{sync::Arc, time::Duration};
use vm_core::{BookSummary, Chapter, Loc};
use vm_store::{Annotation, AnnotationColor, Command, Illustration, IllustrationImage};
use vm_text::{IllustrationAnchor, PageBox, PageLayout};

const PAGE_TURN_SECONDS: f64 = 0.20;
const MAX_INLINE_EDGE: u32 = 1200;
const DEFAULT_FONT_SIZE: f32 = 19.0;

fn page_surface_id() -> egui::Id {
    egui::Id::new("reader-page-surface")
}

struct ReadingBook {
    summary: BookSummary,
    toc: Vec<String>,
    chapter: u32,
    title: String,
    paragraphs: Arc<[String]>,
    loc: Loc,
}

#[derive(Clone, Copy)]
struct Request {
    position: Flip,
    // Resize, theme changes and opening a saved position preserve its exact
    // character offset, even if it falls in the middle of a rendered page.
    persist_page_start: bool,
    turn: i8,
}

#[derive(Clone, Copy)]
struct ChapterRequest {
    index: u32,
    request: Request,
}

struct PageTurn {
    previous: TextureHandle,
    scale: f32,
    theme: Theme,
    direction: f32,
    start: f64,
    illustrations: Vec<(TextureHandle, vm_text::Rect)>,
}

#[derive(Default)]
struct InlineImages {
    anchors: Arc<[IllustrationAnchor]>,
    textures: Vec<(u32, Option<TextureHandle>)>,
    waiting: Option<(u32, u32)>,
    awaiting_list: bool,
}

struct InlineDecode {
    receiver: Receiver<Option<vm_media::RgbaImage>>,
    book_id: String,
    chapter: u32,
    ordinal: u32,
}

#[derive(Default)]
struct TouchGesture {
    active: Vec<(egui::TouchDeviceId, egui::TouchId)>,
    start: Option<egui::Pos2>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Navigation {
    Page(i8),
    Chapter(i8),
    Tools,
}

fn tap_navigation(canvas: egui::Rect, x: f32, center_opens_tools: bool) -> Navigation {
    if center_opens_tools && (x - canvas.center().x).abs() <= canvas.width() * 0.20 {
        Navigation::Tools
    } else {
        Navigation::Page(if x < canvas.center().x { -1 } else { 1 })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Paper,
    White,
    Night,
}

impl Theme {
    fn colors(self) -> (Color32, Color32) {
        match self {
            Self::Paper => (
                Color32::from_rgb(248, 241, 223),
                Color32::from_rgb(53, 47, 38),
            ),
            Self::White => (
                Color32::from_rgb(255, 255, 253),
                Color32::from_rgb(34, 35, 33),
            ),
            Self::Night => (
                Color32::from_rgb(36, 42, 45),
                Color32::from_rgb(231, 224, 210),
            ),
        }
    }
}

pub struct Shown {
    pub first: bool,
    pub layout_ms: Option<f64>,
    pub raster_ms: f64,
}

#[derive(Clone, Copy, PartialEq)]
struct PageGeometry {
    page: PageBox,
    scale: f32,
    gutter: Option<u32>,
}

impl PageGeometry {
    fn new(available: egui::Vec2, font: f32, density: f32) -> Self {
        let gutter = (available.x * 0.055).clamp(44.0, 112.0);
        let double = available.x >= 1120.0_f32.max(font * 46.0);
        let column_width = (font * 39.0).clamp(610.0, 950.0);
        let width = if double {
            column_width * 2.0 + gutter
        } else {
            column_width
        };
        let size = egui::vec2(available.x.min(width), available.y).max(egui::Vec2::splat(104.0));
        // Keep native raster buffers within the paginator's pixel budget on
        // high-DPI / ultrawide displays without reducing the logical viewport.
        let scale = density
            .clamp(1.0, 2.0)
            .min(4096.0 / size.x.max(size.y))
            .min((4_194_304.0 / (size.x * size.y)).sqrt());
        Self {
            page: PageBox {
                width: (size.x * scale).floor(),
                height: (size.y * scale).floor(),
                font_size: font * scale,
                line_height: font * 1.55 * scale,
            },
            scale,
            gutter: double.then_some((gutter * scale).round() as u32),
        }
    }
}

pub struct Reader {
    worker: LayoutWorker,
    book: Option<ReadingBook>,
    chapter_request: Option<ChapterRequest>,
    pending: Option<Request>,
    running: Option<(u64, Request, Theme, f32)>,
    next_id: u64,
    screen: Option<PageGeometry>,
    render_scale: f32,
    layout_dirty: bool,
    layout_worker_failed: bool,
    texture: Option<(TextureHandle, Theme)>,
    turn: Option<PageTurn>,
    page: usize,
    page_count: usize,
    theme: Option<Theme>,
    font_px: f32,
    missing_glyphs: usize,
    unsaved: Option<Loc>,
    first_page: bool,
    layout: Option<Arc<PageLayout>>,
    pub annotations: Vec<Annotation>,
    selection: Option<(Loc, Loc)>,
    mark: Option<(Loc, u32)>,
    search_highlight: Option<(Loc, String)>,
    inline: InlineImages,
    inline_decode: Option<InlineDecode>,
    touch: TouchGesture,
    tools_requested: bool,
}

impl Reader {
    #[cfg(not(target_os = "android"))]
    pub fn comic_source(&self) -> Option<String> {
        let book = self.book.as_ref()?;
        let mut segments = vm_core::segments::segment_chapter(book.chapter, &book.paragraphs);
        vm_core::segments::rank_segments(&mut segments, 1);
        let paragraphs = segments
            .iter()
            .find(|segment| segment.selected)
            .and_then(|segment| book.paragraphs.get(segment.start_para..=segment.end_para))
            .unwrap_or(&book.paragraphs);
        let excerpt: String = paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.chars().chain(['\n']))
            .take(12_001)
            .collect();
        Some(excerpt)
    }

    pub fn new(worker: LayoutWorker) -> Self {
        Self {
            worker,
            book: None,
            chapter_request: None,
            pending: None,
            running: None,
            next_id: 0,
            screen: None,
            render_scale: 1.0,
            layout_dirty: true,
            layout_worker_failed: false,
            texture: None,
            turn: None,
            page: 0,
            page_count: 0,
            theme: None,
            font_px: DEFAULT_FONT_SIZE,
            missing_glyphs: 0,
            unsaved: None,
            first_page: true,
            layout: None,
            annotations: Vec::new(),
            selection: None,
            mark: None,
            search_highlight: None,
            inline: InlineImages::default(),
            inline_decode: None,
            touch: TouchGesture::default(),
            tools_requested: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.book.is_some()
    }

    pub fn is_epub(&self) -> bool {
        self.book
            .as_ref()
            .is_some_and(|book| book.summary.format == vm_core::BookFormat::Epub)
    }
    pub fn book_id(&self) -> Option<&str> {
        self.book.as_ref().map(|book| book.summary.id.as_str())
    }
    pub fn chapter_index(&self) -> u32 {
        self.book.as_ref().map_or(0, |book| book.chapter)
    }
    pub fn toc(&self) -> &[String] {
        self.book.as_ref().map_or(&[], |book| book.toc.as_slice())
    }
    pub fn loc(&self) -> Option<Loc> {
        self.book.as_ref().map(|book| book.loc)
    }
    #[cfg(target_os = "android")]
    pub fn take_tools_request(&mut self) -> bool {
        std::mem::take(&mut self.tools_requested)
    }

    #[cfg(target_os = "android")]
    pub fn book_title(&self) -> &str {
        self.book
            .as_ref()
            .map_or("", |book| book.summary.title.as_str())
    }

    #[cfg(target_os = "android")]
    pub fn paper_color(&self, night: bool) -> Color32 {
        self.theme
            .unwrap_or(if night { Theme::Night } else { Theme::Paper })
            .colors()
            .0
    }

    pub fn take_mark(&mut self) -> Option<(Loc, u32)> {
        self.mark.take()
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (start, end) = (a.min(b), a.max(b));
        let book = self.book.as_ref()?;
        if start == end || start.chapter != book.chapter || end.chapter != book.chapter {
            return None;
        }
        let mut selected = String::new();
        for para in start.para..=end.para {
            let chunk = book.paragraphs.get(para as usize)?;
            let begin = if para == start.para { start.off } else { 0 };
            let chars = if para == end.para {
                end.off - begin
            } else {
                u32::MAX
            };
            if para > start.para {
                selected.push('\n');
            }
            selected.extend(chunk.chars().skip(begin as usize).take(chars as usize));
        }
        (!selected.is_empty()).then_some(selected)
    }

    pub fn current_paragraph(&self) -> Option<String> {
        let book = self.book.as_ref()?;
        book.paragraphs.get(book.loc.para as usize).cloned()
    }

    pub fn preferences(&self) -> (f32, Option<&'static str>) {
        let theme = self.theme.map(|theme| match theme {
            Theme::Paper => "paper",
            Theme::White => "light",
            Theme::Night => "dark",
        });
        (self.font_px, theme)
    }

    pub fn apply_preferences(&mut self, font: f32, theme: Option<&str>) {
        let size = if font.is_finite() {
            (font.clamp(12.0, 48.0) * 10.0).round() / 10.0
        } else {
            DEFAULT_FONT_SIZE
        };
        if self.font_px != size {
            self.font_px = size;
            self.layout_dirty = true;
        }
        self.theme = match theme {
            Some("paper") => Some(Theme::Paper),
            Some("light") => Some(Theme::White),
            Some("dark") => Some(Theme::Night),
            _ => None,
        };
    }

    pub fn navigation_has_focus(&self, ctx: &egui::Context) -> bool {
        self.is_open() && ctx.memory(|memory| memory.has_focus(page_surface_id()))
    }

    pub fn highlight_search(&mut self, loc: Loc, query: &str) {
        self.search_highlight = Some((loc, query.to_owned()));
    }

    fn search_range(&self) -> Option<(Loc, Loc)> {
        let (loc, query) = self.search_highlight.as_ref()?;
        let book = self.book.as_ref()?;
        if book.chapter != loc.chapter {
            return None;
        }
        let para = book.paragraphs.get(loc.para as usize)?;
        let mut source = para.chars().skip(loc.off as usize);
        let folded = query.to_lowercase();
        let mut matched = folded.chars();
        let mut consumed = 0;
        while let Some(target) = matched.next() {
            let value = source.next()?;
            consumed += 1;
            let mut lower = value.to_lowercase();
            if lower.next() != Some(target) || !lower.all(|ch| matched.next() == Some(ch)) {
                return None;
            }
        }
        (consumed != 0).then_some((
            *loc,
            Loc {
                off: loc.off + consumed,
                ..*loc
            },
        ))
    }

    pub fn inline_illustrations(&mut self, id: &str, chapter: u32, entries: &[Illustration]) {
        if !self.is_epub() || self.book_id() != Some(id) || self.chapter_index() != chapter {
            return;
        }
        let anchors: Arc<[IllustrationAnchor]> = entries
            .iter()
            .map(|item| IllustrationAnchor {
                ordinal: item.ordinal,
                before_para: item.before_para,
            })
            .collect();
        self.inline.awaiting_list = false;
        if self.inline.anchors != anchors {
            self.inline.anchors = anchors;
            self.layout_dirty = true;
        }
    }

    pub fn inline_illustrations_failed(&mut self) {
        self.inline.awaiting_list = false;
    }

    pub fn inline_image_request(&mut self) -> Option<Command> {
        if self.inline.waiting.is_some()
            || self.inline.awaiting_list
            || self.inline_decode.is_some()
        {
            return None;
        }
        let book = self.book.as_ref()?;
        let layout = self.layout.as_ref()?;
        let ordinal = layout
            .illustrations(self.page)
            .iter()
            .map(|item| item.ordinal)
            .find(|&ordinal| {
                !self
                    .inline
                    .textures
                    .iter()
                    .any(|(loaded, _)| *loaded == ordinal)
            })?;
        self.inline.waiting = Some((book.chapter, ordinal));
        Some(Command::ReadIllustration {
            book_id: book.summary.id.clone(),
            chapter: book.chapter,
            ordinal,
        })
    }

    pub fn receive_inline_image(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        chapter: u32,
        ordinal: u32,
        image: &IllustrationImage,
    ) {
        if self.book_id() != Some(id)
            || self.inline.waiting != Some((chapter, ordinal))
            || self.inline_decode.is_some()
        {
            return;
        }
        let bytes = image.bytes.clone();
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        if std::thread::Builder::new()
            .name("velmira-inline-illustration".into())
            .spawn(move || {
                let result = vm_media::decode_page(&bytes, MAX_INLINE_EDGE).ok();
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.inline_decode = Some(InlineDecode {
                receiver: rx,
                book_id: id.to_owned(),
                chapter,
                ordinal,
            });
        } else {
            self.inline_image_failed();
        }
    }

    pub fn inline_image_failed(&mut self) {
        if self.inline_decode.is_none() {
            self.finish_image(None);
        }
    }

    fn finish_image(&mut self, texture: Option<TextureHandle>) {
        if let Some((chapter, ordinal)) = self.inline.waiting.take() {
            if self.chapter_index() == chapter
                && self.layout.as_ref().is_some_and(|layout| {
                    layout
                        .illustrations(self.page)
                        .iter()
                        .any(|item| item.ordinal == ordinal)
                })
            {
                self.inline.textures.push((ordinal, texture));
            }
        }
    }

    fn poll_inline_images(&mut self, ctx: &egui::Context) {
        let Some(decode) = self.inline_decode.as_ref() else {
            return;
        };
        let image = match decode.receiver.try_recv() {
            Ok(image) => image,
            Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => return,
        };
        let current = self.book_id() == Some(decode.book_id.as_str())
            && self.inline.waiting == Some((decode.chapter, decode.ordinal));
        self.inline_decode = None;
        if current {
            let texture = image.map(|image| {
                ctx.load_texture(
                    "reader-inline-illustration",
                    ColorImage::from_rgba_unmultiplied(
                        [image.width as usize, image.height as usize],
                        &image.pixels,
                    ),
                    TextureOptions::LINEAR,
                )
            });
            self.finish_image(texture);
        }
    }

    pub fn open(&mut self, summary: BookSummary, toc: Vec<String>, index: u32, chapter: Chapter) {
        let saved = summary.progress.filter(|loc| loc.chapter == index);
        let loc = saved.unwrap_or(Loc {
            chapter: index,
            ..Loc::default()
        });
        self.close();
        self.book = Some(ReadingBook {
            summary,
            toc,
            chapter: index,
            title: chapter.title,
            paragraphs: chapter.paragraphs.into(),
            loc,
        });
        self.pending = Some(Request {
            position: saved.map_or(Flip::First, Flip::Keep),
            persist_page_start: false,
            turn: 0,
        });
        self.inline.awaiting_list = self.is_epub();
    }

    pub fn ready(&self) -> bool {
        self.texture.is_some()
    }

    pub fn close(&mut self) {
        self.book = None;
        self.texture = None;
        self.turn = None;
        self.pending = None;
        self.running = None;
        self.chapter_request = None;
        self.unsaved = None;
        self.page_count = 0;
        self.layout_dirty = true;
        self.layout_worker_failed = false;
        self.first_page = true;
        self.missing_glyphs = 0;
        self.layout = None;
        self.annotations.clear();
        self.selection = None;
        self.mark = None;
        self.tools_requested = false;
        self.search_highlight = None;
        self.inline = InlineImages::default();
        self.touch = TouchGesture::default();
    }

    pub fn prepare_jump(&mut self, loc: Loc) -> Option<(String, u32)> {
        let request = Request {
            position: Flip::Keep(loc),
            persist_page_start: false,
            turn: self
                .book
                .as_ref()
                .map_or(0, |book| match loc.cmp(&book.loc) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Greater => 1,
                    std::cmp::Ordering::Equal => 0,
                }),
        };
        self.request_chapter(loc.chapter, request)
    }

    pub fn prepare_adjacent(&mut self, index: u32, last_page: bool) -> Option<(String, u32)> {
        let direction = if index < self.chapter_index() { -1 } else { 1 };
        self.request_chapter(
            index,
            Request {
                position: if last_page { Flip::Last } else { Flip::First },
                persist_page_start: true,
                turn: direction,
            },
        )
    }

    fn request_chapter(&mut self, index: u32, request: Request) -> Option<(String, u32)> {
        let book = self.book.as_ref()?;
        if self.chapter_request.is_some() || index >= book.summary.chapter_count {
            return None;
        }
        if index == book.chapter {
            self.pending = Some(request);
            if matches!(request.position, Flip::Keep(_)) {
                // Keep only this jump's result, including when a resize is in flight.
                self.running = None;
            }
            return None;
        }
        self.chapter_request = Some(ChapterRequest { index, request });
        Some((book.summary.id.clone(), index))
    }

    pub fn reset_load(&mut self) {
        self.chapter_request = None;
    }

    pub fn chapter_read(&mut self, id: &str, index: u32, chapter: Chapter) -> bool {
        let (Some(book), Some(request)) = (&mut self.book, self.chapter_request) else {
            return false;
        };
        if book.summary.id != id || request.index != index {
            return false;
        }
        self.chapter_request = None;
        book.chapter = index;
        book.title = chapter.title;
        book.paragraphs = chapter.paragraphs.into();
        book.loc = Loc {
            chapter: index,
            ..Loc::default()
        };
        self.turn = None;
        self.running = None;
        self.layout_dirty = true;
        self.pending = Some(request.request);
        self.page_count = 0;
        self.layout = None;
        self.annotations.clear();
        self.selection = None;
        self.inline = InlineImages {
            awaiting_list: self.is_epub(),
            ..InlineImages::default()
        };
        self.touch = TouchGesture::default();
        true
    }

    pub fn unsaved(&self) -> Option<(String, Loc)> {
        Some((self.book.as_ref()?.summary.id.clone(), self.unsaved?))
    }

    pub fn progress_sent(&mut self, loc: Loc) {
        if self.unsaved == Some(loc) {
            self.unsaved = None;
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Result<Shown, String>> {
        self.poll_inline_images(ctx);
        loop {
            let (id, result) = match self.worker.receive.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    let running = self.running.take().is_some();
                    let pending = self.pending.take().is_some();
                    let failed = std::mem::take(&mut self.layout_worker_failed);
                    return (running || pending || failed)
                        .then(|| Err("Layout worker exited".into()));
                }
            };
            let Some((active, request, theme, scale)) = self.running else {
                continue;
            };
            if id != active {
                continue;
            }
            self.running = None;
            let rendered = match result {
                Ok(rendered) => rendered,
                Err(error) => {
                    self.layout_dirty = true;
                    return Some(Err(error));
                }
            };
            let Some(book) = &mut self.book else { continue };
            let target = match request.position {
                Flip::Keep(loc) if !request.persist_page_start => loc,
                _ => rendered.start,
            };
            let target = rendered
                .layout
                .page_for(target)
                .map_or(rendered.start, |_| target);
            if book.summary.progress != Some(target) {
                book.loc = target;
                book.summary.progress = Some(target);
                self.unsaved = Some(target);
            }
            if self
                .search_highlight
                .as_ref()
                .is_some_and(|(loc, _)| rendered.layout.page_for(*loc) != Some(rendered.index))
            {
                self.search_highlight = None;
            }
            let image = ColorImage::from_rgba_unmultiplied(
                [
                    rendered.raster.width as usize,
                    rendered.raster.height as usize,
                ],
                &rendered.raster.rgba,
            );
            if request.turn != 0 {
                let illustrations = self.layout.as_ref().map_or_else(Vec::new, |layout| {
                    layout
                        .illustrations(self.page)
                        .iter()
                        .filter_map(|item| {
                            self.inline.textures.iter().find_map(|(id, texture)| {
                                (*id == item.ordinal)
                                    .then_some(texture.as_ref())
                                    .flatten()
                                    .map(|texture| (texture.clone(), item.rect))
                            })
                        })
                        .collect()
                });
                self.turn = self.texture.take().map(|(previous, theme)| PageTurn {
                    previous,
                    scale: self.render_scale,
                    theme,
                    direction: f32::from(request.turn),
                    start: ctx.input(|input| input.time),
                    illustrations,
                });
            }
            if let Some((texture, rendered_theme)) = &mut self.texture {
                texture.set(image, TextureOptions::LINEAR);
                *rendered_theme = theme;
            } else {
                self.texture = Some((
                    ctx.load_texture("reader-page", image, TextureOptions::LINEAR),
                    theme,
                ));
            }
            self.render_scale = scale;
            self.page = rendered.index;
            self.page_count = rendered.count;
            self.missing_glyphs = rendered.missing_glyphs;
            self.layout = Some(rendered.layout);
            if let Some(layout) = &self.layout {
                self.inline.textures.retain(|(id, _)| {
                    layout
                        .illustrations(self.page)
                        .iter()
                        .any(|item| item.ordinal == *id)
                });
            }
            self.selection = None;
            let first = std::mem::replace(&mut self.first_page, false);
            return Some(Ok(Shown {
                first,
                layout_ms: rendered.layout_ms,
                raster_ms: rendered.raster_ms,
            }));
        }
    }

    fn request_navigation(&mut self, navigation: Navigation) -> Option<(u32, bool)> {
        if navigation == Navigation::Tools {
            self.tools_requested = true;
            return None;
        }
        if self.pending.is_some() || self.running.is_some() || self.chapter_request.is_some() {
            return None;
        }
        let book = self.book.as_ref()?;
        match navigation {
            Navigation::Tools => None,
            Navigation::Chapter(step) => {
                let chapter = book.chapter.checked_add_signed(i32::from(step))?;
                (chapter < book.summary.chapter_count).then_some((chapter, false))
            }
            Navigation::Page(step) => {
                if self.page_count == 0 || self.layout_dirty {
                    return None;
                }
                let page = self.page.checked_add_signed(isize::from(step));
                if let Some(page) = page.filter(|page| *page < self.page_count) {
                    self.pending = Some(Request {
                        position: Flip::Page(page),
                        persist_page_start: true,
                        turn: step,
                    });
                    return None;
                }
                let chapter = book.chapter.checked_add_signed(i32::from(step))?;
                (chapter < book.summary.chapter_count).then_some((chapter, step < 0))
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        chinese: bool,
        resizing: bool,
        dialog_open: bool,
        #[cfg(target_os = "android")] tap_area: egui::Rect,
    ) -> Option<(u32, bool)> {
        let tr = |zh: &'static str, en: &'static str| if chinese { zh } else { en };
        let book = self.book.as_ref()?;
        let night = ui.visuals().dark_mode;
        let p = crate::theme::palette(night);
        let has_selection = self.selection.is_some_and(|(a, b)| a != b);
        let chapter_count = book.summary.chapter_count.max(1);
        let current_chapter = book.chapter;
        #[cfg(not(target_os = "android"))]
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(&book.title).strong().size(20.0)).truncate(),
            );
            let position = if chinese {
                format!(
                    "第{}章 / 共{}章",
                    book.chapter + 1,
                    book.summary.chapter_count
                )
            } else {
                format!(
                    "Chapter {} of {}",
                    book.chapter + 1,
                    book.summary.chapter_count
                )
            };
            ui.label(egui::RichText::new(position).size(13.0).color(p.muted));
        });
        let mut navigation = None;
        let mut copy_requested = false;
        let mobile = cfg!(target_os = "android");
        if !mobile || has_selection {
            ui.horizontal_wrapped(|ui| {
                if let Some((a, b)) = self.selection {
                    let (start, end) = (a.min(b), a.max(b));
                    if start.para == end.para
                        && end.off > start.off
                        && ui.button(tr("添加批注", "Add note")).clicked()
                    {
                        self.mark = Some((start, end.off));
                    }
                }
                copy_requested =
                    has_selection && ui.button(tr("复制选中文字", "Copy selection")).clicked();
                #[cfg(not(target_os = "android"))]
                {
                    let changed = ui
                        .add(
                            egui::Slider::new(&mut self.font_px, 12.0..=48.0)
                                .text(tr("字号", "Font"))
                                .step_by(0.1)
                                .fixed_decimals(1),
                        )
                        .changed();
                    let reset = ui
                        .add_enabled(
                            (self.font_px - DEFAULT_FONT_SIZE).abs() > 0.01,
                            egui::Button::new(tr("恢复字号", "Reset font")),
                        )
                        .clicked();
                    if reset {
                        self.font_px = DEFAULT_FONT_SIZE;
                    }
                    if changed || reset {
                        self.layout_dirty = true;
                    }
                    egui::ComboBox::from_id_salt("reader-theme")
                        .selected_text(match self.theme {
                            None => tr("跟随应用", "Follow app"),
                            Some(Theme::Paper) => tr("纸色", "Paper"),
                            Some(Theme::White) => tr("浅色", "Light"),
                            Some(Theme::Night) => tr("深色", "Dark"),
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.theme,
                                None,
                                tr("跟随应用", "Follow app"),
                            );
                            ui.selectable_value(
                                &mut self.theme,
                                Some(Theme::Paper),
                                tr("纸色", "Paper"),
                            );
                            ui.selectable_value(
                                &mut self.theme,
                                Some(Theme::White),
                                tr("浅色", "Light"),
                            );
                            ui.selectable_value(
                                &mut self.theme,
                                Some(Theme::Night),
                                tr("深色", "Dark"),
                            );
                        });
                }
            });
        }
        if !mobile && self.missing_glyphs > 0 {
            ui.label(tr(
                "部分文字缺少字形。请安装包含这些文字的系统字体。",
                "Some glyphs are missing. Install a system font that covers this text.",
            ));
        }
        let available = ui.available_size().max(egui::Vec2::ZERO);
        let margin = if mobile {
            egui::vec2((available.x * 0.05).clamp(16.0, 28.0), 13.0)
        } else {
            egui::vec2((available.x * 0.04).clamp(12.0, 36.0), 14.0)
        };
        let footer = if mobile { 34.0 } else { 48.0 };
        let size = egui::vec2(available.x, (available.y - footer).max(0.0));
        let geometry = PageGeometry::new(
            size - margin * 2.0,
            self.font_px,
            ui.ctx().pixels_per_point(),
        );
        if !resizing && self.screen != Some(geometry) {
            self.screen = Some(geometry);
            self.layout_dirty = true;
            self.turn = None;
        }
        let theme = self
            .theme
            .unwrap_or(if night { Theme::Night } else { Theme::Paper });
        let displayed_theme = self.texture.as_ref().map_or(theme, |(_, theme)| *theme);
        if !resizing
            && (self.layout_dirty || displayed_theme != theme)
            && self.pending.is_none()
            && self.running.is_none()
            && self.chapter_request.is_none()
        {
            self.pending = Some(Request {
                position: Flip::Keep(book.loc),
                persist_page_start: false,
                turn: 0,
            });
        }
        if (!resizing || self.texture.is_none()) && !self.inline.awaiting_list {
            self.submit(ui.ctx(), geometry, theme);
        }
        let (_, rect) = ui.allocate_space(size);
        #[cfg(target_os = "android")]
        let tap_area = rect.intersect(tap_area);
        #[cfg(not(target_os = "android"))]
        let tap_area = rect;
        let response = ui.interact(tap_area, page_surface_id(), egui::Sense::click_and_drag());
        let blocked = dialog_open || egui::Popup::is_any_open(ui.ctx()) || resizing;
        let (touch_page, touch_input) = ui.input(|input| {
            if input.focused {
                self.touch.update(&input.events, tap_area, !blocked)
            } else {
                self.touch = TouchGesture::default();
                (None, false)
            }
        });
        let time = ui.input(|input| input.time);
        let progress = self
            .turn
            .as_ref()
            .map(|turn| ((time - turn.start) / PAGE_TURN_SECONDS) as f32);
        if progress.is_some_and(|progress| progress >= 1.0) {
            self.turn = None;
        }
        if !blocked {
            if !ui.ctx().wants_keyboard_input() || self.navigation_has_focus(ui.ctx()) {
                navigation = ui.input(|input| keyboard_navigation(input).or(navigation));
            }
            if let Some(action) = touch_page {
                if action != Navigation::Tools {
                    self.selection = None;
                }
                navigation = Some(action);
            } else if response.clicked_by(PointerButton::Primary) && !touch_input {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let action = tap_navigation(tap_area, pointer.x, mobile);
                    if action != Navigation::Tools {
                        self.selection = None;
                    }
                    navigation = Some(action);
                    response.request_focus();
                }
            }
        }
        if !blocked && (self.navigation_has_focus(ui.ctx()) || !ui.ctx().wants_keyboard_input()) {
            copy_requested |= ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Copy))
            });
        }
        if !blocked && copy_requested {
            if let Some(text) = self.selected_text() {
                ui.ctx().copy_text(text);
            }
        }
        let (bg, fg) = displayed_theme.colors();
        let mut incoming = rect;
        let painter = ui.painter_at(rect);
        if let (Some(turn), Some(progress)) = (&self.turn, progress) {
            let eased = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(3);
            let distance = 0.22 * rect.width() * turn.direction;
            let outgoing = rect.translate(egui::vec2(-distance * eased, 0.0));
            incoming = rect.translate(egui::vec2(distance * (1.0 - eased), 0.0));
            painter.rect_filled(rect, 0, turn.theme.colors().0);
            let old_body = text_rect(outgoing, turn.previous.size_vec2() / turn.scale, margin.y);
            painter.image(
                turn.previous.id(),
                old_body,
                IMAGE_UV,
                Color32::WHITE.gamma_multiply(1.0 - eased),
            );
            for (image, slot) in &turn.illustrations {
                paint_illustration(
                    &painter,
                    image,
                    illustration_rect(*slot, old_body.min, turn.scale),
                    Color32::WHITE.gamma_multiply(1.0 - eased),
                );
            }
            painter.rect_filled(incoming, 0, bg.gamma_multiply(0.80 + 0.20 * eased));
            let edge = if turn.direction > 0.0 {
                incoming.left()
            } else {
                incoming.right()
            };
            let shadow = egui::Rect::from_center_size(
                egui::pos2(edge, rect.center().y),
                egui::vec2(5.0, rect.height()),
            );
            painter.rect_filled(
                shadow,
                2.0,
                Color32::from_black_alpha((55.0 * (1.0 - eased)) as u8),
            );
            ui.ctx().request_repaint();
        } else {
            painter.rect_filled(rect, 0, bg);
        }
        if let Some((texture, _)) = &self.texture {
            let scale = self.render_scale;
            let visible = text_rect(incoming, texture.size_vec2() / scale, margin.y);
            if let Some(layout) = &self.layout {
                let painter = ui.painter_at(visible.intersect(rect));
                let draw = |start: Loc, end: Loc, color| {
                    for r in layout.range_rects(self.page, (start, end)) {
                        let position = visible.min + egui::vec2(r.x, r.y) / scale;
                        let size = egui::vec2(r.width, r.height) / scale;
                        painter.rect_filled(egui::Rect::from_min_size(position, size), 2.0, color);
                    }
                };
                for mark in &self.annotations {
                    let color = match mark.color {
                        AnnotationColor::Yellow => {
                            Color32::from_rgba_unmultiplied(225, 176, 17, 114)
                        }
                        AnnotationColor::Green => {
                            Color32::from_rgba_unmultiplied(75, 174, 103, 115)
                        }
                        AnnotationColor::Blue => Color32::from_rgba_unmultiplied(60, 146, 217, 110),
                        AnnotationColor::Pink => {
                            Color32::from_rgba_unmultiplied(223, 105, 151, 112)
                        }
                    };
                    draw(
                        mark.loc,
                        Loc {
                            off: mark.end_off,
                            ..mark.loc
                        },
                        color,
                    );
                }
                if let Some((start, end)) = self.search_range() {
                    draw(
                        start,
                        end,
                        Color32::from_rgba_unmultiplied(247, 155, 34, 155),
                    );
                }
                let dragging = response.dragged_by(PointerButton::Primary)
                    || response.drag_stopped_by(PointerButton::Primary);
                if !blocked && self.turn.is_none() && dragging && !touch_input {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        let to_loc = |p: egui::Pos2| {
                            let p = (p - visible.min) * scale;
                            layout.hit_test(self.page, p.x, p.y)
                        };
                        if response.drag_started_by(PointerButton::Primary) {
                            let origin = ui
                                .input(|input| input.pointer.press_origin())
                                .unwrap_or(pointer);
                            self.selection = to_loc(origin).map(|loc| (loc, loc));
                        }
                        if let (Some((anchor, _)), Some(caret)) = (self.selection, to_loc(pointer))
                        {
                            self.selection = Some((anchor, caret));
                        }
                    }
                }
                if let Some((start, end)) = self.selection {
                    draw(
                        start.min(end),
                        start.max(end),
                        Color32::from_rgba_unmultiplied(65, 134, 201, 110),
                    );
                }
                for item in layout.illustrations(self.page) {
                    let slot = illustration_rect(item.rect, visible.min, scale);
                    match self
                        .inline
                        .textures
                        .iter()
                        .find(|(id, _)| *id == item.ordinal)
                    {
                        Some((_, Some(image))) => {
                            paint_illustration(&painter, image, slot, Color32::WHITE)
                        }
                        item => {
                            let label = if item.is_some() {
                                tr("插图不可用", "Image unavailable")
                            } else {
                                tr("正在加载插图…", "Loading illustration…")
                            };
                            painter.rect_filled(slot, 6.0, p.surface2);
                            painter.text(
                                slot.center(),
                                egui::Align2::CENTER_CENTER,
                                label,
                                egui::FontId::proportional(14.0),
                                p.muted,
                            );
                        }
                    }
                }
            }
            ui.painter_at(rect)
                .image(texture.id(), visible, IMAGE_UV, Color32::WHITE);
        } else {
            ui.painter_at(rect).text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr("正在排版…", "Preparing page…"),
                egui::FontId::proportional(16.0),
                fg,
            );
        }
        if self.page_count > 0 {
            let fraction = (current_chapter as f32
                + (self.page + 1) as f32 / self.page_count as f32)
                / chapter_count as f32;
            if mobile {
                let percent = if fraction * 100.0 < 0.1 {
                    "<0.1%".into()
                } else {
                    format!("{:.1}%", fraction * 100.0)
                };
                let position = format!("{} / {} · {percent}", self.page + 1, self.page_count);
                let status = fg.gamma_multiply(0.68);
                let title = self.book.as_ref().map_or("", |book| book.title.as_str());
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.add_sized(
                        [(ui.available_width() - 130.0).max(1.0), 26.0],
                        egui::Label::new(egui::RichText::new(title).size(12.0).color(status))
                            .truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new(position).size(12.0).color(status));
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!blocked, egui::Button::new(tr("上一页", "Previous")))
                        .clicked()
                    {
                        navigation = Some(Navigation::Page(-1));
                    }
                    ui.label(format!("{} / {}", self.page + 1, self.page_count));
                    if ui
                        .add_enabled(!blocked, egui::Button::new(tr("下一页", "Next")))
                        .clicked()
                    {
                        navigation = Some(Navigation::Page(1));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{:.1}%", fraction * 100.0)).color(p.muted),
                        );
                    });
                });
                let (track, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 3.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(track, 1.5, p.line);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        track.min,
                        egui::vec2(track.width() * fraction.clamp(0.0, 1.0), track.height()),
                    ),
                    1.5,
                    p.accent,
                );
            }
        }
        let chapter = if resizing || dialog_open {
            None
        } else {
            navigation.and_then(|navigation| self.request_navigation(navigation))
        };
        if self.pending.is_some() && !resizing && !self.inline.awaiting_list {
            ui.ctx().request_repaint();
        }
        chapter
    }

    fn submit(&mut self, ctx: &egui::Context, geometry: PageGeometry, theme: Theme) {
        let Some(book) = self.book.as_ref() else {
            return;
        };
        let Some(request) = self.pending else { return };
        if self.running.is_some() {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let color = theme.colors().1.to_array();
        let reflow = self.layout_dirty;
        let command = if reflow {
            LayoutCommand::Reflow {
                chapter: book.chapter,
                paragraphs: Arc::clone(&book.paragraphs),
                illustrations: Arc::clone(&self.inline.anchors),
                page: geometry.page,
                gutter: geometry.gutter,
                position: request.position,
                color,
            }
        } else {
            LayoutCommand::Flip {
                position: request.position,
                color,
            }
        };
        match self.worker.send.try_send((self.next_id, command)) {
            Ok(()) => {
                let scale = if reflow {
                    geometry.scale
                } else {
                    self.render_scale
                };
                self.running = Some((self.next_id, request, theme, scale));
                self.pending = None;
                self.layout_dirty = false;
            }
            Err(TrySendError::Full(_)) => ctx.request_repaint_after(Duration::from_millis(20)),
            Err(TrySendError::Disconnected(_)) => {
                self.pending = None;
                self.layout_dirty = false;
                self.layout_worker_failed = true;
                ctx.request_repaint();
            }
        }
    }
}

const IMAGE_UV: egui::Rect = egui::Rect {
    min: egui::Pos2::ZERO,
    max: egui::pos2(1.0, 1.0),
};

fn text_rect(canvas: egui::Rect, size: egui::Vec2, top: f32) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(canvas.center().x - size.x * 0.5, canvas.top() + top),
        size,
    )
}

fn illustration_rect(slot: vm_text::Rect, origin: egui::Pos2, scale: f32) -> egui::Rect {
    egui::Rect::from_min_size(
        origin + egui::vec2(slot.x, slot.y) / scale,
        egui::vec2(slot.width, slot.height) / scale,
    )
}

fn paint_illustration(
    painter: &egui::Painter,
    image: &TextureHandle,
    slot: egui::Rect,
    tint: Color32,
) {
    let size = image.size_vec2();
    let scale = (slot.width() / size.x).min(slot.height() / size.y);
    painter.image(
        image.id(),
        egui::Rect::from_center_size(slot.center(), size * scale),
        IMAGE_UV,
        tint,
    );
}

impl TouchGesture {
    fn update(
        &mut self,
        events: &[egui::Event],
        canvas: egui::Rect,
        enabled: bool,
    ) -> (Option<Navigation>, bool) {
        let mut saw_touch = !self.active.is_empty();
        let mut page = None;
        if !enabled {
            self.start = None;
        }
        for event in events {
            let egui::Event::Touch {
                device_id,
                id,
                phase,
                pos,
                ..
            } = event
            else {
                continue;
            };
            saw_touch = true;
            let key = (*device_id, *id);
            match phase {
                egui::TouchPhase::Start => {
                    self.start = (enabled && self.active.is_empty() && canvas.contains(*pos))
                        .then_some(*pos);
                    if !self.active.contains(&key) {
                        self.active.push(key);
                    }
                }
                egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                    if let Some(index) = self.active.iter().position(|&known| known == key) {
                        self.active.swap_remove(index);
                        if let Some(start) = self.start.take() {
                            let delta = *pos - start;
                            if *phase == egui::TouchPhase::End {
                                if delta.x.abs() >= 46.0 && delta.x.abs() > 1.3 * delta.y.abs() {
                                    page =
                                        Some(Navigation::Page(if delta.x < 0.0 { 1 } else { -1 }));
                                } else if delta.length() < 10.0 && canvas.contains(*pos) {
                                    page = Some(tap_navigation(
                                        canvas,
                                        pos.x,
                                        cfg!(target_os = "android"),
                                    ));
                                }
                            }
                        }
                    }
                }
                egui::TouchPhase::Move => {}
            }
        }
        (page, saw_touch)
    }
}

fn keyboard_navigation(input: &egui::InputState) -> Option<Navigation> {
    if input.modifiers.alt || input.modifiers.ctrl || input.modifiers.command {
        return None;
    }
    if input.key_pressed(Key::ArrowUp) {
        Some(Navigation::Chapter(-1))
    } else if input.key_pressed(Key::ArrowDown) {
        Some(Navigation::Chapter(1))
    } else if input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::PageUp) {
        Some(Navigation::Page(-1))
    } else if input.key_pressed(Key::ArrowRight) || input.key_pressed(Key::PageDown) {
        Some(Navigation::Page(1))
    } else if input.key_pressed(Key::Space) {
        Some(Navigation::Page(if input.modifiers.shift { -1 } else { 1 }))
    } else {
        None
    }
}
