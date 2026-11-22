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

fn sheet_rect(safe: Rect, progress: f32) -> Rect {
    let height = (safe.height() * 0.78)
        .max(392.0)
        .min((safe.height() - 12.0).max(0.0));
    let width = safe.width().min(600.0);
    let top = safe.bottom() - height * progress;
    Rect::from_min_size(
        egui::pos2(safe.center().x - width / 2.0, top),
        egui::vec2(width, height),
    )
}

pub fn sheet(
    ctx: &egui::Context,
    safe: Rect,
    progress: f32,
    interactive: bool,
    fill: Color32,
    content: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let area = egui::Area::new(Id::new("reader-tools-sheet"))
        .kind(egui::UiKind::Modal)
        .order(egui::Order::Foreground)
        .sense(Sense::hover())
        .fixed_pos(safe.min)
        .default_size(safe.size())
        .constrain_to(safe)
        .fade_in(false);
    ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    let output = area.show(ctx, |ui| {
        ui.expand_to_include_rect(safe);
        let backdrop = ui.interact(safe, ui.id().with("backdrop"), Sense::click());
        ui.painter()
            .rect_filled(safe, 0, Color32::from_black_alpha((90.0 * progress) as u8));
        let bounds = sheet_rect(safe, progress);
        let mut sheet = ui.new_child(
            UiBuilder::new()
                .id_salt("sheet")
                .max_rect(bounds)
                .sense(Sense::click()),
        );
        sheet.set_clip_rect(bounds.intersect(safe));
        sheet.set_min_size(bounds.size());
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
                color: Color32::from_black_alpha(38),
            }
            .as_shape(bounds, corner),
        );
        sheet.painter().rect_filled(bounds, corner, fill);
        let inset = bounds.shrink2(egui::vec2(16.0, 12.0));
        let mut body = sheet.new_child(UiBuilder::new().id_salt("content").max_rect(inset));
        if !interactive {
            body.visuals_mut().disabled_alpha = 1.0;
            body.disable();
        }
        let close = content(&mut body);
        interactive
            && (close
                || backdrop.clicked()
                || ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
    });
    output.inner
}

pub fn list_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    title: &str,
    ink: Color32,
    fill: Color32,
) -> egui::Response {
    let size = egui::vec2(ui.available_width(), 48.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active.bg_fill
    } else {
        fill
    };
    ui.painter().rect_filled(rect, 10, fill);
    let label = rect.shrink2(egui::vec2(14.0, 4.0));
    ui.put(
        label,
        egui::Label::new(RichText::new(title).size(15.0).color(ink))
            .truncate()
            .halign(egui::Align::Min),
    );
    response
}
