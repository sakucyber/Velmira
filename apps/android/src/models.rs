use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use android_activity::AndroidApp;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, RichText};
use vm_ai::{
    reading::{Action, Language, MAX_SOURCE_CHARS},
    route::Strategy,
    service::{ComicService, Connection},
};
use zeroize::Zeroizing;

use crate::{
    model_settings::{self, ConnectionSettings, Settings, MAX_CONNECTIONS},
    saf, theme,
};

#[derive(Clone, Copy)]
enum Field {
    Endpoint,
    Model,
    Key,
}

type Reply = Result<(String, String), String>;
type Task = (Option<Arc<ComicService>>, String, Reply);

pub struct Models {
    path: PathBuf,
    settings: Settings,
    keys: Vec<Zeroizing<String>>,
    selected: usize,
    editing: Option<(usize, Field)>,
    service: Option<Arc<ComicService>>,
    result: Option<Receiver<Task>>,
    settings_status: String,
    action: Action,
    excerpt: String,
    reply: Option<Reply>,
}

impl Models {
    pub fn new(data: &Path) -> Self {
        let path = data.join("ai-model-pool.json");
        let (settings, settings_status) = match model_settings::read_settings(&path) {
            Ok(settings) => (settings, String::new()),
            Err(error) => (Settings::default(), format!("读取配置失败：{error}")),
        };
        let keys = vec![Zeroizing::new(String::new()); settings.connections.len()];
        Self {
            path,
            settings,
            keys,
            selected: 0,
            editing: None,
            service: None,
            result: None,
            settings_status,
            action: Action::Explain,
            excerpt: String::new(),
            reply: None,
        }
    }

    pub fn prepare(&mut self, text: Option<String>) {
        let excerpt: String = text
            .unwrap_or_default()
            .trim()
            .chars()
            .take(MAX_SOURCE_CHARS)
            .collect();
        if excerpt != self.excerpt || self.result.is_none() {
            self.excerpt = excerpt;
            self.reply = None;
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, android: &AndroidApp) {
        if let Some((index, field)) = self.editing {
            match saf::take_field(android) {
                Ok(None) => ctx.request_repaint_after(Duration::from_millis(80)),
                Ok(Some(result)) => {
                    self.editing = None;
                    if let Some(mut value) = result {
                        value.truncate(value.trim_end().len());
                        let first = value.len() - value.trim_start().len();
                        value.drain(..first);
                        let connection = &mut self.settings.connections[index];
                        match field {
                            Field::Endpoint => connection.endpoint = value,
                            Field::Model => connection.text_model = value,
                            Field::Key => self.keys[index] = Zeroizing::new(value),
                        }
                        self.service = None;
                        self.settings_status.clear();
                    }
                }
                Err(error) => {
                    self.editing = None;
                    self.settings_status = error;
                }
            }
        }
        if let Some(receiver) = &self.result {
            match receiver.try_recv() {
                Ok((service, excerpt, result)) => {
                    self.service = service;
                    if excerpt == self.excerpt {
                        self.reply = Some(result);
                    }
                    self.result = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.result = None;
                    self.reply = Some(Err("模型请求意外终止。".into()));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    pub fn configure(&mut self, ui: &mut egui::Ui, android: &AndroidApp) {
        let p = theme::palette(ui.visuals().dark_mode);
        let working = self.result.is_some() || self.editing.is_some();
        ui.add_enabled_ui(!working, |ui| {
            ui.label(RichText::new("路由方式").strong().color(p.ink));
            let previous = self.settings.strategy;
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.settings.strategy, Strategy::Ordered, "按顺序");
                ui.selectable_value(&mut self.settings.strategy, Strategy::RoundRobin, "轮询");
                ui.selectable_value(&mut self.settings.strategy, Strategy::Random, "随机");
            });
            if previous != self.settings.strategy {
                self.service = None;
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for index in 0..self.settings.connections.len() {
                    ui.selectable_value(&mut self.selected, index, format!("连接 {}", index + 1));
                }
                if ui
                    .add_enabled(
                        self.settings.connections.len() < MAX_CONNECTIONS,
                        egui::Button::new("＋ 添加连接"),
                    )
                    .clicked()
                {
                    self.settings
                        .connections
                        .push(ConnectionSettings::default());
                    self.keys.push(Zeroizing::new(String::new()));
                    self.selected = self.settings.connections.len() - 1;
                    self.service = None;
                }
            });
            let current = &self.settings.connections[self.selected];
            let key = &self.keys[self.selected];
            let fields = [
                (
                    Field::Endpoint,
                    "服务地址",
                    current.endpoint.as_str(),
                    1,
                    2048,
                ),
                (
                    Field::Model,
                    "文本模型",
                    current.text_model.as_str(),
                    0,
                    256,
                ),
                (Field::Key, "API 密钥", key.as_str(), 2, 4096),
            ];
            let mut edit = None;
            ui.add_space(10.0);
            for (field, label, value, mode, limit) in fields {
                ui.label(RichText::new(label).color(p.muted).size(13.0));
                let display = if matches!(field, Field::Key) && !value.is_empty() {
                    "已填写 ······"
                } else if value.is_empty() {
                    "点按填写"
                } else {
                    value
                };
                if crate::mobile_ui::list_row(ui, label, display, p.ink, p.surface2).clicked() {
                    edit = Some((field, label, value, mode, limit));
                }
                ui.add_space(4.0);
            }
            if let Some((field, label, value, mode, limit)) = edit {
                match saf::edit_field(android, label, value, mode, limit) {
                    Ok(()) => self.editing = Some((self.selected, field)),
                    Err(error) => self.settings_status = error,
                }
            }
            ui.label(
                RichText::new("密钥只在本次运行中使用。")
                    .size(12.0)
                    .color(p.muted),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_sized(
                        [94.0, 48.0],
                        egui::Button::new(RichText::new("保存配置").color(p.on_accent))
                            .fill(p.accent),
                    )
                    .clicked()
                {
                    self.settings_status = match self
                        .connections()
                        .and_then(|_| model_settings::write_settings(&self.path, &self.settings))
                    {
                        Ok(()) => "已保存模型池。".into(),
                        Err(error) => error,
                    };
                }
                if ui
                    .add_enabled(
                        self.settings.connections.len() > 1,
                        egui::Button::new("移除此连接").min_size(egui::vec2(96.0, 48.0)),
                    )
                    .clicked()
                {
                    self.settings.connections.remove(self.selected);
                    self.keys.remove(self.selected);
                    self.selected = self.selected.min(self.settings.connections.len() - 1);
                    self.settings_status.clear();
                    self.service = None;
                }
            });
        });
        if !self.settings_status.is_empty() {
            ui.label(&self.settings_status);
        }
    }

    pub fn assistant(&mut self, ui: &mut egui::Ui) -> bool {
        let p = theme::palette(ui.visuals().dark_mode);
        if self.excerpt.is_empty() {
            ui.label("本页没有可发送的正文。");
            return false;
        }
        let working = self.result.is_some();
        let mut settings = false;
        ui.add_enabled_ui(!working, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.action, Action::Explain, "解释");
                ui.selectable_value(&mut self.action, Action::Summarize, "总结");
                ui.selectable_value(&mut self.action, Action::Translate, "译成中文");
            });
            ui.label(
                RichText::new(format!(
                    "将发送以下 {} 字到模型池",
                    self.excerpt.chars().count()
                ))
                .size(12.0)
                .color(p.muted),
            );
            egui::Frame::new()
                .fill(p.surface2)
                .corner_radius(10)
                .inner_margin(10)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ai-excerpt")
                        .max_height(112.0)
                        .show(ui, |ui| {
                            ui.label(&self.excerpt);
                        });
                });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_sized(
                        [86.0, 48.0],
                        egui::Button::new(RichText::new("发送").color(p.on_accent)).fill(p.accent),
                    )
                    .clicked()
                {
                    self.send(ui.ctx());
                }
                settings = ui
                    .add_sized([94.0, 48.0], egui::Button::new("模型池").frame(false))
                    .clicked();
            });
        });
        if working {
            ui.spinner();
        }
        match &self.reply {
            Some(Ok((model, reply))) => {
                ui.separator();
                ui.label(RichText::new(model).color(p.muted).size(12.0));
                ui.label(reply);
            }
            Some(Err(error)) => {
                ui.label(RichText::new(error).color(p.danger));
            }
            None => {}
        }
        settings
    }

    fn connections(&self) -> Result<Vec<Connection>, String> {
        self.settings
            .connections
            .iter()
            .zip(&self.keys)
            .enumerate()
            .map(|(index, (item, key))| {
                if item.endpoint.trim().is_empty() || item.text_model.trim().is_empty() {
                    return Err(format!("请填写连接 {} 的服务地址和文本模型。", index + 1));
                }
                vm_ai::validate_endpoint(&item.endpoint)
                    .map_err(|error| format!("连接 {}：{error}", index + 1))?;
                Ok(Connection {
                    endpoint: item.endpoint.clone(),
                    key: key.to_string(),
                    text_model: item.text_model.clone(),
                    image_model: String::new(),
                    rpm: item.rpm,
                    concurrency: item.concurrency.unwrap_or(1),
                })
            })
            .collect()
    }

    fn send(&mut self, ctx: &egui::Context) {
        let configuration = if let Some(service) = &self.service {
            Ok(service.clone())
        } else {
            match self.connections() {
                Ok(connections) => Err(connections),
                Err(error) => {
                    self.reply = Some(Err(error));
                    return;
                }
            }
        };
        let (send, receive) = bounded(1);
        let strategy = self.settings.strategy;
        let action = self.action;
        let excerpt = self.excerpt.clone();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("velmira-model-pool".into())
            .spawn(move || {
                let service = configuration
                    .or_else(|config| ComicService::new(config, strategy).map(Arc::new));
                let outcome = match service {
                    Ok(service) => {
                        let reply = service
                            .read_excerpt(action, Language::Chinese, &excerpt)
                            .map_err(|error| error.to_string());
                        (Some(service), reply)
                    }
                    Err(error) => (None, Err(error.to_string())),
                };
                let _ = send.send((outcome.0, excerpt, outcome.1));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.result = Some(receive);
                self.reply = None;
            }
            Err(error) => self.reply = Some(Err(error.to_string())),
        }
    }
}
