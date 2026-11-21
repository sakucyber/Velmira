use crossbeam_channel::{bounded, Receiver, Sender};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::thread;
use vm_core::{BookSummary, Loc};
use vm_store::ComicPage;

type DecodeResult = (u64, Result<vm_media::RgbaImage, String>);

pub struct Comic {
    send: Sender<(u64, ComicPage)>,
    receive: Receiver<DecodeResult>,
    summary: Option<BookSummary>,
    page: u32,
    pending: bool,
    pending_page: Option<ComicPage>,
    decoding: Option<u64>,
    sequence: u64,
    texture: Option<TextureHandle>,
    fit_width: bool,
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
            pending: false,
            pending_page: None,
            decoding: None,
            sequence: 0,
            texture: None,
            fit_width: false,
        })
    }

    pub fn book_id(&self) -> Option<&str> {
        self.summary.as_ref().map(|book| book.id.as_str())
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
        self.pending = false;
        self.texture = None;
    }

    pub fn reset_load(&mut self) {
        self.pending = false;
    }

    pub fn loaded(&mut self, book_id: &str, page: ComicPage) {
        if self.pending && self.book_id() == Some(book_id) {
            self.pending = false;
            self.pending_page = Some(page);
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Result<(String, Loc), String>> {
        let mut ready = None;
        while let Ok((id, decoded)) = self.receive.try_recv() {
            if self.decoding != Some(id) {
                continue;
            }
            self.decoding = None;
            if let Some(book) = self.summary.as_ref() {
                ready = Some(decoded.map(|image| {
                    self.texture = Some(ctx.load_texture(
                        "comic-page",
                        ColorImage::from_rgba_unmultiplied(
                            [image.width as usize, image.height as usize],
                            &image.pixels,
                        ),
                        TextureOptions::LINEAR,
                    ));
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
                    self.page = index;
                    self.decoding = Some(self.sequence);
                    self.texture = None;
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

    pub fn show(&mut self, ui: &mut egui::Ui, chinese: bool) -> Option<(String, u32)> {
        let book = self.summary.as_ref()?;
        let tr = |zh: &'static str, en: &'static str| if chinese { zh } else { en };
        ui.add(egui::Label::new(egui::RichText::new(&book.title).strong().size(20.0)).truncate());
        let busy = self.pending || self.decoding.is_some() || self.pending_page.is_some();
        let (mut previous, mut next) = (false, false);
        ui.horizontal_wrapped(|ui| {
            previous = ui
                .add_enabled(
                    !busy && self.page > 0,
                    egui::Button::new(tr("上一页", "Previous")),
                )
                .clicked();
            ui.label(format!("{} / {}", self.page + 1, book.chapter_count));
            next = ui
                .add_enabled(
                    !busy && self.page + 1 < book.chapter_count,
                    egui::Button::new(tr("下一页", "Next")),
                )
                .clicked();
            ui.separator();
            ui.selectable_value(&mut self.fit_width, false, tr("适页", "Fit page"));
            ui.selectable_value(&mut self.fit_width, true, tr("适宽", "Fit width"));
        });
        if !busy && !ui.ctx().wants_keyboard_input() {
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
        if let Some(image) = &self.texture {
            let size = image.size_vec2();
            let available = ui.available_size();
            let scale = (available.x / size.x)
                .min(if self.fit_width {
                    f32::INFINITY
                } else {
                    available.y / size.y
                })
                .max(0.01);
            egui::ScrollArea::vertical()
                .id_salt(("comic", self.page))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new((image.id(), size * scale)).corner_radius(6));
                    });
                });
        } else if busy {
            ui.centered_and_justified(|ui| {
                ui.label(tr("正在加载漫画页…", "Loading comic page…"));
            });
        }
        let target = if previous && self.page > 0 {
            self.page - 1
        } else if next && self.page + 1 < book.chapter_count {
            self.page + 1
        } else {
            return None;
        };
        self.pending = true;
        Some((book.id.clone(), target))
    }
}
