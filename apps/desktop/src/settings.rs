use std::path::Path;
use std::time::{Duration, Instant};

use crate::{reading_preferences, theme, ui};
use eframe::egui::{self, RichText};

#[derive(Clone, Debug)]
struct Device {
    name: String,
    status: &'static str,
    time: &'static str,
    glyph: &'static str,
    id: Option<String>,
}

pub struct PairingCode {
    width: usize,
    modules: Vec<bool>,
    expires: Instant,
}

#[derive(Default)]
pub struct SettingsState {
    pub font_size: f32,
    pub paper: Option<String>,
    pub page_style: reading_preferences::PageStyle,
    pub fit_width: bool,
    devices: Vec<Device>,
    invite: Option<PairingCode>,
}

impl SettingsState {
    pub fn new(font_size: f32, paper: Option<&str>) -> Self {
        Self {
            font_size,
            paper: paper.map(str::to_owned),
            page_style: reading_preferences::PageStyle::Page,
            fit_width: false,
            invite: None,
            devices: vec![
                Device {
                    name: "此电脑 · Windows".into(),
                    status: "已同步",
                    time: "2 分钟前",
                    glyph: "电",
                    id: None,
                },
                Device {
                    name: "手机 · Android".into(),
                    status: "未连接",
                    time: "等待配对",
                    glyph: "机",
                    id: None,
                },
                Device {
                    name: "平板 · Android".into(),
                    status: "未连接",
                    time: "等待配对",
                    glyph: "板",
                    id: None,
                },
            ],
        }
    }

    pub fn set_invitation(&mut self, payload: &str, expires: Instant) -> Result<(), String> {
        let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|error| error.to_string())?;
        self.invite = Some(PairingCode {
            width: code.width(),
            modules: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
            expires,
        });
        Ok(())
    }

    pub fn refresh_devices(&mut self, data_dir: &Path) {
        let paired = crate::pairing_cli::devices(data_dir).unwrap_or_default();
        let mut devices = vec![Device {
            name: "此电脑 · Windows".into(),
            status: "已同步",
            time: "2 分钟前",
            glyph: "电",
            id: None,
        }];
        for (index, id) in paired.into_iter().take(2).enumerate() {
            devices.push(Device {
                name: if index == 0 {
                    "手机 · Android"
                } else {
                    "平板 · Android"
                }
                .into(),
                status: "已配对",
                time: "刚刚",
                glyph: if index == 0 { "机" } else { "板" },
                id: Some(id),
            });
        }
        while devices.len() < 3 {
            let index = devices.len() - 1;
            devices.push(Device {
                name: if index == 1 {
                    "手机 · Android"
                } else {
                    "平板 · Android"
                }
                .into(),
                status: "未连接",
                time: "等待配对",
                glyph: if index == 1 { "机" } else { "板" },
                id: None,
            });
        }
        self.devices = devices;
    }
}

pub enum Action {
    Pair,
    Revoke(String),
}

fn device_row(
    ui: &mut egui::Ui,
    palette: theme::Palette,
    tokens: theme::Tokens,
    device: &Device,
) -> Option<String> {
    let mut revoke = None;
    ui::card(ui, palette, tokens, |ui| {
        ui.horizontal(|ui| {
            let (icon, _) = ui.allocate_exact_size(egui::vec2(42.0, 42.0), egui::Sense::hover());
            ui.painter().rect_filled(icon, 10, palette.surface2);
            ui.painter().text(
                icon.center(),
                egui::Align2::CENTER_CENTER,
                device.glyph,
                egui::FontId::proportional(17.0),
                palette.muted,
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(&device.name).strong().color(palette.ink));
                ui.label(RichText::new(device.time).size(11.0).color(palette.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(device.id.is_some(), egui::Button::new("撤销"))
                    .clicked()
                {
                    revoke = device.id.clone();
                }
                if device.status == "已同步" {
                    ui::status_chip(ui, palette, device.status, true);
                } else {
                    ui::neutral_chip(ui, palette, device.status);
                }
            });
        });
    });
    revoke
}

pub fn show(ui: &mut egui::Ui, state: &mut SettingsState, night: bool) -> Option<Action> {
    let palette = theme::palette(night);
    let tokens = theme::Tokens::desktop();
    if state
        .invite
        .as_ref()
        .is_some_and(|invite| invite.expires <= Instant::now())
    {
        state.invite = None;
    }
    let mut action = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui::section_header(
            ui,
            palette,
            "离线与同步",
            Some("同网设备自动发现，断网照常阅读"),
        );
        let mut revoke = None;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                for device in &state.devices {
                    if let Some(name) = device_row(ui, palette, tokens, device) {
                        revoke = Some(name);
                    }
                    ui.add_space(tokens.space_sm);
                }
            });
            ui::card(ui, palette, tokens, |ui| {
                ui.set_width(250.0);
                ui.vertical_centered(|ui| {
                    let (qr, _) =
                        ui.allocate_exact_size(egui::vec2(156.0, 156.0), egui::Sense::hover());
                    ui.painter().rect_filled(qr, 10, palette.accent_weak);
                    ui.painter().rect_stroke(
                        qr,
                        10,
                        egui::Stroke::new(1.0_f32, palette.line),
                        egui::StrokeKind::Inside,
                    );
                    if let Some(code) = state.invite.as_ref() {
                        let quiet = 4usize;
                        let total = code.width + quiet * 2;
                        let scale = qr.width() / total as f32;
                        let origin = qr.min + egui::vec2(scale, scale) * quiet as f32;
                        for y in 0..code.width {
                            for x in 0..code.width {
                                if code.modules[y * code.width + x] {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            origin + egui::vec2(x as f32, y as f32) * scale,
                                            egui::vec2(scale, scale),
                                        ),
                                        0,
                                        palette.ink,
                                    );
                                }
                            }
                        }
                        let remaining = code
                            .expires
                            .saturating_duration_since(Instant::now())
                            .as_secs();
                        ui.small(format!("剩余 {remaining} 秒"));
                        ui.ctx().request_repaint_after(Duration::from_secs(1));
                    } else {
                        ui.painter().text(
                            qr.center(),
                            egui::Align2::CENTER_CENTER,
                            "生成一次性配对二维码",
                            egui::FontId::proportional(12.0),
                            palette.muted,
                        );
                    }
                    ui.label(
                        RichText::new("连接新设备")
                            .size(17.0)
                            .strong()
                            .color(palette.accent),
                    );
                    ui.label(
                        RichText::new("二维码 5 分钟内有效，仅可使用一次")
                            .size(10.0)
                            .color(palette.muted),
                    );
                    if ui::primary_button(ui, palette, tokens, "生成配对二维码").clicked() {
                        action = Some(Action::Pair);
                    }
                });
            });
        });
        if let Some(name) = revoke {
            action = Some(Action::Revoke(name));
        }
        ui.label(
            RichText::new("局域网自动发现已开启 · 重连后自动补齐")
                .size(11.0)
                .color(palette.muted),
        );
    });
    action
}
