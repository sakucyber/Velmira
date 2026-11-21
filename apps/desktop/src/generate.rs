use std::{path::PathBuf, thread};

use crossbeam_channel::{bounded, Receiver};
use eframe::egui;

use crate::ImportRequest;

#[derive(Default)]
pub struct Generator {
    pub open: bool,
    endpoint: String,
    text_model: String,
    image_model: String,
    key: String,
    source: String,
    selection: usize,
    status: String,
    result: Option<Receiver<Result<Option<PathBuf>, String>>>,
}

impl Generator {
    pub fn prepare(&mut self, source: String) {
        if self.result.is_some() {
            self.open = true;
            return;
        }
        self.selection = source.chars().take(12_001).count();
        self.source = source.chars().take(12_000).collect();
        self.open = true;
        self.status.clear();
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImportRequest> {
        if let Some(result) = &self.result {
            match result.try_recv() {
                Ok(result) => {
                    self.result = None;
                    match result {
                        Ok(Some(path)) => {
                            self.status = format!("已保存 CBZ：{}", path.display());
                            return Some(ImportRequest::File(path));
                        }
                        Ok(None) => self.status = "已取消保存。".into(),
                        Err(error) => self.status = error,
                    }
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.result = None;
                    self.status = "漫画生成任务意外结束。".into();
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
        let mut open = self.open;
        let mut generate = false;
        egui::Window::new("小说转漫画")
            .open(&mut open)
            .resizable(true)
            .default_width(500.0)
            .show(ctx, |ui| {
                ui.label("已按当前章节挑选片段，可编辑。确认后将正文发送给所填服务，生成一页漫画并保存为 CBZ。");
                if self.selection > 12_000 {
                    ui.label("选取内容超过上限，已载入前 12000 字。");
                }
                ui.add_enabled_ui(self.result.is_none(), |ui| {
                    ui.label("OpenAI 兼容接口地址");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.endpoint)
                            .hint_text("https://api.example.com/v1")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.text_model).hint_text("文本模型"),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.image_model).hint_text("图像模型"),
                        );
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.key)
                            .hint_text("API 密钥（仅本次运行保留）")
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut self.source)
                            .char_limit(12_000)
                            .desired_rows(8)
                            .desired_width(f32::INFINITY),
                    );
                    let can_generate = !self.source.trim().is_empty()
                        && !self.endpoint.trim().is_empty()
                        && !self.text_model.trim().is_empty()
                        && !self.image_model.trim().is_empty();
                    generate = ui
                        .add_enabled(can_generate, egui::Button::new("选择保存位置并生成"))
                        .clicked();
                });
                if self.result.is_some() {
                    ui.spinner();
                }
                if !self.status.is_empty() {
                    ui.label(&self.status);
                }
            });
        self.open = open;
        if generate {
            self.start(ctx);
        }
        None
    }

    fn start(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let (send, receive) = bounded(1);
        let endpoint = self.endpoint.clone();
        let text_model = self.text_model.clone();
        let image_model = self.image_model.clone();
        let key = self.key.clone();
        let source = self.source.clone();
        match thread::Builder::new()
            .name("velmira-comic-generation".into())
            .spawn(move || {
                let output = (|| {
                    let provider = vm_ai::Provider::new(&endpoint, key, &text_model, &image_model)
                        .map_err(|error| error.to_string())?;
                    let Some(path) = rfd::FileDialog::new()
                        .add_filter("CBZ", &["cbz"])
                        .set_file_name("Velmira 漫画.cbz")
                        .save_file()
                    else {
                        return Ok(None);
                    };
                    let path = path.with_extension("cbz");
                    if path.exists() {
                        return Err("保存位置已有文件，请选用其他文件名。".into());
                    }
                    let dir = path.parent().ok_or("无效的保存位置")?;
                    let page = provider
                        .generate(&source)
                        .map_err(|error| error.to_string())?;
                    let mut file =
                        tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
                    page.write_cbz(file.as_file_mut())
                        .map_err(|error| error.to_string())?;
                    file.persist_noclobber(&path)
                        .map_err(|error| error.to_string())?;
                    Ok(Some(path))
                })();
                let _ = send.send(output);
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.result = Some(receive);
                self.status = "请先选择保存位置。发送请求后，生成可能需要几分钟。".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }
}
