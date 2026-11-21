use crossbeam_channel::{bounded, Receiver, Sender};
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use std::{
    collections::{HashSet, VecDeque},
    thread,
};
use vm_core::BookSummary;
use vm_store::{Command, CoverSize, Store};

const CACHE_CAPACITY: usize = 40;
const IN_FLIGHT: usize = 2;
type CoverEntry = (String, Option<TextureHandle>);
type LoadCover = (String, Vec<u8>);

pub struct ShelfCovers {
    known: VecDeque<CoverEntry>,
    in_flight: HashSet<String>,
    decode: Sender<LoadCover>,
    completed: Receiver<CoverEntry>,
}

impl ShelfCovers {
    pub fn spawn(ctx: egui::Context) -> std::io::Result<Self> {
        let (decode, requests) = bounded::<LoadCover>(IN_FLIGHT);
        let (results, completed) = bounded(IN_FLIGHT);
        thread::Builder::new()
            .name("velmira-covers".into())
            .spawn(move || {
                for (id, bytes) in requests {
                    let texture = vm_media::decode_page(&bytes, 320).ok().map(|image| {
                        let color = ColorImage::from_rgba_unmultiplied(
                            [image.width as usize, image.height as usize],
                            &image.pixels,
                        );
                        ctx.load_texture("book-cover", color, TextureOptions::LINEAR)
                    });
                    if results.send((id, texture)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self {
            known: VecDeque::new(),
            in_flight: HashSet::new(),
            decode,
            completed,
        })
    }

    pub fn poll(&mut self) {
        while let Ok((id, texture)) = self.completed.try_recv() {
            self.finish(id, texture);
        }
    }

    pub fn loaded(&mut self, id: String, bytes: Option<Vec<u8>>) {
        if let Some(bytes) = bytes {
            if self.decode.try_send((id.clone(), bytes)).is_ok() {
                return;
            }
        }
        self.finish(id, None);
    }

    fn finish(&mut self, id: String, texture: Option<TextureHandle>) {
        self.in_flight.remove(&id);
        if let Some(index) = self.known.iter().position(|(known, _)| known == &id) {
            self.known.remove(index);
        }
        if self.known.len() >= CACHE_CAPACITY {
            self.known.pop_front();
        }
        self.known.push_back((id, texture));
    }

    pub fn read_failed(&mut self) {
        for id in std::mem::take(&mut self.in_flight) {
            self.finish(id, None);
        }
    }

    pub fn show_at(
        &mut self,
        ui: &mut egui::Ui,
        store: &Store,
        book: &BookSummary,
        size: egui::Vec2,
    ) -> egui::Response {
        let p = crate::theme::palette(ui.visuals().dark_mode);
        if let Some(index) = self.known.iter().position(|(id, _)| id == &book.id) {
            let entry = self.known.remove(index).expect("known cover index");
            let response = if let Some(texture) = &entry.1 {
                let response = ui.add(
                    egui::Image::new((texture.id(), size))
                        .corner_radius(10)
                        .sense(egui::Sense::click()),
                );
                ui.painter().rect_stroke(
                    response.rect,
                    10,
                    egui::Stroke::new(1.0_f32, p.line),
                    egui::StrokeKind::Middle,
                );
                response
            } else {
                self.procedural(ui, book, size)
            };
            self.known.push_back(entry);
            return response;
        }
        if book.format != vm_core::BookFormat::Txt
            && self.in_flight.len() < IN_FLIGHT
            && !self.in_flight.contains(&book.id)
            && store
                .send(Command::ReadCover {
                    book_id: book.id.clone(),
                    size: CoverSize::Shelf,
                })
                .is_ok()
        {
            self.in_flight.insert(book.id.clone());
        }
        self.procedural(ui, book, size)
    }

    fn procedural(
        &self,
        ui: &mut egui::Ui,
        book: &BookSummary,
        size: egui::Vec2,
    ) -> egui::Response {
        let night = ui.visuals().dark_mode;
        let p = crate::theme::palette(night);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let painter = ui.painter();

        let cloth_day = [
            Color32::from_rgb(122, 81, 56),
            Color32::from_rgb(53, 103, 95),
            Color32::from_rgb(111, 70, 93),
            Color32::from_rgb(53, 82, 124),
            Color32::from_rgb(128, 106, 58),
            Color32::from_rgb(88, 90, 112),
            Color32::from_rgb(146, 82, 70),
            Color32::from_rgb(72, 100, 74),
        ];
        let index = book.id.bytes().fold(0u64, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u64::from(byte))
        }) as usize
            % cloth_day.len();
        let base = if night {
            crate::theme::mix(cloth_day[index], Color32::from_rgb(16, 18, 23), 0.55)
        } else {
            cloth_day[index]
        };
        let ink = if night {
            Color32::from_rgb(238, 232, 220)
        } else {
            Color32::from_rgb(248, 243, 232)
        };

        painter.rect_filled(rect, 10.0, base);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() * 0.45)),
            10.0,
            Color32::from_white_alpha(if night { 10 } else { 22 }),
        );
        let spine = egui::Rect::from_min_size(rect.min, egui::vec2(10.0, rect.height()));
        painter.rect_filled(spine, 0.0, crate::theme::mix(base, Color32::BLACK, 0.28));
        painter.line_segment(
            [
                rect.min + egui::vec2(10.5, 0.0),
                rect.min + egui::vec2(10.5, rect.height()),
            ],
            egui::Stroke::new(
                1.0_f32,
                Color32::from_white_alpha(if night { 26 } else { 60 }),
            ),
        );
        let chars: Vec<char> = book.title.chars().take(28).collect();
        let lines: Vec<String> = chars
            .chunks(7)
            .take(4)
            .map(|chunk| chunk.iter().collect())
            .collect();
        let area = egui::Rect::from_min_max(
            rect.min + egui::vec2(14.0, 30.0),
            rect.max - egui::vec2(8.0, 26.0),
        );
        let line_h = 23.0;
        let start_y = area.center().y - (lines.len() as f32 - 1.0) * line_h * 0.5;
        for (i, line) in lines.iter().enumerate() {
            painter.text(
                egui::pos2(area.center().x, start_y + i as f32 * line_h),
                egui::Align2::CENTER_CENTER,
                line,
                egui::FontId::proportional(15.0),
                ink,
            );
        }
        let badge_text = book.format.as_str().to_ascii_uppercase();
        let badge =
            egui::Rect::from_min_size(rect.max - egui::vec2(42.0, 24.0), egui::vec2(34.0, 16.0));
        painter.rect_filled(
            badge,
            4.0,
            if night {
                Color32::from_black_alpha(120)
            } else {
                Color32::from_white_alpha(210)
            },
        );
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            badge_text,
            egui::FontId::proportional(9.0),
            if night {
                ink
            } else {
                crate::theme::mix(base, Color32::BLACK, 0.3)
            },
        );
        painter.rect_stroke(
            rect,
            10,
            egui::Stroke::new(1.0_f32, p.line),
            egui::StrokeKind::Middle,
        );
        response
    }
}
