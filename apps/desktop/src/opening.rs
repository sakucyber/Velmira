use eframe::egui::{self, Color32, Id, LayerId, Rect, Ui, UiBuilder, Vec2};

const DURATION: f64 = 0.34;
const RETURN_DURATION: f64 = 0.29;

pub struct Opening {
    pub book_id: String,
    pub loaded: bool,
    origin: Option<Rect>,
    start: Option<f64>,
}

impl Opening {
    pub fn new(book_id: String, origin: Option<Rect>) -> Self {
        Self {
            book_id,
            loaded: false,
            origin,
            start: None,
        }
    }

    pub fn frame(
        opening: &mut Option<Self>,
        ctx: &egui::Context,
        ready: bool,
        screen: Rect,
    ) -> Option<Reveal> {
        let open = opening.as_mut().filter(|open| open.loaded && ready)?;
        if !screen.is_positive() {
            return None;
        }
        let now = ctx.input(|input| input.time);
        let progress = ((now - *open.start.get_or_insert(now)) / DURATION) as f32;
        if progress >= 1.0 {
            *opening = None;
            return None;
        }
        ctx.request_repaint();
        let origin = open
            .origin
            .map(|rect| rect.intersect(screen))
            .filter(|rect| rect.is_positive())
            .unwrap_or_else(|| {
                Rect::from_center_size(screen.center(), screen.size().min(egui::vec2(108.0, 148.0)))
            });
        let eased = 1.0 - (1.0 - progress).powi(3);
        let rect = Rect::from_min_max(
            origin.min.lerp(screen.min, eased),
            origin.max.lerp(screen.max, eased),
        );
        Some(Reveal {
            rect,
            screen,
            eased,
            progress,
        })
    }
}

pub struct Returning {
    book_id: String,
    target: Option<Rect>,
    start: Option<f64>,
}

impl Returning {
    pub fn new(book_id: String) -> Self {
        Self {
            book_id,
            target: None,
            start: None,
        }
    }

    pub fn cover(&mut self, book_id: &str, cover: Rect) {
        if self.start.is_none() && self.book_id == book_id && cover.is_positive() {
            self.target = Some(cover);
        }
    }

    pub fn frame(&mut self, ctx: &egui::Context, screen: Rect) -> Option<Reveal> {
        if !screen.is_positive() {
            return None;
        }
        let now = ctx.input(|input| input.time);
        let elapsed = (now - *self.start.get_or_insert(now)) / RETURN_DURATION;
        if elapsed >= 1.0 {
            return None;
        }
        ctx.request_repaint();
        let progress = 1.0 - elapsed as f32;
        let eased = curve(progress, 0.0, 1.0);
        let target = self
            .target
            .map(|target| target.intersect(screen))
            .filter(|target| target.is_positive())
            .unwrap_or_else(|| screen.shrink2(screen.size() * 0.07));
        Some(Reveal {
            rect: Rect::from_min_max(
                target.min.lerp(screen.min, eased),
                target.max.lerp(screen.max, eased),
            ),
            screen,
            eased,
            progress,
        })
    }
}

pub fn scene(ctx: &egui::Context, id: Id, enabled: bool, bounds: Rect) -> Ui {
    let mut builder = UiBuilder::new().max_rect(bounds);
    if !enabled {
        let mut style = (*ctx.style()).clone();
        style.visuals.disabled_alpha = 1.0;
        builder = builder.style(style).disabled();
    }
    Ui::new(ctx.clone(), id, builder)
}

pub struct Reveal {
    rect: Rect,
    screen: Rect,
    eased: f32,
    progress: f32,
}

impl Reveal {
    pub fn paint(self, ui: &mut Ui, fill: Color32, draw: impl FnOnce(&mut Ui)) {
        let layer = LayerId::background();
        let ctx = ui.ctx().clone();
        let painter = ctx.layer_painter(layer);
        let face = curve(self.progress, 0.0, 0.24);
        let fade = 1.0 - curve(self.progress, 0.55, 1.0);
        let radius = (14.0 * (1.0 - self.eased)).round() as u8;
        painter.rect_filled(
            self.screen,
            0,
            Color32::from_black_alpha((78.0 * face * fade) as u8),
        );
        let shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha((124.0 * face * fade) as u8),
        };
        painter.add(shadow.as_shape(self.rect, radius));
        painter.rect_filled(self.rect, radius, fill.gamma_multiply(face));

        let start = ctx.graphics_mut(|graphics| graphics.entry(layer).next_idx());
        ui.set_opacity(curve(self.progress, 0.10, 0.48));
        draw(ui);

        // Lay out the ready reader at its final size. Only the painted scene
        // moves, so the opening does not repaginate or move hit targets.
        let scale = (self.rect.width() / self.screen.width())
            .max(self.rect.height() / self.screen.height());
        let transform = egui::emath::TSTransform::new(
            self.rect.center().to_vec2() - self.screen.center().to_vec2() * scale,
            scale,
        );
        let clip = self.rect.shrink2(Vec2::splat(f32::from(radius) * 0.35));
        ctx.graphics_mut(|graphics| {
            let list = graphics.entry(layer);
            let end = list.next_idx();
            list.transform_range(start, end, transform);
            for index in start.0..end.0 {
                list.mutate_shape(egui::layers::ShapeIdx(index), |shape| {
                    shape.clip_rect = shape.clip_rect.intersect(clip);
                });
            }
        });
    }
}

fn curve(value: f32, start: f32, end: f32) -> f32 {
    let t = ((value - start) / (end - start)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
