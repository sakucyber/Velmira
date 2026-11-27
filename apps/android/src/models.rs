use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use android_activity::AndroidApp;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use vm_ai::{
    manga::Layout,
    pool::Config,
    reading::{self, Action, Language, Source, MAX_QUERY_CHARS, MAX_SOURCE_CHARS},
    route::Strategy,
    service::{legacy_config, ComicService, SessionKeys},
};
use zeroize::Zeroizing;

use crate::{
    model_settings::{self, ConnectionSettings, Profile, Settings, SharedPool, MAX_CONNECTIONS},
    saf, theme,
};

#[derive(Clone, Copy)]
enum Field {
    Endpoint,
    Model,
    Image,
    Key,
    SearchKey,
    SearchQuery,
    Prompt,
    Memory,
    MangaSource,
    ArtStyle,
    PoolConfig,
    PoolKey,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Extras {
    translation: Option<Language>,
    prompt: String,
    memory: std::collections::BTreeMap<String, String>,
    layout: Layout,
    art_style: String,
}

enum ExtensionResult {
    Search(Vec<Source>),
    Comic(tempfile::TempPath),
}

type Reply = Result<(String, String), String>;
type Task = (String, Reply);

pub struct Models {
    path: PathBuf,
    extras_path: PathBuf,
    extras: Extras,
    extension_task: Option<Receiver<Result<ExtensionResult, String>>>,
    extension_status: String,
    search_key: Zeroizing<String>,
    search_query: String,
    sources: Vec<Source>,
    manga_source: String,
    comic: Option<tempfile::TempPath>,
    memory_book: Option<String>,
    settings: Settings,
    profile: Option<Config>,
    pool_keys: SessionKeys,
    pool: SharedPool,
    keys: Vec<Zeroizing<String>>,
    selected: usize,
    editing: Option<(usize, Field)>,
    result: Option<Receiver<Task>>,
    settings_status: String,
    action: Action,
    language: Language,
    excerpt: String,
    reply: Option<Reply>,
}

impl Models {
    pub fn new(data: &Path) -> Self {
        let path = data.join("ai-model-pool.json");
        let (settings, profile, settings_status) = match model_settings::read_settings(&path) {
            Ok(Profile::Legacy(settings)) => (settings, None, String::new()),
            Ok(Profile::Pools(config)) => (Settings::default(), Some(config), String::new()),
            Err(error) => (Settings::default(), None, format!("读取配置失败：{error}")),
        };
        let keys = vec![Zeroizing::new(String::new()); settings.connections.len()];
        let extras_path = data.join("ai-reading-options.json");
        let extras = std::fs::File::open(&extras_path)
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(256 * 1024 + 1).read_to_end(&mut bytes).ok()?;
                if bytes.len() > 256 * 1024 {
                    return None;
                }
                serde_json::from_slice(&bytes).ok()
            })
            .unwrap_or_default();
        Self {
            path,
            extras_path,
            extras,
            extension_task: None,
            extension_status: String::new(),
            search_key: Zeroizing::new(String::new()),
            search_query: String::new(),
            sources: Vec::new(),
            manga_source: String::new(),
            comic: None,
            memory_book: None,
            settings,
            profile,
            pool_keys: SessionKeys::new(),
            pool: SharedPool::default(),
            keys,
            selected: 0,
            editing: None,
            result: None,
            settings_status,
            action: Action::Explain,
            language: Language::Chinese,
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

    pub fn prepare_translation(&mut self, text: String) {
        let han = text
            .chars()
            .filter(|c| matches!(c, '\u{3400}'..='\u{9fff}'))
            .count();
        self.language = self.extras.translation.unwrap_or_else(|| {
            if han > 0 && han * 100 >= text.chars().count() * 8 {
                Language::English
            } else {
                Language::Chinese
            }
        });
        self.action = Action::Translate;
        self.prepare(Some(text));
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
                        match field {
                            Field::Endpoint => self.settings.connections[index].endpoint = value,
                            Field::Model => self.settings.connections[index].text_model = value,
                            Field::Image => self.settings.connections[index].image_model = value,
                            Field::Key => self.keys[index] = Zeroizing::new(value),
                            Field::SearchKey => self.search_key = Zeroizing::new(value),
                            Field::SearchQuery => {
                                self.search_query = value;
                                self.sources.clear();
                            }
                            Field::Prompt => {
                                self.extras.prompt = value;
                                self.save_extras();
                            }
                            Field::Memory => {
                                if let Some(id) = self.memory_book.clone() {
                                    if value.is_empty() {
                                        self.extras.memory.remove(&id);
                                    } else {
                                        self.extras.memory.insert(id, value);
                                    }
                                    self.save_extras();
                                }
                            }
                            Field::MangaSource => self.manga_source = value,
                            Field::ArtStyle => {
                                self.extras.art_style = value;
                                self.save_extras();
                            }
                            Field::PoolConfig => self.apply_config(&value),
                            Field::PoolKey => {
                                if let Some(reference) = self.session_refs().get(index).cloned() {
                                    if value.is_empty() {
                                        self.pool_keys.remove(&reference);
                                    } else {
                                        self.pool_keys.insert(reference, Zeroizing::new(value));
                                    }
                                    self.settings_status = self.service().err().unwrap_or_default();
                                }
                            }
                        }
                        if matches!(
                            field,
                            Field::Endpoint | Field::Model | Field::Image | Field::Key
                        ) {
                            self.settings_status.clear();
                        }
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
                Ok((excerpt, result)) => {
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
        if let Some(task) = &self.extension_task {
            match task.try_recv() {
                Ok(result) => {
                    self.extension_task = None;
                    match result {
                        Ok(ExtensionResult::Search(sources)) => {
                            self.sources = sources;
                            if self.sources.is_empty() {
                                self.extension_status = "没有搜索结果".into();
                            }
                        }
                        Ok(ExtensionResult::Comic(path)) => {
                            self.comic = Some(path);
                        }
                        Err(error) => self.extension_status = error,
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.extension_task = None;
                    self.extension_status = "任务意外结束".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn save_extras(&mut self) {
        let result = (|| -> Result<(), String> {
            let parent = self.extras_path.parent().ok_or("设置目录无效")?;
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let bytes = serde_json::to_vec(&self.extras).map_err(|error| error.to_string())?;
            if bytes.len() > 256 * 1024 {
                return Err("设置空间已满，请清理旧记忆".into());
            }
            let mut output =
                tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
            output
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            output
                .as_file()
                .sync_all()
                .map_err(|error| error.to_string())?;
            output
                .persist(&self.extras_path)
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        self.extension_status = result.err().unwrap_or_default();
    }

    fn edit_extra(
        &mut self,
        app: &AndroidApp,
        field: Field,
        title: &str,
        value: &str,
        mode: i32,
        limit: i32,
    ) {
        if self.editing.is_none() {
            match saf::edit_field(app, title, value, mode, limit) {
                Ok(()) => self.editing = Some((self.selected, field)),
                Err(error) => self.extension_status = error,
            }
        }
    }

    pub fn translation_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("选区翻译使用模型池中的文本模型。设置默认目标语言，也可以在发送前更改。");
        let previous = self.extras.translation;
        ui.horizontal_wrapped(|ui| {
            for (language, label) in [
                (None, "自动"),
                (Some(Language::Chinese), "简体中文"),
                (Some(Language::English), "英语"),
                (Some(Language::Japanese), "日语"),
                (Some(Language::Korean), "韩语"),
            ] {
                ui.selectable_value(&mut self.extras.translation, language, label);
            }
        });
        if previous != self.extras.translation {
            self.save_extras();
        }
        if !self.extension_status.is_empty() {
            ui.label(&self.extension_status);
        }
    }

    pub fn book_context(&mut self, id: Option<&str>) {
        self.memory_book = id.map(str::to_owned);
    }

    pub fn prompt_settings(&mut self, ui: &mut egui::Ui, app: &AndroidApp, memory: bool) {
        let p = theme::palette(ui.visuals().dark_mode);
        let (field, title, value) = if memory {
            let Some(id) = &self.memory_book else {
                ui.label("打开书籍后，可以为当前书保存人物、术语或剧情记忆。");
                return;
            };
            ui.label("本书记忆保存在设备上，用于阅读 AI 的解释与总结。");
            (
                Field::Memory,
                "编辑本书记忆",
                self.extras.memory.get(id).cloned().unwrap_or_default(),
            )
        } else {
            ui.label("为阅读 AI 添加翻译术语、语气或输出格式要求。");
            (Field::Prompt, "编辑提示词", self.extras.prompt.clone())
        };
        if crate::mobile_ui::list_row(ui, title, title, p.accent, p.accent_weak).clicked() {
            self.edit_extra(app, field, title, &value, 3, 2000);
        }
        if !value.is_empty() {
            ui.label(value);
        }
        if !self.extension_status.is_empty() {
            ui.label(&self.extension_status);
        }
    }

    pub fn web_search(&mut self, ui: &mut egui::Ui, app: &AndroidApp) {
        let p = theme::palette(ui.visuals().dark_mode);
        ui.label("Brave Search");
        ui.add_enabled_ui(
            self.editing.is_none() && self.extension_task.is_none(),
            |ui| {
                if crate::mobile_ui::list_row(
                    ui,
                    "brave-key",
                    if self.search_key.is_empty() {
                        "填写 API 密钥"
                    } else {
                        "API 密钥已填写"
                    },
                    p.ink,
                    p.surface2,
                )
                .clicked()
                {
                    let value = self.search_key.clone();
                    self.edit_extra(
                        app,
                        Field::SearchKey,
                        "Brave Search API 密钥",
                        &value,
                        2,
                        1024,
                    );
                }
                if crate::mobile_ui::list_row(
                    ui,
                    "brave-query",
                    if self.search_query.is_empty() {
                        "输入搜索词"
                    } else {
                        &self.search_query
                    },
                    p.ink,
                    p.surface2,
                )
                .clicked()
                {
                    let query = self.search_query.clone();
                    self.edit_extra(
                        app,
                        Field::SearchQuery,
                        "联网搜索",
                        &query,
                        0,
                        MAX_QUERY_CHARS as i32,
                    );
                }
                let available = !self.search_key.is_empty() && !self.search_query.is_empty();
                if ui
                    .add_enabled(
                        available,
                        egui::Button::new("搜索").min_size(egui::vec2(96.0, 48.0)),
                    )
                    .clicked()
                {
                    let query = self.search_query.clone();
                    let key = self.search_key.clone();
                    self.start_extension(ui.ctx(), move || {
                        reading::search_brave(&query, &key)
                            .map(ExtensionResult::Search)
                            .map_err(|error| error.to_string())
                    });
                }
            },
        );
        for source in &self.sources {
            ui.separator();
            ui.hyperlink_to(&source.title, source.url.as_str());
            ui.label(&source.description);
        }
        self.extension_status(ui);
    }

    pub fn prepare_manga(&mut self, text: Option<String>) {
        if self.extension_task.is_none() {
            self.manga_source = text.unwrap_or_default().chars().take(4000).collect();
            self.extension_status.clear();
        }
    }

    pub fn manga_options(&self) -> (Layout, &str) {
        (self.extras.layout, &self.extras.art_style)
    }

    pub fn manga_service(&self) -> Result<Arc<ComicService>, String> {
        let service = self.service()?;
        if !service.pool().has_task("planner") || !service.pool().has_task("manga_image") {
            return Err("请在模型库配置文本模型和图像模型。".into());
        }
        Ok(service)
    }

    pub fn manga(&mut self, ui: &mut egui::Ui, app: &AndroidApp) {
        let p = theme::palette(ui.visuals().dark_mode);
        let previous = self.extras.layout;
        ui.label("拼页布局");
        ui.horizontal_wrapped(|ui| {
            for (layout, name) in [
                (Layout::Automatic, "自动"),
                (Layout::Rows, "横向分层"),
                (Layout::Columns, "双列"),
                (Layout::LeadPanel, "首格通栏"),
            ] {
                ui.selectable_value(&mut self.extras.layout, layout, name);
            }
        });
        if previous != self.extras.layout {
            self.save_extras();
        }
        let style = self.extras.art_style.clone();
        if crate::mobile_ui::list_row(
            ui,
            "manga-style",
            if style.is_empty() {
                "设置画风"
            } else {
                &style
            },
            p.ink,
            p.surface2,
        )
        .clicked()
        {
            self.edit_extra(app, Field::ArtStyle, "画风", &style, 3, 400);
        }
        ui.add_enabled_ui(
            self.editing.is_none() && self.extension_task.is_none(),
            |ui| {
                if crate::mobile_ui::list_row(ui, "manga-text", "编辑漫画原文", p.ink, p.surface2)
                    .clicked()
                {
                    let source = self.manga_source.clone();
                    self.edit_extra(app, Field::MangaSource, "漫画原文", &source, 3, 4000);
                }
                if !self.manga_source.is_empty() {
                    ui.label(self.manga_source.chars().take(160).collect::<String>());
                }
                if ui
                    .add_enabled(
                        !self.manga_source.is_empty(),
                        egui::Button::new("生成一页并导入书架").min_size(egui::vec2(160.0, 48.0)),
                    )
                    .clicked()
                {
                    let service = match self.manga_service() {
                        Ok(service) => service,
                        Err(error) => {
                            self.extension_status = error;
                            return;
                        }
                    };
                    let layout = self.extras.layout;
                    let style = self.extras.art_style.clone();
                    let source = self.manga_source.clone();
                    let folder = self
                        .path
                        .parent()
                        .expect("settings has a parent")
                        .to_owned();
                    self.start_extension(ui.ctx(), move || {
                        let board = service
                            .storyboard(&source)
                            .map_err(|error| error.to_string())?;
                        let page = service
                            .render_styled(&board, layout, &style)
                            .map_err(|error| error.to_string())?;
                        let mut output = tempfile::Builder::new()
                            .prefix("Velmira-漫画-")
                            .suffix(".cbz")
                            .tempfile_in(folder)
                            .map_err(|error| error.to_string())?;
                        page.write_cbz(output.as_file_mut())
                            .map_err(|error| error.to_string())?;
                        Ok(ExtensionResult::Comic(output.into_temp_path()))
                    });
                }
            },
        );
        self.extension_status(ui);
    }

    pub fn take_comic(&mut self) -> Option<tempfile::TempPath> {
        self.comic.take()
    }

    fn extension_status(&self, ui: &mut egui::Ui) {
        if self.extension_task.is_some() {
            ui.spinner();
        }
        if !self.extension_status.is_empty() {
            ui.label(&self.extension_status);
        }
    }

    fn start_extension(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce() -> Result<ExtensionResult, String> + Send + 'static,
    ) {
        let ctx = ctx.clone();
        let (send, receive) = bounded(1);
        match thread::Builder::new()
            .name("velmira-mobile-ai".into())
            .spawn(move || {
                let _ = send.send(task());
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.extension_status.clear();
                self.extension_task = Some(receive);
            }
            Err(error) => self.extension_status = error.to_string(),
        }
    }

    pub fn configure(&mut self, ui: &mut egui::Ui, android: &AndroidApp, library: bool) {
        let p = theme::palette(ui.visuals().dark_mode);
        let working = self.result.is_some() || self.editing.is_some();
        if !library || self.profile.is_some() {
            if ui
                .add_enabled(
                    !working,
                    egui::Button::new("编辑模型池 JSON").min_size(egui::vec2(120.0, 46.0)),
                )
                .clicked()
            {
                let value = self
                    .profile
                    .as_ref()
                    .and_then(|config| serde_json::to_string_pretty(config).ok())
                    .or_else(|| {
                        let (config, _) = legacy_config(
                            self.settings.connections(|_| String::new()),
                            self.settings.strategy,
                        );
                        config.validate().ok()?;
                        serde_json::to_string_pretty(&config).ok()
                    })
                    .unwrap_or_default();
                self.edit_extra(
                    android,
                    Field::PoolConfig,
                    "模型池 JSON",
                    &value,
                    3,
                    1_048_576,
                );
            }
            model_settings::show_status(ui, &self.pool);
        }
        if self.profile.is_some() {
            ui.add_enabled_ui(!working, |ui| {
                for (index, reference) in self.session_refs().into_iter().enumerate() {
                    if mobile_ui_key(ui, &reference, self.pool_keys.contains_key(&reference))
                        .clicked()
                    {
                        let secret = self
                            .pool_keys
                            .get(&reference)
                            .map_or("", |key| key.as_str());
                        match saf::edit_field(android, &reference, secret, 2, 2048) {
                            Ok(()) => self.editing = Some((index, Field::PoolKey)),
                            Err(error) => self.settings_status = error,
                        }
                    }
                }
                if ui.button("刷新池状态").clicked() {
                    self.settings_status = self.service().err().unwrap_or_default();
                }
            });
            if !self.settings_status.is_empty() {
                ui.label(&self.settings_status);
            }
            return;
        }
        ui.add_enabled_ui(!working, |ui| {
            if !library {
                ui.label(RichText::new("路由方式").strong().color(p.ink));

                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.settings.strategy, Strategy::Ordered, "按顺序");
                    ui.selectable_value(&mut self.settings.strategy, Strategy::RoundRobin, "轮询");
                    ui.selectable_value(&mut self.settings.strategy, Strategy::Random, "随机");
                });

                let mut move_to = None;
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(self.selected > 0, egui::Button::new("提高优先级"))
                        .clicked()
                    {
                        move_to = Some(self.selected - 1);
                    }
                    if ui
                        .add_enabled(
                            self.selected + 1 < self.settings.connections.len(),
                            egui::Button::new("降低优先级"),
                        )
                        .clicked()
                    {
                        move_to = Some(self.selected + 1);
                    }
                });
                if let Some(next) = move_to {
                    self.settings.connections.swap(self.selected, next);
                    self.keys.swap(self.selected, next);
                    self.selected = next;
                }
                let config = &mut self.settings.connections[self.selected];
                let mut concurrency = config.concurrency.unwrap_or(1);
                ui.horizontal_wrapped(|ui| {
                    ui.label("每分钟请求数");
                    ui.add(egui::DragValue::new(&mut config.rpm).range(0..=10_000));
                    ui.label("并发");
                    if ui
                        .add(egui::DragValue::new(&mut concurrency).range(1..=64))
                        .changed()
                    {
                        config.concurrency = Some(concurrency);
                    }
                });
                ui.label(
                    RichText::new("每分钟 0 次表示不限")
                        .size(12.0)
                        .color(p.muted),
                );
            } else {
                ui.label(RichText::new("服务商与模型").strong().color(p.ink));
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
                (
                    Field::Image,
                    "图像模型",
                    current.image_model.as_str(),
                    0,
                    256,
                ),
                (Field::Key, "API 密钥", key.as_str(), 2, 4096),
            ];
            let mut edit = None;
            ui.add_space(10.0);
            for (field, label, value, mode, limit) in fields {
                if !library && matches!(field, Field::Image) {
                    continue;
                }
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
                        .service()
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
                ui.selectable_value(&mut self.action, Action::Translate, "翻译");
            });
            if self.action == Action::Translate {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.language, Language::Chinese, "简体中文");
                    ui.selectable_value(&mut self.language, Language::English, "英语");
                    ui.selectable_value(&mut self.language, Language::Japanese, "日语");
                    ui.selectable_value(&mut self.language, Language::Korean, "韩语");
                });
            }
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
                            ui.label(RichText::new(&self.excerpt).color(p.ink));
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
                ui.label(RichText::new(reply).color(p.ink));
            }
            Some(Err(error)) => {
                ui.label(RichText::new(error).color(p.danger));
            }
            None => {}
        }
        settings
    }

    pub fn service(&self) -> Result<Arc<ComicService>, String> {
        match &self.profile {
            Some(config) => self.pool.configured(config, &self.pool_keys),
            None => self
                .pool
                .legacy(&self.settings, |index| self.keys[index].to_string(), false),
        }
    }

    fn session_refs(&self) -> Vec<String> {
        self.profile
            .as_ref()
            .into_iter()
            .flat_map(|config| config.providers.values())
            .flat_map(|provider| &provider.keys)
            .filter(|reference| reference.starts_with("session:"))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn apply_config(&mut self, value: &str) {
        self.settings_status =
            match model_settings::parse_config(value.as_bytes()).and_then(|config| {
                model_settings::write_config(&self.path, &config)?;
                if self.profile.as_ref() != Some(&config) {
                    self.pool_keys.clear();
                }
                self.profile = Some(config);
                self.service()?;
                Ok(())
            }) {
                Ok(()) => "已保存模型池配置。".into(),
                Err(error) => error,
            };
    }

    fn send(&mut self, ctx: &egui::Context) {
        let service = match self.service() {
            Ok(service) => service,
            Err(error) => {
                self.reply = Some(Err(error));
                return;
            }
        };
        let (send, receive) = bounded(1);
        let action = self.action;
        let language = if action == Action::Translate {
            self.language
        } else {
            Language::Chinese
        };
        let plugin_prompt = match crate::plugins::Registry::load(
            self.path.parent().unwrap_or_else(|| Path::new(".")),
        ) {
            Ok(registry) => registry.prompt_for(action),
            Err(error) => {
                self.reply = Some(Err(error));
                return;
            }
        };
        let mut prompt = plugin_prompt;
        if !self.extras.prompt.trim().is_empty() {
            if !prompt.is_empty() {
                prompt.push('\n');
            }
            prompt.push_str(self.extras.prompt.trim());
        }
        if action != Action::Translate {
            if let Some(memory) = self
                .memory_book
                .as_ref()
                .and_then(|id| self.extras.memory.get(id))
            {
                prompt.push_str("\nBook context supplied by the reader: ");
                prompt.push_str(memory);
            }
        }
        let excerpt = self.excerpt.clone();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("velmira-model-pool".into())
            .spawn(move || {
                let reply = service
                    .read_excerpt_with_prompt(action, language, &excerpt, &prompt)
                    .map_err(|error| error.to_string());
                let _ = send.send((excerpt, reply));
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

fn mobile_ui_key(ui: &mut egui::Ui, reference: &str, configured: bool) -> egui::Response {
    let p = theme::palette(ui.visuals().dark_mode);
    let label = format!(
        "{} · {}",
        reference,
        if configured {
            "已填写"
        } else {
            "填写密钥"
        }
    );
    crate::mobile_ui::list_row(ui, reference, &label, p.ink, p.surface2)
}
