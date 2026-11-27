use eframe::egui::{self, Color32, RichText};

use crate::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageStyle {
    Scroll,
    #[default]
    Page,
    Continuous,
}

pub enum Action {
    Font(f32),
    Paper(String),
    PageStyle(PageStyle),
    FitWidth(bool),
}

fn theme_card(
    ui: &mut egui::Ui,
    palette: theme::Palette,
    label: &str,
    value: &str,
    swatch: Color32,
    text: Color32,
    selected: bool,
) -> Option<String> {
    let mut chosen = None;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(92.0, 78.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 10, palette.surface);
    ui.painter().rect_stroke(
        rect,
        10,
        egui::Stroke::new(
            if selected { 2.0_f32 } else { 1.0_f32 },
            if selected {
                palette.accent
            } else {
                palette.line
            },
        ),
        egui::StrokeKind::Inside,
    );
    let preview =
        egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(76.0, 42.0));
    ui.painter().rect_filled(preview, 7, swatch);
    for (y, width) in [(17.0, 30.0), (27.0, 46.0)] {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(preview.min + egui::vec2(10.0, y), egui::vec2(width, 3.0)),
            2,
            text,
        );
    }
    ui.painter().text(
        rect.center() + egui::vec2(0.0, 29.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        palette.muted,
    );
    if response.clicked() {
        chosen = Some(value.to_owned());
    }
    chosen
}

pub fn show(
    ui: &mut egui::Ui,
    night: bool,
    chinese: bool,
    font_size: &mut f32,
    paper: &mut Option<String>,
    page_style: &mut PageStyle,
    fit_width: &mut bool,
) -> Option<Action> {
    let palette = theme::palette(night);
    let mut action = None;
    ui.horizontal(|ui| {
        for (label, value, swatch, text) in [
            (
                "浅色",
                "light",
                Color32::from_rgb(247, 244, 238),
                Color32::from_rgb(108, 102, 93),
            ),
            (
                "棕褐",
                "paper",
                Color32::from_rgb(216, 194, 164),
                Color32::from_rgb(106, 81, 55),
            ),
            (
                "深色",
                "dark",
                Color32::from_rgb(20, 22, 26),
                Color32::from_rgb(216, 213, 207),
            ),
        ] {
            if let Some(chosen) = theme_card(
                ui,
                palette,
                label,
                value,
                swatch,
                text,
                paper.as_deref() == Some(value) || (paper.is_none() && (value == "dark") == night),
            ) {
                action = Some(Action::Paper(chosen));
                break;
            }
        }
    });
    let value = if (*font_size - font_size.round()).abs() < 0.05 {
        format!("{:.0} px", font_size.round())
    } else {
        format!("{:.1} px", font_size)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("12 – 48 px").color(palette.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).color(palette.accent));
        });
    });
    ui.horizontal(|ui| {
        let mut step = ((*font_size - 12.0) * 10.0).round().clamp(0.0, 360.0) as i32;
        let width = (ui.available_width() - 104.0).max(80.0);
        let changed = ui
            .add_sized(
                [width, 34.0],
                egui::Slider::new(&mut step, 0..=360)
                    .step_by(1.0)
                    .show_value(false),
            )
            .changed();
        if changed {
            *font_size = 12.0 + step as f32 / 10.0;
            action = Some(Action::Font(*font_size));
        }
        if ui.button("恢复默认").clicked() {
            *font_size = 19.0;
            action = Some(Action::Font(19.0));
        }
    });
    ui.horizontal_wrapped(|ui| {
        for (style, label) in [
            (PageStyle::Scroll, "翻页滚动"),
            (PageStyle::Page, "翻页翻页"),
            (PageStyle::Continuous, "连续滚动"),
        ] {
            if ui.selectable_value(page_style, style, label).changed() {
                action = Some(Action::PageStyle(style));
            }
        }
    });
    ui.horizontal(|ui| {
        if ui.selectable_value(fit_width, false, "适页").changed() {
            action = Some(Action::FitWidth(false));
        }
        if ui.selectable_value(fit_width, true, "适宽").changed() {
            action = Some(Action::FitWidth(true));
        }
    });
    if chinese {
        ui.small("快捷键：← / → 翻页，空格 下一页，T 目录，F 搜索，B 书签。浮层打开时暂停。");
    }
    action
}
