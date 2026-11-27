use eframe::egui::{self, Color32, Rect, RichText, Sense, Stroke};
use vm_store::AnnotationColor;

pub enum Action {
    Highlight(AnnotationColor),
    Note,
    Copy,
    Translate,
    Explain,
}

fn bounds(screen: Rect, anchor: Rect, width: f32) -> Rect {
    let height = 37.0;
    let width = width.min(screen.width());
    let left = (anchor.center().x - width / 2.0).clamp(screen.left(), screen.right() - width);
    let y = if anchor.bottom() + 7.0 + height <= screen.bottom() {
        anchor.bottom() + 7.0
    } else {
        anchor.top() - height - 7.0
    };
    Rect::from_min_size(
        egui::pos2(
            left,
            y.clamp(screen.top(), (screen.bottom() - height).max(screen.top())),
        ),
        egui::vec2(width, height),
    )
}

fn show_toolbar(ui: &mut egui::Ui, chinese: bool) -> Option<Action> {
    let p = crate::theme::palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.line))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(7, 4))
        .shadow(ui.visuals().popup_shadow)
        .show(ui, |ui| {
            let mut selected = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (color, fill, zh, en) in [
                    (
                        AnnotationColor::Yellow,
                        Color32::from_rgb(250, 207, 99),
                        "黄色高亮",
                        "Yellow highlight",
                    ),
                    (
                        AnnotationColor::Green,
                        Color32::from_rgb(146, 206, 139),
                        "绿色高亮",
                        "Green highlight",
                    ),
                    (
                        AnnotationColor::Blue,
                        Color32::from_rgb(136, 196, 239),
                        "蓝色高亮",
                        "Blue highlight",
                    ),
                    (
                        AnnotationColor::Pink,
                        Color32::from_rgb(239, 149, 184),
                        "粉色高亮",
                        "Pink highlight",
                    ),
                ] {
                    let (chip, response) =
                        ui.allocate_exact_size(egui::vec2(23.0, 27.0), Sense::click());
                    let response = response.on_hover_text(if chinese { zh } else { en });
                    if response.hovered() {
                        ui.painter().circle_stroke(
                            chip.center(),
                            10.0,
                            Stroke::new(1.0_f32, p.muted),
                        );
                    }
                    ui.painter().circle_filled(chip.center(), 6.8, fill);
                    if response.clicked() {
                        selected = Some(Action::Highlight(color));
                    }
                }
                ui.separator();
                for (action, zh, en, accent) in [
                    (Action::Note, "笔记", "Note", false),
                    (Action::Copy, "复制", "Copy", false),
                    (Action::Translate, "翻译", "Translate", true),
                    (Action::Explain, "AI 解释", "AI Explain", true),
                ] {
                    let label = RichText::new(if chinese { zh } else { en })
                        .size(13.0)
                        .color(if accent { p.accent } else { p.ink });
                    if ui
                        .add_sized(
                            [if chinese { 45.0 } else { 57.0 }, 27.0],
                            egui::Button::new(label).frame(false),
                        )
                        .clicked()
                    {
                        selected = Some(action);
                    }
                }
            });
            selected
        })
        .inner
}

pub fn show(ctx: &egui::Context, anchor: Rect, chinese: bool) -> Option<Action> {
    let screen = ctx.screen_rect().shrink(8.0);
    if screen.width() < 200.0 || !anchor.intersects(screen) {
        return None;
    }
    let rect = bounds(
        screen,
        anchor.intersect(screen),
        if chinese { 316.0 } else { 370.0 },
    );
    egui::Area::new(egui::Id::new("novel-selection-bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .default_size(rect.size())
        .movable(false)
        .constrain_to(screen)
        .fade_in(false)
        .show(ctx, |ui| show_toolbar(ui, chinese))
        .inner
}
