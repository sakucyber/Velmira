use eframe::egui::{self, Color32, Id, Rect, RichText, Sense, Ui, UiBuilder};

pub const TOP_BAR_HEIGHT: f32 = 54.0;
pub const BOTTOM_BAR_HEIGHT: f32 = 58.0;

pub fn content_rect(screen: Rect, insets: [u16; 4], pixels_per_point: f32) -> Rect {
    let [left, top, right, bottom] = insets.map(|size| f32::from(size) / pixels_per_point);
    let min = (screen.min + egui::vec2(left, top)).min(screen.max);
    let max = (screen.max - egui::vec2(right, bottom)).max(min);
    Rect::from_min_max(min, max)
}

pub fn tap_rect(bounds: Rect, controls: bool) -> Rect {
    if !controls {
        return bounds;
    }
    let top = (bounds.top() + TOP_BAR_HEIGHT).min(bounds.bottom());
    let bottom = (bounds.bottom() - BOTTOM_BAR_HEIGHT).max(top);
    Rect::from_x_y_ranges(bounds.x_range(), top..=bottom)
}

pub fn selection_menu_rect(bounds: Rect, anchor: Rect) -> Option<Rect> {
    let width = (bounds.width() - 16.0).min(280.0);
    if width <= 0.0 || bounds.height() < 70.0 {
        return None;
    }
    let left =
        (anchor.center().x - width / 2.0).clamp(bounds.left() + 8.0, bounds.right() - width - 8.0);
    let top = if anchor.bottom() + 61.0 <= bounds.bottom() {
        anchor.bottom() + 7.0
    } else {
        (anchor.top() - 61.0).max(bounds.top() + 4.0)
    };
    Some(Rect::from_min_size(
        egui::pos2(left, top),
        egui::vec2(width, 54.0),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detent {
    Peek,
    Half,
    Full,
}

impl Detent {
    fn height(self, safe: Rect) -> f32 {
        let available = (safe.height() - 12.0).max(0.0);
        match self {
            Self::Peek => (safe.height() * 0.38).min(300.0).min(available),
            Self::Half => (safe.height() * 0.62).min(520.0).min(available),
            Self::Full => available,
        }
    }

    fn raised(self) -> Self {
        match self {
            Self::Peek => Self::Half,
            Self::Half | Self::Full => Self::Full,
        }
    }

    fn lowered(self) -> Self {
        match self {
            Self::Peek => Self::Peek,
            Self::Half => Self::Peek,
            Self::Full => Self::Half,
        }
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct SheetResponse {
    pub close: bool,
    pub detent: Detent,
}

fn sheet_rect(safe: Rect, progress: f32, height: f32) -> Rect {
    let height = height.min((safe.height() - 12.0).max(0.0));
    let width = safe.width().min(600.0);
    let top = safe.bottom() - height * progress;
    Rect::from_min_size(
        egui::pos2(safe.center().x - width / 2.0, top),
        egui::vec2(width, height),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sheet(
    ctx: &egui::Context,
    safe: Rect,
    progress: f32,
    interactive: bool,
    visuals: egui::Visuals,
    tokens: crate::theme::Tokens,
    detent: Detent,
    content: impl FnOnce(&mut Ui) -> bool,
) -> SheetResponse {
    let area = egui::Area::new(Id::new("reader-tools-sheet"))
        .kind(egui::UiKind::Modal)
        .order(egui::Order::Foreground)
        .sense(Sense::hover())
        .fixed_pos(safe.min)
        .default_size(safe.size())
        .constrain_to(safe)
        .fade_in(false);
    ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    area.show(ctx, |ui| {
        ui.expand_to_include_rect(safe);
        let backdrop = ui.interact(safe, ui.id().with("backdrop"), Sense::click());
        let dark = visuals.dark_mode;
        let scrim = if dark { 112.0 } else { 90.0 };
        ui.painter()
            .rect_filled(safe, 0, Color32::from_black_alpha((scrim * progress) as u8));
        let target_height = detent.height(safe);
        let height = ctx.animate_value_with_time(
            Id::new("reader-sheet-height"),
            target_height,
            tokens.motion_panel as f32,
        );
        let bounds = sheet_rect(safe, progress, height);
        let corner = egui::CornerRadius {
            nw: 22,
            ne: 22,
            sw: 0,
            se: 0,
        };
        ui.painter_at(safe).add(
            egui::epaint::Shadow {
                offset: [0, -4],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(tokens.shadow_alpha),
            }
            .as_shape(bounds, corner),
        );
        ui.painter().rect(
            bounds,
            corner,
            visuals.window_fill,
            visuals.window_stroke,
            egui::StrokeKind::Inside,
        );

        let handle_height = tokens.touch_min.max(30.0);
        let handle_rect =
            Rect::from_min_size(bounds.min, egui::vec2(bounds.width(), handle_height));
        let handle = ui.interact(
            handle_rect,
            ui.id().with("sheet-handle"),
            Sense::click_and_drag(),
        );
        let grab = Rect::from_center_size(
            handle_rect.center() - egui::vec2(0.0, 3.0),
            egui::vec2(34.0, 4.0),
        );
        ui.painter().rect_filled(grab, 2.0, visuals.faint_bg_color);

        let inset = Rect::from_min_size(
            bounds.min + egui::vec2(16.0, handle_height + 4.0),
            bounds.size() - egui::vec2(32.0, handle_height + 16.0),
        );
        let mut body = ui.new_child(UiBuilder::new().id_salt("sheet-content").max_rect(inset));
        body.set_clip_rect(inset.intersect(bounds));
        *body.visuals_mut() = visuals;
        if !interactive {
            body.visuals_mut().disabled_alpha = 1.0;
            body.disable();
        }
        let requested_close = content(&mut body);

        let mut next = detent;
        let mut dragged_close = false;
        let drag_released = handle.drag_delta() != egui::Vec2::ZERO
            && !ui.input(|input| input.pointer.primary_down());
        if interactive && drag_released {
            let delta = handle.drag_delta().y;
            if delta > 48.0 && detent == Detent::Peek {
                dragged_close = true;
            } else if delta < -36.0 {
                next = detent.raised();
            } else if delta > 36.0 {
                next = detent.lowered();
            }
        } else if interactive && handle.double_clicked() {
            next = detent.raised();
        }
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        SheetResponse {
            close: interactive
                && (requested_close || dragged_close || backdrop.clicked() || escape),
            detent: next,
        }
    })
    .inner
}

pub fn list_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    title: &str,
    ink: Color32,
    fill: Color32,
) -> egui::Response {
    let mut button = egui::Button::new(RichText::new(title).size(15.0).color(ink))
        .truncate()
        .min_size(egui::vec2(ui.available_width(), 48.0))
        .corner_radius(10);
    if fill == Color32::TRANSPARENT {
        button = button.frame(false);
    } else {
        button = button.fill(fill).stroke(egui::Stroke::NONE);
    }
    ui.push_id(id, |ui| ui.add(button)).inner
}
