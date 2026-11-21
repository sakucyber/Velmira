use crate::workers::{Flip, LayoutCommand, LayoutWorker};
use crossbeam_channel::TrySendError;
use eframe::egui::{self, Color32, ColorImage, Key, PointerButton, TextureHandle, TextureOptions};
use std::{sync::Arc, time::Duration};
use vm_core::{BookSummary, Chapter, Loc};
use vm_store::{Annotation, AnnotationColor};
use vm_text::{PageBox, PageLayout};

const PAGE_TURN_SECONDS: f64 = 0.20;

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
    theme: Theme,
    direction: f32,
    start: f64,
}

enum Navigation {
    Page(i8),
    Chapter(i8),
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

pub struct Reader {
    worker: LayoutWorker,
    book: Option<ReadingBook>,
    chapter_request: Option<ChapterRequest>,
    pending: Option<Request>,
    running: Option<(u64, Request, Theme)>,
    next_id: u64,
    screen: Option<(u32, u32, u32, u32)>,
    layout_dirty: bool,
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
}

impl Reader {
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
            layout_dirty: true,
            texture: None,
            turn: None,
            page: 0,
            page_count: 0,
            theme: None,
            font_px: 19.0,
            missing_glyphs: 0,
            unsaved: None,
            first_page: true,
            layout: None,
            annotations: Vec::new(),
            selection: None,
            mark: None,
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
    pub fn take_mark(&mut self) -> Option<(Loc, u32)> {
        self.mark.take()
    }

    pub fn open(&mut self, summary: BookSummary, toc: Vec<String>, index: u32, chapter: Chapter) {
        let loc = summary
            .progress
            .filter(|loc| loc.chapter == index)
            .unwrap_or(Loc {
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
            position: Flip::Keep(loc),
            persist_page_start: false,
            turn: 0,
        });
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
        self.first_page = true;
        self.missing_glyphs = 0;
        self.layout = None;
        self.annotations.clear();
        self.selection = None;
        self.mark = None;
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
        while let Ok((id, result)) = self.worker.receive.try_recv() {
            let Some((active, request, theme)) = self.running else {
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
            let image = ColorImage::from_rgba_unmultiplied(
                [
                    rendered.raster.width as usize,
                    rendered.raster.height as usize,
                ],
                &rendered.raster.rgba,
            );
            if request.turn != 0 {
                self.turn = self.texture.take().map(|(previous, theme)| PageTurn {
                    previous,
                    theme,
                    direction: f32::from(request.turn),
                    start: ctx.input(|input| input.time),
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
            self.page = rendered.index;
            self.page_count = rendered.count;
            self.missing_glyphs = rendered.missing_glyphs;
            self.layout = Some(rendered.layout);
            self.selection = None;
            let first = std::mem::replace(&mut self.first_page, false);
            return Some(Ok(Shown {
                first,
                layout_ms: rendered.layout_ms,
                raster_ms: rendered.raster_ms,
            }));
        }
        None
    }

    fn request_navigation(&mut self, navigation: Navigation) -> Option<(u32, bool)> {
        if self.pending.is_some() || self.running.is_some() || self.chapter_request.is_some() {
            return None;
        }
        let book = self.book.as_ref()?;
        match navigation {
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
    ) -> Option<(u32, bool)> {
        let tr = |zh: &'static str, en: &'static str| if chinese { zh } else { en };
        let book = self.book.as_ref()?;
        let night = ui.visuals().dark_mode;
        let p = crate::theme::palette(night);
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
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("上一页", "Previous")).clicked() {
                navigation = Some(Navigation::Page(-1));
            }
            if self.page_count > 0 {
                ui.label(format!("{} / {}", self.page + 1, self.page_count));
            }
            if ui.button(tr("下一页", "Next")).clicked() {
                navigation = Some(Navigation::Page(1));
            }
            ui.separator();
            if let Some((a, b)) = self.selection {
                let (start, end) = (a.min(b), a.max(b));
                if start.para == end.para
                    && end.off > start.off
                    && ui.button(tr("添加批注", "Add note")).clicked()
                {
                    self.mark = Some((start, end.off));
                }
            }
            let changed = ui
                .add(
                    egui::Slider::new(&mut self.font_px, 12.0..=48.0)
                        .text(tr("字号", "Font"))
                        .step_by(1.0),
                )
                .changed();
            if changed {
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
                    ui.selectable_value(&mut self.theme, None, tr("跟随应用", "Follow app"));
                    ui.selectable_value(&mut self.theme, Some(Theme::Paper), tr("纸色", "Paper"));
                    ui.selectable_value(&mut self.theme, Some(Theme::White), tr("浅色", "Light"));
                    ui.selectable_value(&mut self.theme, Some(Theme::Night), tr("深色", "Dark"));
                });
        });
        if self.missing_glyphs > 0 {
            ui.label(tr(
                "部分文字缺少字形。请安装包含这些文字的系统字体。",
                "Some glyphs are missing. Install a system font that covers this text.",
            ));
        }
        let available = ui.available_size().max(egui::Vec2::ZERO);
        let margin = egui::vec2(24.0, 18.0);
        let scale = ui.ctx().pixels_per_point().clamp(1.0, 2.0);
        let bg_size = available
            .min(egui::vec2(1080.0, 1100.0))
            .min(egui::vec2(1600.0, 1400.0) / scale + margin * 2.0);
        let desired = (bg_size - margin * 2.0).max(egui::vec2(104.0, 104.0));
        let width = (desired.x * scale).floor() as u32;
        let height = (desired.y * scale).floor() as u32;
        let scale_key = scale.to_bits();
        let form = (width, height, self.font_px.to_bits(), scale_key);
        if !resizing && self.screen != Some(form) {
            self.screen = Some(form);
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
        if !resizing || self.texture.is_none() {
            self.submit(
                ui.ctx(),
                PageBox {
                    width: width as f32,
                    height: height as f32,
                    font_size: self.font_px * scale,
                    line_height: self.font_px * 1.55 * scale,
                },
                theme,
            );
        }
        let (id, canvas) = ui.allocate_space(egui::vec2(available.x, bg_size.y));
        let rect = egui::Rect::from_min_size(
            egui::pos2(canvas.center().x - bg_size.x / 2.0, canvas.top()),
            bg_size,
        );
        let response = ui.interact(canvas, id, egui::Sense::click_and_drag());
        let time = ui.input(|input| input.time);
        let progress = self
            .turn
            .as_ref()
            .map(|turn| ((time - turn.start) / PAGE_TURN_SECONDS) as f32);
        if progress.is_some_and(|progress| progress >= 1.0) {
            self.turn = None;
        }
        if !dialog_open && !egui::Popup::is_any_open(ui.ctx()) {
            if !ui.ctx().wants_keyboard_input() || response.has_focus() {
                navigation = ui.input(|input| keyboard_navigation(input).or(navigation));
            }
            if response.clicked_by(PointerButton::Primary) {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let step = if pointer.x < rect.center().x { -1 } else { 1 };
                    self.selection = None;
                    navigation = Some(Navigation::Page(step));
                    response.request_focus();
                }
            }
        }
        let (bg, fg) = displayed_theme.colors();
        let shadow_alpha = if night { 60 } else { 10 };
        for (expand, alpha) in [
            (7.0, shadow_alpha),
            (4.0, shadow_alpha + 6),
            (1.0, shadow_alpha + 12),
        ] {
            ui.painter()
                .rect_filled(rect.expand(expand), 12.0, Color32::from_black_alpha(alpha));
        }
        let mut incoming = rect;
        let painter = ui.painter_at(rect);
        if let (Some(turn), Some(progress)) = (&self.turn, progress) {
            let eased = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(3);
            let distance = 0.22 * rect.width() * turn.direction;
            let outgoing = rect.translate(egui::vec2(-distance * eased, 0.0));
            incoming = rect.translate(egui::vec2(distance * (1.0 - eased), 0.0));
            painter.rect_filled(rect, 9.0, turn.theme.colors().0);
            let old_body =
                egui::Rect::from_min_size(outgoing.min + margin, turn.previous.size_vec2() / scale);
            painter.image(
                turn.previous.id(),
                old_body,
                IMAGE_UV,
                Color32::WHITE.gamma_multiply(1.0 - eased),
            );
            painter.rect_filled(incoming, 9.0, bg.gamma_multiply(0.80 + 0.20 * eased));
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
            painter.rect_filled(rect, 9.0, bg);
        }
        if let Some((texture, _)) = &self.texture {
            let actual = texture.size_vec2() / scale;
            let visible = egui::Rect::from_min_size(incoming.left_top() + margin, actual);
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
                let dragging = response.dragged_by(PointerButton::Primary)
                    || response.drag_stopped_by(PointerButton::Primary);
                if !resizing && self.turn.is_none() && !dialog_open && dragging {
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
        if self.page_count > 0 && rect.width() > 20.0 && rect.height() > 6.0 {
            let fraction = (self.page + 1) as f32 / self.page_count as f32;
            let track = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 10.0, rect.max.y - 6.0),
                egui::vec2(rect.width() - 20.0, 3.0),
            );
            let painter = ui.painter_at(rect);
            painter.rect_filled(track, 1.5, fg.gamma_multiply(0.2));
            painter.rect_filled(
                egui::Rect::from_min_size(
                    track.min,
                    egui::vec2(track.width() * fraction.clamp(0.01, 1.0), track.height()),
                ),
                1.5,
                crate::theme::palette(displayed_theme == Theme::Night).accent,
            );
        }
        let chapter = if resizing || dialog_open {
            None
        } else {
            navigation.and_then(|navigation| self.request_navigation(navigation))
        };
        if self.pending.is_some() && !resizing {
            ui.ctx().request_repaint();
        }
        chapter
    }

    fn submit(&mut self, ctx: &egui::Context, page: PageBox, theme: Theme) {
        let Some(book) = self.book.as_ref() else {
            return;
        };
        let Some(request) = self.pending else { return };
        if self.running.is_some() {
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let color = theme.colors().1.to_array();
        let command = if self.layout_dirty {
            LayoutCommand::Reflow {
                chapter: book.chapter,
                paragraphs: Arc::clone(&book.paragraphs),
                page,
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
                self.running = Some((self.next_id, request, theme));
                self.pending = None;
                self.layout_dirty = false;
            }
            Err(TrySendError::Full(_)) => ctx.request_repaint_after(Duration::from_millis(20)),
            Err(TrySendError::Disconnected(_)) => self.pending = None,
        }
    }
}

const IMAGE_UV: egui::Rect = egui::Rect {
    min: egui::Pos2::ZERO,
    max: egui::pos2(1.0, 1.0),
};

fn keyboard_navigation(input: &egui::InputState) -> Option<Navigation> {
    if input.key_pressed(Key::ArrowUp) {
        Some(Navigation::Chapter(-1))
    } else if input.key_pressed(Key::ArrowDown) {
        Some(Navigation::Chapter(1))
    } else if input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::PageUp) {
        Some(Navigation::Page(-1))
    } else if input.key_pressed(Key::ArrowRight)
        || input.key_pressed(Key::PageDown)
        || input.key_pressed(Key::Space)
    {
        Some(Navigation::Page(1))
    } else {
        None
    }
}
