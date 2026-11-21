use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use serde::{Deserialize, Serialize};
use vm_ai::{
    route::Strategy,
    service::{ComicService, Connection},
    Storyboard,
};

use crate::ImportRequest;

const MAX_CONNECTIONS: usize = 8;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_SOURCE: usize = 12_000;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionSettings {
    endpoint: String,
    text_model: String,
    image_model: String,
    rpm: u32,
    #[serde(default)]
    concurrency: Option<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    strategy: Strategy,
    connections: Vec<ConnectionSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            strategy: Strategy::Ordered,
            connections: vec![ConnectionSettings::default()],
        }
    }
}

fn read_settings(path: &Path) -> Result<Settings, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("模型池配置文件过大。".into());
    }
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !(1..=MAX_CONNECTIONS).contains(&settings.connections.len()) {
        return Err("模型池连接数量超出范围。".into());
    }
    Ok(settings)
}

fn write_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path.parent().ok_or("配置目录无效。")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(output.as_file_mut(), settings)
        .map_err(|error| error.to_string())?;
    output
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    output.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

enum Output {
    Loaded(Settings),
    Storyboard(Storyboard),
    File(PathBuf),
    Cancelled,
}

struct Outcome {
    service: Option<Arc<ComicService>>,
    output: Result<Output, String>,
}

pub struct Generator {
    pub open: bool,
    settings_path: PathBuf,
    settings: Settings,
    loaded: bool,
    keys: Vec<String>,
    service: Option<Arc<ComicService>>,
    source: String,
    truncated: bool,
    board: Option<Storyboard>,
    status: String,
    result: Option<Receiver<Outcome>>,
}

impl Generator {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            open: false,
            settings_path: data_dir.join("ai-model-pool.json"),
            settings: Settings::default(),
            loaded: false,
            keys: vec![String::new()],
            service: None,
            source: String::new(),
            truncated: false,
            board: None,
            status: String::new(),
            result: None,
        }
    }

    pub fn prepare(&mut self, source: String) {
        self.open = true;
        if self.result.is_some() {
            return;
        }
        self.truncated = source.chars().take(MAX_SOURCE + 1).count() > MAX_SOURCE;
        self.source = source.chars().take(MAX_SOURCE).collect();
        self.board = None;
        self.status.clear();
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImportRequest> {
        if self.open && !self.loaded && self.result.is_none() {
            self.loaded = true;
            let path = self.settings_path.clone();
            self.spawn(ctx, "正在读取模型池…", move || Outcome {
                service: None,
                output: read_settings(&path).map(Output::Loaded),
            });
        }
        if let Some(result) = &self.result {
            match result.try_recv() {
                Ok(Outcome { service, output }) => {
                    self.result = None;
                    if service.is_some() {
                        self.service = service;
                    }
                    match output {
                        Ok(Output::Loaded(settings)) => {
                            self.keys = vec![String::new(); settings.connections.len()];
                            self.settings = settings;
                            self.status.clear();
                        }
                        Ok(Output::Storyboard(board)) => {
                            self.board = Some(board);
                            self.status = "分镜已生成。可修改画面描述，再生成漫画。".into();
                        }
                        Ok(Output::File(path)) => {
                            self.status = format!("已保存 CBZ：{}", path.display());
                            return Some(ImportRequest::File(path));
                        }
                        Ok(Output::Cancelled) => self.status = "已取消保存。".into(),
                        Err(error) => self.status = error,
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.result = None;
                    self.status = "任务意外结束。".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        let mut open = self.open;
        let (mut storyboard, mut render) = (false, false);
        let busy = self.result.is_some();
        egui::Window::new("小说转漫画")
            .open(&mut open)
            .resizable(true)
            .default_width(540.0)
            .max_height(ctx.screen_rect().height() * 0.85)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::CollapsingHeader::new("模型池")
                            .default_open(self.settings.connections[0].endpoint.is_empty())
                            .show(ui, |ui| self.edit_connections(ui));
                        ui.label("生成分镜会发送正文。生成漫画只发送分镜画面描述。");
                        if self.truncated {
                            ui.label("已载入选段的前 12000 字。");
                        }
                        let source_edited = ui
                            .add(
                                egui::TextEdit::multiline(&mut self.source)
                                    .char_limit(MAX_SOURCE)
                                    .desired_rows(6)
                                    .desired_width(f32::INFINITY),
                            )
                            .changed();
                        if source_edited {
                            self.board = None;
                        }
                        let connections = &self.settings.connections;
                        let configured = connections.iter().all(|connection| {
                            !connection.endpoint.trim().is_empty()
                                && (!connection.text_model.trim().is_empty()
                                    || !connection.image_model.trim().is_empty())
                        });
                        let can_send = configured
                            && !self.source.trim().is_empty()
                            && connections
                                .iter()
                                .any(|connection| !connection.text_model.trim().is_empty());
                        storyboard = ui
                            .add_enabled(can_send, egui::Button::new("生成分镜"))
                            .clicked();
                        if let Some(board) = &mut self.board {
                            ui.separator();
                            for (index, panel) in board.panels.iter_mut().enumerate() {
                                ui.label(format!("第 {} 格 · {}", index + 1, panel.source_quote));
                                ui.add(
                                    egui::TextEdit::multiline(&mut panel.visual)
                                        .char_limit(400)
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY),
                                );
                            }
                            let ready = configured
                                && connections
                                    .iter()
                                    .any(|connection| !connection.image_model.trim().is_empty())
                                && board.validate(&self.source).is_ok();
                            render = ui
                                .add_enabled(ready, egui::Button::new("选择保存位置并生成漫画"))
                                .clicked();
                        }
                    });
                    if busy {
                        ui.spinner();
                    }
                    if !self.status.is_empty() {
                        ui.label(&self.status);
                    }
                });
            });
        self.open = open;
        if storyboard || render {
            self.start(ctx, render);
        }
        None
    }

    fn edit_connections(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal(|ui| {
            for (strategy, label) in [
                (Strategy::Ordered, "按顺序"),
                (Strategy::RoundRobin, "轮询"),
                (Strategy::Random, "随机"),
            ] {
                changed |= ui
                    .selectable_value(&mut self.settings.strategy, strategy, label)
                    .changed();
            }
        });
        let mut remove = None;
        let mut reorder = None;
        let len = self.settings.connections.len();
        for (index, connection) in self.settings.connections.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("连接 {}", index + 1));
                    if ui
                        .add_enabled(index > 0, egui::Button::new("上移"))
                        .clicked()
                    {
                        reorder = Some(index);
                    }
                    if ui.add_enabled(len > 1, egui::Button::new("移除")).clicked() {
                        remove = Some(index);
                    }
                });
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut connection.endpoint)
                            .hint_text("https://api.example.com/v1")
                            .char_limit(512)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
                ui.horizontal_wrapped(|ui| {
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut connection.text_model)
                                .hint_text("文本模型")
                                .char_limit(160)
                                .desired_width(150.0),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut connection.image_model)
                                .hint_text("图像模型")
                                .char_limit(160)
                                .desired_width(150.0),
                        )
                        .changed();
                });
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.keys[index])
                            .hint_text("API 密钥（仅本次运行）")
                            .password(true)
                            .char_limit(512)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("每分钟请求上限（0 为不限）");
                    changed |= ui
                        .add(egui::DragValue::new(&mut connection.rpm).range(0..=6000))
                        .changed();
                    let mut concurrency = connection.concurrency.unwrap_or(1);
                    ui.label("并发上限");
                    if ui
                        .add(egui::DragValue::new(&mut concurrency).range(1..=16))
                        .changed()
                    {
                        connection.concurrency = Some(concurrency);
                        changed = true;
                    }
                });
            });
        }
        if let Some(index) = remove {
            self.settings.connections.remove(index);
            self.keys.remove(index);
            changed = true;
        } else if let Some(index) = reorder {
            self.settings.connections.swap(index - 1, index);
            self.keys.swap(index - 1, index);
            changed = true;
        }
        if ui
            .add_enabled(len < MAX_CONNECTIONS, egui::Button::new("添加连接"))
            .clicked()
        {
            self.settings
                .connections
                .push(ConnectionSettings::default());
            self.keys.push(String::new());
            changed = true;
        }
        if changed {
            self.service = None;
        }
    }

    fn start(&mut self, ctx: &egui::Context, render: bool) {
        let settings = self.settings.clone();
        let path = self.settings_path.clone();
        let keys = self.keys.clone();
        let mut service = self.service.clone();
        let source = self.source.clone();
        let board = if render { self.board.clone() } else { None };
        self.spawn(
            ctx,
            if render {
                "请先选择保存位置…"
            } else {
                "正在生成分镜…"
            },
            move || {
                let output = (|| {
                    if service.is_none() {
                        let connections = settings
                            .connections
                            .iter()
                            .zip(keys)
                            .map(|(connection, key)| Connection {
                                endpoint: connection.endpoint.clone(),
                                key,
                                text_model: connection.text_model.clone(),
                                image_model: connection.image_model.clone(),
                                rpm: connection.rpm,
                                concurrency: connection.concurrency.unwrap_or(1),
                            })
                            .collect();
                        service = Some(Arc::new(
                            ComicService::new(connections, settings.strategy)
                                .map_err(|error| error.to_string())?,
                        ));
                    }
                    write_settings(&path, &settings)?;
                    let service = service.as_ref().ok_or("模型池不可用。")?;
                    match board {
                        Some(board) => {
                            board.validate(&source).map_err(|error| error.to_string())?;
                            render_comic(service, &board)
                        }
                        None => service
                            .storyboard(&source)
                            .map(Output::Storyboard)
                            .map_err(|error| error.to_string()),
                    }
                })();
                Outcome { service, output }
            },
        );
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        status: &str,
        work: impl FnOnce() -> Outcome + Send + 'static,
    ) {
        let ctx = ctx.clone();
        let (send, receive) = bounded(1);
        match thread::Builder::new()
            .name("velmira-comic-generation".into())
            .spawn(move || {
                let _ = send.send(work());
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.result = Some(receive);
                self.status = status.into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }
}

fn render_comic(service: &ComicService, board: &Storyboard) -> Result<Output, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CBZ", &["cbz"])
        .set_file_name("Velmira 漫画.cbz")
        .save_file()
    else {
        return Ok(Output::Cancelled);
    };
    let path = path.with_extension("cbz");
    if path.exists() {
        return Err("保存位置已有文件，请选用其他文件名。".into());
    }
    let dir = path.parent().ok_or("无效的保存位置")?;
    let page = service.render(board).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    page.write_cbz(file.as_file_mut())
        .map_err(|error| error.to_string())?;
    file.persist_noclobber(&path)
        .map_err(|error| error.to_string())?;
    Ok(Output::File(path))
}
