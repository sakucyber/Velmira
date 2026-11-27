use eframe::egui::{self, Color32, Response, RichText, Ui};

use crate::theme::{Palette, Tokens};

pub fn card<R>(
    ui: &mut Ui,
    palette: Palette,
    tokens: Tokens,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0_f32, palette.line))
        .corner_radius(tokens.radius_md)
        .shadow(egui::epaint::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(tokens.card_shadow_alpha),
        })
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, add_contents)
}

pub fn primary_button(ui: &mut Ui, palette: Palette, tokens: Tokens, label: &str) -> Response {
    ui.add_sized(
        [ui.available_width(), tokens.control_height],
        egui::Button::new(RichText::new(label).strong().color(palette.on_accent))
            .fill(palette.accent)
            .stroke(egui::Stroke::NONE)
            .corner_radius(tokens.radius_sm),
    )
}

#[cfg(not(target_os = "android"))]
pub fn secondary_button(ui: &mut Ui, palette: Palette, tokens: Tokens, label: &str) -> Response {
    ui.add_sized(
        [ui.available_width(), tokens.control_height],
        egui::Button::new(RichText::new(label).color(palette.ink))
            .fill(palette.surface2)
            .stroke(egui::Stroke::new(1.0_f32, palette.line))
            .corner_radius(tokens.radius_sm),
    )
}

pub fn disclosure<R>(
    ui: &mut Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut Ui) -> R,
) {
    let id = ui.id().with(title);
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    let icon = if state.openness(ui.ctx()) > 0.5 {
        "▼"
    } else {
        "▶"
    };
    state
        .show_header(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon).size(11.0));
                ui.label(egui::RichText::new(title).strong());
            });
        })
        .body(|ui| add_contents(ui));
}

pub fn section_header(ui: &mut Ui, palette: Palette, title: &str, caption: Option<&str>) {
    ui.label(RichText::new(title).size(18.0).strong().color(palette.ink));
    if let Some(caption) = caption {
        ui.label(RichText::new(caption).size(12.0).color(palette.muted));
    }
    ui.add_space(6.0);
}

pub fn status_chip(ui: &mut Ui, palette: Palette, label: &str, healthy: bool) -> Response {
    let color = if healthy {
        palette.success
    } else {
        palette.danger
    };
    let fill = if healthy {
        palette.accent_weak
    } else {
        Color32::from_rgb(64, 35, 34)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(color))
            .fill(fill)
            .stroke(egui::Stroke::NONE)
            .corner_radius(99),
    )
}

#[cfg(not(target_os = "android"))]
pub fn neutral_chip(ui: &mut Ui, palette: Palette, label: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(82.0, 26.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 13.0, palette.surface2);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        palette.muted,
    );
}

pub fn metric_card(
    ui: &mut Ui,
    palette: Palette,
    tokens: Tokens,
    title: &str,
    value: &str,
    detail: &str,
    healthy: bool,
) -> Response {
    card(ui, palette, tokens, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(12.0).color(palette.muted));
                ui.label(RichText::new(value).size(22.0).strong().color(palette.ink));
                ui.label(RichText::new(detail).size(11.0).color(palette.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                status_chip(ui, palette, if healthy { "可用" } else { "异常" }, healthy);
            });
        });
    })
    .response
}

pub fn empty_state(ui: &mut Ui, palette: Palette, title: &str, message: &str) {
    let width = ui.available_width().min(440.0);
    let height = 176.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().rect_filled(rect, 14, palette.surface);
    ui.painter().rect_stroke(
        rect,
        14,
        egui::Stroke::new(1.0_f32, palette.line),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, -20.0),
        egui::Align2::CENTER_CENTER,
        title,
        egui::FontId::proportional(18.0),
        palette.ink,
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, 16.0),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(12.0),
        palette.muted,
    );
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Copy, Debug)]
pub struct BookCardMetrics {
    pub width: f32,
    pub height: f32,
    pub cover: egui::Vec2,
}

#[cfg(not(target_os = "android"))]
impl BookCardMetrics {
    pub fn new(width: f32) -> Self {
        let cover = egui::vec2(width, width * 1.5);
        Self {
            width,
            height: cover.y + 64.0,
            cover,
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn book_card(
    ui: &mut Ui,
    palette: Palette,
    tokens: Tokens,
    title: &str,
    meta: &str,
    progress: Option<f32>,
    cover: impl FnOnce(&mut Ui),
) -> Response {
    let metrics = BookCardMetrics::new(ui.available_width().clamp(88.0, 176.0));
    let _ = metrics.cover;
    ui.allocate_ui_with_layout(
        egui::vec2(metrics.width, metrics.height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_size(egui::vec2(metrics.width, metrics.height));
            cover(ui);
            let response = ui.add_sized(
                [metrics.width, 24.0],
                egui::Button::new(RichText::new(title).size(14.0).color(palette.ink))
                    .truncate()
                    .frame(false),
            );
            ui.add_sized(
                [metrics.width, 18.0],
                egui::Label::new(RichText::new(meta).size(11.0).color(palette.muted)).truncate(),
            );
            let (track, _) =
                ui.allocate_exact_size(egui::vec2(metrics.width, 4.0), egui::Sense::hover());
            ui.painter().rect_filled(track, 2.0, palette.line);
            if let Some(progress) = progress {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        track.min,
                        egui::vec2(track.width() * progress.clamp(0.0, 1.0), track.height()),
                    ),
                    2.0,
                    palette.accent,
                );
            }
            let _ = tokens;
            response
        },
    )
    .inner
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavAction {
    Library,
    Preferences,
    Manga,
    Models,
    Settings,
    Plugins,
    About,
}

#[cfg(not(target_os = "android"))]
fn nav_icon(ui: &mut Ui, action: NavAction, color: Color32, center: egui::Pos2) {
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.6_f32, color);
    match action {
        NavAction::Library => {
            for offset in [-7.0, 0.0, 7.0] {
                painter.line_segment(
                    [
                        center + egui::vec2(offset, -8.0),
                        center + egui::vec2(offset, 8.0),
                    ],
                    stroke,
                );
            }
        }
        NavAction::Manga => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(18.0, 16.0)),
                2,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -8.0),
                    center + egui::vec2(0.0, 8.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-9.0, 0.0),
                    center + egui::vec2(9.0, 0.0),
                ],
                stroke,
            );
        }
        NavAction::Models => {
            for offset in [-5.0, 5.0] {
                painter.rect_stroke(
                    egui::Rect::from_center_size(
                        center + egui::vec2(offset, 0.0),
                        egui::vec2(8.0, 14.0),
                    ),
                    2,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        NavAction::Preferences => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(16.0, 18.0)),
                2,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, -4.0),
                    center + egui::vec2(5.0, -4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-5.0, 2.0),
                    center + egui::vec2(5.0, 2.0),
                ],
                stroke,
            );
        }
        NavAction::Plugins => {
            for (x, y) in [(-5.0, -5.0), (5.0, -5.0), (-5.0, 5.0), (5.0, 5.0)] {
                painter.rect_stroke(
                    egui::Rect::from_center_size(center + egui::vec2(x, y), egui::vec2(7.0, 7.0)),
                    1,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        NavAction::Settings => {
            painter.circle_stroke(center, 7.0, stroke);
            painter.line_segment(
                [
                    center + egui::vec2(-4.0, 0.0),
                    center + egui::vec2(4.0, 0.0),
                ],
                stroke,
            );
        }
        NavAction::About => {
            painter.circle_stroke(center, 7.0, stroke);
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "i",
                egui::FontId::proportional(11.0),
                color,
            );
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn nav_rail(
    ui: &mut Ui,
    palette: Palette,
    tokens: Tokens,
    active: NavAction,
    chinese: bool,
) -> Option<NavAction> {
    let mut selected = None;
    ui.add_space(tokens.space_sm);
    for (action, zh, en) in [
        (NavAction::Library, "书架", "Library"),
        (NavAction::Preferences, "阅读设置", "Reading"),
        (NavAction::Manga, "漫画与画风", "Manga"),
        (NavAction::Models, "模型与连接", "Models"),
        (NavAction::Settings, "离线与同步", "Offline & sync"),
        (NavAction::Plugins, "插件", "Plugins"),
        (NavAction::About, "关于", "About"),
    ] {
        let is_active = action == active;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(168.0, 46.0), egui::Sense::click());
        if is_active || response.hovered() {
            ui.painter().rect_filled(
                rect,
                tokens.radius_md,
                if is_active {
                    palette.accent_weak
                } else {
                    palette.surface2
                },
            );
        }
        if is_active {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 10.0), egui::vec2(3.0, 26.0)),
                2,
                palette.accent,
            );
        }
        let color = if is_active {
            palette.accent
        } else {
            palette.muted
        };
        let icon_center = rect.min + egui::vec2(25.0, 23.0);
        ui.painter().circle_filled(
            icon_center,
            13.0,
            if is_active {
                palette.accent
            } else {
                palette.surface2
            },
        );
        nav_icon(
            ui,
            action,
            if is_active { palette.on_accent } else { color },
            icon_center,
        );
        ui.painter().text(
            rect.min + egui::vec2(50.0, 23.0),
            egui::Align2::LEFT_CENTER,
            if chinese { zh } else { en },
            egui::FontId::proportional(14.0),
            palette.ink,
        );
        if response.clicked() && !is_active {
            selected = Some(action);
        }
    }
    selected
}

#[cfg(not(target_os = "android"))]
pub fn validation_page(ui: &mut Ui, night: bool) {
    let palette = crate::theme::palette(night);
    let tokens = Tokens::desktop();
    section_header(
        ui,
        palette,
        "Velmira UI 验证",
        Some("Token · 固定卡片 · 状态组件"),
    );
    ui.add_space(tokens.space_xs);
    ui.horizontal_wrapped(|ui| {
        let _ = primary_button(ui, palette, tokens, "主要操作").clicked();
        let _ = secondary_button(ui, palette, tokens, "次要操作").clicked();
        status_chip(ui, palette, "模型可用", true);
    });
    ui.add_space(tokens.space_md);
    ui.horizontal_top(|ui| {
        metric_card(
            ui,
            palette,
            tokens,
            "Gemini",
            "99.2%",
            "平均 1.8 秒 · 并发 0",
            true,
        );
        metric_card(
            ui,
            palette,
            tokens,
            "MOSS",
            "冷却中",
            "最近失败：工作流节点",
            false,
        );
    });
    ui.add_space(tokens.space_lg);
    ui.horizontal_top(|ui| {
        book_card(
            ui,
            palette,
            tokens,
            "固定行高书籍",
            "EPUB · 12 章",
            Some(0.42),
            |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(120.0, 180.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 10, palette.accent_weak);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "封面",
                    egui::FontId::proportional(16.0),
                    palette.accent,
                );
            },
        );
        empty_state(
            ui,
            palette,
            "还没有书籍",
            "导入 TXT、EPUB 或 CBZ 开始阅读。",
        );
    });
    ui.add_space(tokens.space_xl);
}
