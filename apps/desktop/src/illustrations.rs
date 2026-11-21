use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use vm_store::{Command, Illustration, IllustrationImage};

#[derive(Default)]
pub struct Illustrations {
    chapter: Option<(String, u32)>,
    entries: Option<Vec<Illustration>>,
    selected: Option<u32>,
    decoded: Option<Receiver<Result<vm_media::RgbaImage, String>>>,
    texture: Option<TextureHandle>,
    error: Option<String>,
}

impl Illustrations {
    pub fn is_open(&self) -> bool {
        self.chapter.is_some()
    }

    pub fn open(&mut self, book_id: String, chapter: u32) -> Command {
        *self = Self {
            chapter: Some((book_id.clone(), chapter)),
            ..Self::default()
        };
        Command::ListIllustrations { book_id, chapter }
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    fn matches(&self, book_id: &str, chapter: u32) -> bool {
        self.chapter
            .as_ref()
            .is_some_and(|(book, index)| book == book_id && *index == chapter)
    }

    pub fn listed(&mut self, book_id: &str, chapter: u32, entries: Vec<Illustration>) {
        if self.matches(book_id, chapter) {
            self.entries = Some(entries);
        }
    }

    pub fn loaded(
        &mut self,
        ctx: &egui::Context,
        book_id: &str,
        chapter: u32,
        ordinal: u32,
        image: IllustrationImage,
    ) {
        if !self.matches(book_id, chapter) || self.selected != Some(ordinal) {
            return;
        }
        let ctx = ctx.clone();
        let (send, receive) = bounded(1);
        match std::thread::Builder::new()
            .name("velmira-epub-image".into())
            .spawn(move || {
                let result = vm_media::decode_page(&image.bytes, vm_media::MAX_OUTPUT_EDGE)
                    .map_err(|error| error.to_string());
                let _ = send.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => self.decoded = Some(receive),
            Err(error) => self.fail(error.to_string()),
        }
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Command> {
        let (book_id, chapter) = self.chapter.as_ref()?;
        if let Some(decoded) = &self.decoded {
            match decoded.try_recv() {
                Ok(result) => {
                    self.decoded = None;
                    match result {
                        Ok(image) => {
                            self.texture = Some(ctx.load_texture(
                                "epub-illustration",
                                ColorImage::from_rgba_unmultiplied(
                                    [image.width as usize, image.height as usize],
                                    &image.pixels,
                                ),
                                TextureOptions::LINEAR,
                            ));
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.decoded = None;
                    self.error = Some("插图解码已停止。".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut open = true;
        let mut request = None;
        let max = ctx.screen_rect().size() * 0.75;
        egui::Window::new("本章插图")
            .open(&mut open)
            .default_width(520.0_f32.min(max.x))
            .max_size(max)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(max.y)
                    .show(ui, |ui| {
                        if let Some(entries) = &self.entries {
                            if entries.is_empty() {
                                ui.label("本章没有已导入的插图。");
                            }
                            let pending = self.selected.is_some()
                                && self.texture.is_none()
                                && self.error.is_none();
                            ui.add_enabled_ui(!pending, |ui| {
                                for entry in entries {
                                    let label = if entry.alt.trim().is_empty() {
                                        format!("插图 {}", entry.ordinal + 1)
                                    } else {
                                        format!("插图 {} · {}", entry.ordinal + 1, entry.alt)
                                    };
                                    if ui
                                        .selectable_label(
                                            self.selected == Some(entry.ordinal),
                                            label,
                                        )
                                        .on_hover_text(format!(
                                            "正文位置：第 {} 段前",
                                            entry.before_para + 1
                                        ))
                                        .clicked()
                                    {
                                        self.selected = Some(entry.ordinal);
                                        self.error = None;
                                        self.texture = None;
                                        request = Some(Command::ReadIllustration {
                                            book_id: book_id.clone(),
                                            chapter: *chapter,
                                            ordinal: entry.ordinal,
                                        });
                                    }
                                }
                            });
                        }
                        if let Some(error) = &self.error {
                            ui.label(error);
                        } else if let Some(image) = &self.texture {
                            let size = image.size_vec2();
                            let scale = (ui.available_width() / size.x)
                                .min((max.y * 0.8) / size.y)
                                .clamp(0.0, 1.0);
                            ui.image((image.id(), size * scale));
                        } else if self.entries.is_none() || self.selected.is_some() {
                            ui.spinner();
                        }
                    });
            });
        if !open {
            self.close();
            return None;
        }
        request
    }
}
