use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use serde::Deserialize;
use vm_ai::{
    reading::{
        search_brave, synthesize_speech, Action, Language, Source, MAX_QUERY_CHARS,
        MAX_SOURCE_CHARS,
    },
    Provider,
};
use zeroize::Zeroizing;

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
struct ModelPool {
    connections: Vec<TextConnection>,
}

#[derive(Deserialize)]
struct TextConnection {
    endpoint: String,
    text_model: String,
}

enum Response {
    Text(String),
    Sources(Vec<Source>),
    Speech(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Excerpt,
    Search,
    Speech,
}

pub struct ReadingAi {
    settings: PathBuf,
    open: bool,
    chinese: bool,
    source: String,
    endpoint: String,
    model: String,
    speech_model: String,
    voice: String,
    key: Zeroizing<String>,
    search_key: Zeroizing<String>,
    search_query: String,
    tab: Tab,
    action: Action,
    language: Language,
    response: Option<Response>,
    status: Option<String>,
    result: Option<Receiver<Result<Response, String>>>,
}

impl ReadingAi {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            settings: data_dir.join("ai-model-pool.json"),
            open: false,
            chinese: false,
            source: String::new(),
            endpoint: String::new(),
            model: String::new(),
            speech_model: "gpt-4o-mini-tts".into(),
            voice: "alloy".into(),
            key: Zeroizing::new(String::new()),
            search_key: Zeroizing::new(String::new()),
            search_query: String::new(),
            tab: Tab::Excerpt,
            action: Action::Translate,
            language: Language::Chinese,
            response: None,
            status: None,
            result: None,
        }
    }

    pub fn prepare(&mut self, source: String, chinese: bool) {
        self.open = true;
        if self.result.is_some() {
            return;
        }
        self.chinese = chinese;
        self.status = None;
        self.response = None;
        if self.endpoint.is_empty() && self.model.is_empty() {
            if let Some(connection) = load_text_connection(&self.settings) {
                self.endpoint = connection.endpoint;
                self.model = connection.text_model;
            }
        }
        let source = source.trim();
        self.source = if source.chars().take(MAX_SOURCE_CHARS + 1).count() > MAX_SOURCE_CHARS {
            self.status = Some(
                tr(
                    chinese,
                    "请缩短选区至 4000 字以内。",
                    "Select at most 4,000 characters.",
                )
                .into(),
            );
            String::new()
        } else {
            source.to_owned()
        };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(result) = &self.result {
            match result.try_recv() {
                Ok(Ok(response)) => {
                    self.result = None;
                    self.response = Some(response);
                    self.status = None;
                }
                Ok(Err(error)) => {
                    self.result = None;
                    self.status = Some(error);
                }
                Err(TryRecvError::Disconnected) => {
                    self.result = None;
                    self.status = Some(
                        tr(
                            self.chinese,
                            "AI 请求意外结束。",
                            "The AI request ended unexpectedly.",
                        )
                        .into(),
                    );
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if !self.open {
            return;
        }
        let chinese = self.chinese;
        let mut open = true;
        let mut send = false;
        let mut save = false;
        egui::Window::new(tr(chinese, "阅读助手", "Reading assistant"))
            .id(egui::Id::new("reading-ai"))
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                let working = self.result.is_some();
                ui.add_enabled_ui(!working, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(
                            &mut self.tab,
                            Tab::Excerpt,
                            tr(chinese, "选区", "Excerpt"),
                        );
                        ui.selectable_value(
                            &mut self.tab,
                            Tab::Speech,
                            tr(chinese, "朗读", "Speech"),
                        );
                        ui.selectable_value(&mut self.tab, Tab::Search, "Brave Search");
                    });
                    if self.tab == Tab::Search {
                        ui.label(tr(chinese, "搜索词", "Search terms"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search_query)
                                .char_limit(MAX_QUERY_CHARS)
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(tr(chinese, "Brave Search API 密钥", "Brave Search API key"));
                        ui.add(
                            egui::TextEdit::singleline(&mut *self.search_key)
                                .password(true)
                                .char_limit(1024),
                        );
                        send = ui
                            .add_enabled(
                                !self.search_query.trim().is_empty()
                                    && !self.search_key.trim().is_empty(),
                                egui::Button::new(tr(chinese, "搜索", "Search")),
                            )
                            .clicked();
                        return;
                    }
                    if self.tab == Tab::Excerpt {
                        ui.horizontal_wrapped(|ui| {
                            for (action, label) in [
                                (Action::Translate, tr(chinese, "翻译", "Translate")),
                                (Action::Explain, tr(chinese, "解释", "Explain")),
                                (Action::Summarize, tr(chinese, "摘要", "Summarize")),
                            ] {
                                ui.selectable_value(&mut self.action, action, label);
                            }
                            egui::ComboBox::from_id_salt("reading-ai-language")
                                .selected_text(language_label(self.language, chinese))
                                .show_ui(ui, |ui| {
                                    for language in [
                                        Language::Chinese,
                                        Language::English,
                                        Language::Japanese,
                                        Language::Korean,
                                    ] {
                                        ui.selectable_value(
                                            &mut self.language,
                                            language,
                                            language_label(language, chinese),
                                        );
                                    }
                                });
                        });
                    }
                    ui.label(tr(chinese, "AI 服务地址", "AI service URL"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.endpoint)
                            .hint_text("https://example.com/v1")
                            .desired_width(f32::INFINITY)
                            .char_limit(1024),
                    );
                    let (has_model, send_label) = if self.tab == Tab::Speech {
                        ui.label(tr(chinese, "朗读模型", "Speech model"));
                        ui.add(egui::TextEdit::singleline(&mut self.speech_model).char_limit(256));
                        ui.label(tr(chinese, "音色", "Voice"));
                        ui.add(egui::TextEdit::singleline(&mut self.voice).char_limit(256));
                        (
                            !self.speech_model.trim().is_empty() && !self.voice.trim().is_empty(),
                            tr(chinese, "生成朗读音频", "Generate speech"),
                        )
                    } else {
                        ui.label(tr(chinese, "文本模型", "Text model"));
                        ui.add(egui::TextEdit::singleline(&mut self.model).char_limit(256));
                        (!self.model.trim().is_empty(), tr(chinese, "发送", "Send"))
                    };
                    ui.label(tr(chinese, "API 密钥", "API key"));
                    ui.add(
                        egui::TextEdit::singleline(&mut *self.key)
                            .password(true)
                            .char_limit(1024),
                    );
                    ui.label(tr(
                        chinese,
                        "点击发送后，以下文字将发送到所填服务。",
                        "Send the excerpt below to the configured service.",
                    ));
                    egui::ScrollArea::vertical()
                        .id_salt("reading-ai-source")
                        .max_height(125.0)
                        .show(ui, |ui| {
                            ui.label(&self.source);
                        });
                    send = ui
                        .add_enabled(
                            !self.source.is_empty()
                                && !self.endpoint.trim().is_empty()
                                && has_model,
                            egui::Button::new(send_label),
                        )
                        .clicked();
                });
                if working {
                    ui.spinner();
                }
                if let Some(status) = &self.status {
                    ui.label(status);
                }
                if let Some(response) = &self.response {
                    ui.separator();
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("reading-ai-result")
                        .max_height(205.0);
                    match response {
                        Response::Text(response) => {
                            scroll.show(ui, |ui| {
                                ui.label(response);
                            });
                            if ui
                                .button(tr(chinese, "复制结果", "Copy response"))
                                .clicked()
                            {
                                ui.ctx().copy_text(response.clone());
                            }
                        }
                        Response::Sources(sources) => {
                            if sources.is_empty() {
                                ui.label(tr(chinese, "没有搜索结果。", "No search results."));
                            }
                            scroll.show(ui, |ui| {
                                for source in sources {
                                    let host = source.url.host_str().unwrap_or_default();
                                    ui.hyperlink_to(
                                        if source.title.trim().is_empty() {
                                            host
                                        } else {
                                            &source.title
                                        },
                                        source.url.as_str(),
                                    );
                                    ui.small(host);
                                    ui.label(&source.description);
                                    ui.add_space(4.0);
                                }
                            });
                        }
                        Response::Speech(_) => {
                            save = ui.button(tr(chinese, "保存 WAV", "Save WAV")).clicked();
                        }
                    }
                }
            });
        self.open = open;
        if send {
            self.start(ctx);
        } else if save {
            if let Some(Response::Speech(wav)) = &self.response {
                self.status = match export_wav(wav) {
                    Ok(Some(path)) => Some(format!(
                        "{} {}",
                        tr(chinese, "音频已保存至", "Audio saved to"),
                        path.display()
                    )),
                    Ok(None) => None,
                    Err(error) => Some(error.to_string()),
                };
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let tab = self.tab;
        let key = if tab == Tab::Search {
            self.search_key.clone()
        } else {
            self.key.clone()
        };
        let (send, receive) = bounded(1);
        let (endpoint, model, text) = (
            self.endpoint.clone(),
            self.model.clone(),
            if tab == Tab::Search {
                self.search_query.clone()
            } else {
                self.source.clone()
            },
        );
        let (speech_model, voice) = (self.speech_model.clone(), self.voice.clone());
        let (action, language) = (self.action, self.language);
        match std::thread::Builder::new()
            .name("velmira-reading-ai".into())
            .spawn(move || {
                let result = (|| {
                    let timeout = Duration::from_secs(60);
                    match tab {
                        Tab::Search => search_brave(&text, &key).map(Response::Sources),
                        Tab::Speech => synthesize_speech(
                            &endpoint,
                            key.to_string(),
                            &speech_model,
                            &voice,
                            &text,
                            timeout,
                        )
                        .map(Response::Speech),
                        Tab::Excerpt => {
                            let provider = Provider::new(&endpoint, key.to_string(), &model, "")?;
                            provider
                                .read_excerpt(action, language, &text, timeout)
                                .map(Response::Text)
                        }
                    }
                })()
                .map_err(|error| error.to_string());
                let _ = send.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.result = Some(receive);
                self.status = None;
                self.response = None;
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }
}

fn export_wav(wav: &[u8]) -> std::io::Result<Option<PathBuf>> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("WAV", &["wav"])
        .set_file_name("Velmira.wav")
        .save_file()
    else {
        return Ok(None);
    };
    let path = path.with_extension("wav");
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(wav)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path)?;
    Ok(Some(path))
}

fn load_text_connection(path: &Path) -> Option<TextConnection> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return None;
    }
    let models: ModelPool = serde_json::from_slice(&bytes).ok()?;
    models.connections.into_iter().find(|connection| {
        !connection.endpoint.trim().is_empty() && !connection.text_model.trim().is_empty()
    })
}

fn tr(chinese: bool, zh: &'static str, en: &'static str) -> &'static str {
    if chinese {
        zh
    } else {
        en
    }
}

fn language_label(language: Language, chinese: bool) -> &'static str {
    match language {
        Language::Chinese => tr(chinese, "简体中文", "Simplified Chinese"),
        Language::English => tr(chinese, "英语", "English"),
        Language::Japanese => tr(chinese, "日语", "Japanese"),
        Language::Korean => tr(chinese, "韩语", "Korean"),
    }
}
