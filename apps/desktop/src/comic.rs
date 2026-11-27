use crossbeam_channel::{bounded, Receiver, Sender};
use eframe::egui::{self, Color32, ColorImage, Rect, TextureHandle, TextureOptions, Vec2};
use std::thread;
use vm_core::{BookSummary, Loc};
use vm_store::ComicPage;

type DecodeResult = (u64, Result<vm_media::RgbaImage, String>);
const PAGE_TRANSITION_SECONDS: f64 = 0.16;

struct PageTransition {
    previous: TextureHandle,
    previous_scroll: f32,
    direction: f32,
    started: f64,
}

pub struct Comic {
    send: Sender<(u64, ComicPage)>,
    receive: Receiver<DecodeResult>,
    summary: Option<BookSummary>,
    page: u32,
    pending: Option<u32>,
    pending_page: Option<ComicPage>,
    decoding: Option<(u64, u32)>,
    sequence: u64,
    texture: Option<TextureHandle>,
    transition: Option<PageTransition>,
    scroll: f32,
    fit_width: bool,
    #[cfg(target_os = "android")]
    tools_requested: bool,
}

impl Comic {
    pub fn spawn(ctx: egui::Context) -> std::io::Result<Self> {
        let (send, commands) = bounded::<(u64, ComicPage)>(1);
        let (results, receive) = bounded(1);
        thread::Builder::new()
            .name("velmira-comic-image".into())
            .spawn(move || {
                for (id, page) in commands {
                    let rendered =
                        vm_media::decode_page(&page.bytes, 1600).map_err(|e| e.to_string());
                    if results.send((id, rendered)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self {
            send,
            receive,
            summary: None,
            page: 0,
            pending: None,
            pending_page: None,
            decoding: None,
            sequence: 0,
            texture: None,
            transition: None,
            scroll: 0.0,
            fit_width: false,
            #[cfg(target_os = "android")]
            tools_requested: false,
        })
    }

    pub fn book_id(&self) -> Option<&str> {
        self.summary.as_ref().map(|book| book.id.as_str())
    }

    pub fn ready(&self) -> bool {
        self.texture.is_some()
    }

    pub fn page_index(&self) -> u32 {
        self.page
    }

    #[cfg(target_os = "android")]
    pub fn page_count(&self) -> u32 {
        self.summary.as_ref().map_or(0, |book| book.chapter_count)
    }

    pub fn set_fit_width(&mut self, fit_width: bool) {
        self.fit_width = fit_width;
    }

    pub fn request_page(&mut self, index: u32) -> Option<(String, u32)> {
        let book = self.summary.as_ref()?;
        if index == self.page
            || index >= book.chapter_count
            || self.pending.is_some()
            || self.pending_page.is_some()
            || self.decoding.is_some()
        {
            return None;
        }
        self.pending = Some(index);
        Some((book.id.clone(), index))
    }

    pub fn open(&mut self, book: BookSummary, page: ComicPage) {
        self.close();
        self.summary = Some(book);
        self.page = page.index;
        self.pending_page = Some(page);
    }

    pub fn close(&mut self) {
        self.summary = None;
        self.pending_page = None;
        self.decoding = None;
        self.pending = None;
        self.texture = None;
        self.transition = None;
        self.scroll = 0.0;
    }

    pub fn reset_load(&mut self) {
        self.pending = None;
    }

    pub fn loaded(&mut self, book_id: &str, page: ComicPage) {
        if self.pending == Some(page.index) && self.book_id() == Some(book_id) {
            self.pending = None;
            self.pending_page = Some(page);
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Result<(String, Loc), String>> {
        let mut ready = None;
        while let Ok((id, decoded)) = self.receive.try_recv() {
            let Some((active, page)) = self.decoding else {
                continue;
            };
            if id != active {
                continue;
            }
            self.decoding = None;
            if let Some(book) = self.summary.as_ref() {
                ready = Some(decoded.map(|image| {
                    let texture = ctx.load_texture(
                        "comic-page",
                        ColorImage::from_rgba_unmultiplied(
                            [image.width as usize, image.height as usize],
                            &image.pixels,
                        ),
                        TextureOptions::LINEAR,
                    );
                    self.transition = self.texture.replace(texture).and_then(|previous| {
                        (page != self.page).then(|| PageTransition {
                            previous,
                            previous_scroll: self.scroll,
                            direction: if page > self.page { 1.0 } else { -1.0 },
                            started: ctx.input(|input| input.time),
                        })
                    });
                    self.page = page;
                    (
                        book.id.clone(),
                        Loc {
                            chapter: self.page,
                            ..Loc::default()
                        },
                    )
                }));
            }
        }
        if let Some(page) = self.pending_page.take() {
            let index = page.index;
            self.sequence = self.sequence.wrapping_add(1);
            match self.send.try_send((self.sequence, page)) {
                Ok(()) => {
                    self.decoding = Some((self.sequence, index));
                }
                Err(crossbeam_channel::TrySendError::Full((_, page))) => {
                    self.pending_page = Some(page);
                    ctx.request_repaint_after(std::time::Duration::from_millis(20));
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    return Some(Err("Image decoding worker exited".into()))
                }
            }
        }
        ready
    }

    #[cfg(target_os = "android")]
    pub fn take_tools_request(&mut self) -> bool {
        std::mem::take(&mut self.tools_requested)
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        chinese: bool,
        #[cfg(not(target_os = "android"))] keyboard_enabled: bool,
        #[cfg(target_os = "android")] blocked: bool,
        #[cfg(target_os = "android")] tap_area: Rect,
    ) -> Option<(String, u32)> {
        let book = self.summary.as_ref()?;
        let total_pages = book.chapter_count;
        let tr = |zh: &'static str, en: &'static str| if chinese { zh } else { en };
        #[cfg(not(target_os = "android"))]
        ui.add(egui::Label::new(egui::RichText::new(&book.title).strong().size(20.0)).truncate());
        let busy = self.pending.is_some() || self.decoding.is_some() || self.pending_page.is_some();
        let (mut previous, mut next) = (false, false);
        #[cfg(not(target_os = "android"))]
        {
            let palette = crate::theme::palette(ui.visuals().dark_mode);
            let tokens = crate::theme::Tokens::desktop();
            crate::ui::card(ui, palette, tokens, |ui| {
                ui.horizontal_wrapped(|ui| {
                    previous = ui
                        .add_enabled(
                            !busy && self.page > 0,
                            egui::Button::new(tr("上一页", "Previous")),
                        )
                        .clicked();
                    ui.label(format!("{} / {total_pages}", self.page + 1));
                    next = ui
                        .add_enabled(
                            !busy && self.page + 1 < total_pages,
                            egui::Button::new(tr("下一页", "Next")),
                        )
                        .clicked();
                    ui.separator();
                    ui.selectable_value(&mut self.fit_width, false, tr("适页", "Fit page"));
                    ui.selectable_value(&mut self.fit_width, true, tr("适宽", "Fit width"));
                });
            });
        }
        #[cfg(not(target_os = "android"))]
        let blocked = false;
        #[cfg(target_os = "android")]
        let keyboard_enabled = true;
        if keyboard_enabled
            && ui.is_enabled()
            && !blocked
            && !busy
            && !ui.ctx().wants_keyboard_input()
        {
            (previous, next) = ui.input(|state| {
                (
                    previous
                        || state.key_pressed(egui::Key::ArrowLeft)
                        || state.key_pressed(egui::Key::PageUp),
                    next || state.key_pressed(egui::Key::ArrowRight)
                        || state.key_pressed(egui::Key::PageDown)
                        || state.key_pressed(egui::Key::Space),
                )
            });
        }
        if self.texture.is_some() {
            #[cfg(not(target_os = "android"))]
            let (left, right) = self.show_page(
                ui,
                !busy && self.page > 0,
                !busy && self.page + 1 < total_pages,
            );
            #[cfg(target_os = "android")]
            let (left, right) = {
                let area = Vec2::new(
                    ui.available_width(),
                    (ui.available_height() - 34.0).max(0.0),
                );
                let (page, _) = ui.allocate_exact_size(area, egui::Sense::hover());
                let mut page_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("comic-canvas")
                        .max_rect(page),
                );
                page_ui.set_clip_rect(page);
                let actions = self.show_page(
                    &mut page_ui,
                    !blocked && !busy && self.page > 0,
                    !blocked && !busy && self.page + 1 < total_pages,
                    (!blocked).then_some(tap_area),
                );
                ui.centered_and_justified(|ui| {
                    let p = crate::theme::palette(ui.visuals().dark_mode);
                    ui.label(
                        egui::RichText::new(format!("{} / {total_pages}", self.page + 1))
                            .size(12.0)
                            .color(p.muted),
                    );
                });
                actions
            };
            previous |= left;
            next |= right;
        } else if busy {
            ui.centered_and_justified(|ui| {
                ui.label(tr("正在加载漫画页…", "Loading comic page…"));
            });
        }
        let target = if previous && self.page > 0 {
            self.page - 1
        } else if next && self.page + 1 < total_pages {
            self.page + 1
        } else {
            return None;
        };
        self.pending = Some(target);
        Some((self.summary.as_ref()?.id.clone(), target))
    }

    fn show_page(
        &mut self,
        ui: &mut egui::Ui,
        previous: bool,
        next: bool,
        #[cfg(target_os = "android")] tap_area: Option<Rect>,
    ) -> (bool, bool) {
        let Some(image) = &self.texture else {
            return (false, false);
        };
        let now = ui.input(|input| input.time);
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| now - transition.started >= PAGE_TRANSITION_SECONDS)
        {
            self.transition = None;
        }
        let available = ui.available_size();
        let size = fitted_size(image, available, self.fit_width);
        let output = egui::ScrollArea::vertical()
            .id_salt(("comic", self.book_id(), self.page))
            .show_viewport(ui, |ui, viewport| {
                let clicked = ui
                    .vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        if let Some(transition) = &self.transition {
                            let fraction = ((now - transition.started) / PAGE_TRANSITION_SECONDS)
                                .clamp(0.0, 1.0) as f32;
                            let motion = 1.0 - (1.0 - fraction).powi(3);
                            let distance =
                                (available.x * 0.06).clamp(20.0, 52.0) * transition.direction;
                            let old_size =
                                fitted_size(&transition.previous, available, self.fit_width);
                            let old_rect = Rect::from_min_size(
                                egui::pos2(
                                    rect.center().x - old_size.x * 0.5 - motion * distance,
                                    rect.top() + viewport.top() - transition.previous_scroll,
                                ),
                                old_size,
                            );
                            paint_page(ui, &transition.previous, old_rect, 1.0 - fraction);
                            let new_rect =
                                rect.translate(egui::vec2((1.0 - motion) * distance, 0.0));
                            paint_page(ui, image, new_rect, fraction);
                            ui.ctx().request_repaint();
                        } else {
                            paint_page(ui, image, rect, 1.0);
                        }
                        let edge = rect.width() * 0.25;
                        let left = Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.left() + edge, rect.bottom()),
                        );
                        let right = Rect::from_min_max(
                            egui::pos2(rect.right() - edge, rect.top()),
                            rect.max,
                        );
                        #[cfg(target_os = "android")]
                        let touch_clip = tap_area.unwrap_or(Rect::NOTHING);
                        #[cfg(not(target_os = "android"))]
                        let touch_clip = ui.clip_rect();
                        let click = |enabled, rect: Rect, id| {
                            enabled
                                && rect.intersects(touch_clip)
                                && ui
                                    .interact(
                                        rect.intersect(ui.clip_rect()).intersect(touch_clip),
                                        ui.id().with(id),
                                        egui::Sense::click(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                        };
                        #[cfg(target_os = "android")]
                        {
                            let center =
                                Rect::from_x_y_ranges(left.right()..=right.left(), rect.y_range());
                            self.tools_requested |=
                                click(tap_area.is_some(), center, "comic-tools");
                        }
                        (
                            click(previous, left, "comic-prev"),
                            click(next, right, "comic-next"),
                        )
                    })
                    .inner;
                (clicked, viewport.top())
            });
        let (clicked, scroll) = output.inner;
        self.scroll = scroll;
        clicked
    }
}

fn fitted_size(image: &TextureHandle, available: Vec2, fit_width: bool) -> Vec2 {
    let size = image.size_vec2();
    let width_scale = available.x / size.x;
    let scale = if fit_width {
        width_scale
    } else {
        width_scale.min(available.y / size.y)
    };
    size * scale.max(0.01)
}

fn paint_page(ui: &egui::Ui, texture: &TextureHandle, rect: Rect, opacity: f32) {
    egui::Image::new((texture.id(), rect.size()))
        .corner_radius(6)
        .tint(Color32::WHITE.gamma_multiply(opacity))
        .paint_at(ui, rect);
}
