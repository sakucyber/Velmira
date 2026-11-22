use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use vm_ai::{
    manga::{Draft, Layout, Page, Project},
    route::Strategy,
    service::{ComicService, Connection},
    Storyboard,
};

use crate::{
    model_settings::{
        read_settings, write_settings, ConnectionSettings, Settings, MAX_CONNECTIONS,
    },
    ImportRequest,
};

const MAX_SOURCE: usize = 12_000;

enum Output {
    Loaded(Settings),
    Project {
        pages: Vec<Page>,
        selection: Option<i64>,
        status: String,
    },
    File(PathBuf, u32),
    Preview(vm_media::RgbaImage),
    Cancelled,
}

struct Outcome {
    service: Option<Arc<ComicService>>,
    output: Result<Output, String>,
}

enum Action {
    Open,
    Save,
    Storyboard,
    Render,
    RenderPending,
    Preview,
    Move(bool),
    Remove,
    Export,
}

struct Editor {
    page: Option<Page>,
    draft: Draft,
}

impl Editor {
    fn saved(page: Page) -> Self {
        Self {
            draft: page.draft.clone(),
            page: Some(page),
        }
    }

    fn unsaved(&self) -> bool {
        self.page
            .as_ref()
            .map_or(!self.draft.source.is_empty(), |page| {
                page.draft != self.draft
            })
    }
}

struct ReaderContext {
    book_id: String,
    chapter: u32,
    source: String,
}

pub struct Generator {
    pub open: bool,
    settings_path: PathBuf,
    project_path: PathBuf,
    settings: Settings,
    loaded: bool,
    keys: Vec<String>,
    service: Option<Arc<ComicService>>,
    book_id: String,
    reader_chapter: u32,
    reader_source: String,
    pending_reader: Option<ReaderContext>,
    pages: Vec<Page>,
    editor: Option<Editor>,
    preview: Option<egui::TextureHandle>,
    truncated: bool,
    reload_project: bool,
    confirm_remove: bool,
    status: String,
    result: Option<Receiver<Outcome>>,
    pause_render: Option<Arc<AtomicBool>>,
}

impl Generator {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            open: false,
            settings_path: data_dir.join("ai-model-pool.json"),
            project_path: data_dir.join("manga-projects.db"),
            settings: Settings::default(),
            loaded: false,
            keys: vec![String::new()],
            service: None,
            book_id: String::new(),
            reader_chapter: 0,
            reader_source: String::new(),
            pending_reader: None,
            pages: Vec::new(),
            editor: None,
            preview: None,
            truncated: false,
            reload_project: false,
            confirm_remove: false,
            status: String::new(),
            result: None,
            pause_render: None,
        }
    }

    pub fn prepare(&mut self, book_id: String, chapter: u32, source: String) {
        self.open = true;
        self.pending_reader = Some(ReaderContext {
            book_id,
            chapter,
            source,
        });
        self.apply_pending_reader();
    }

    fn apply_pending_reader(&mut self) {
        if self.result.is_some() {
            return;
        }
        let Some(ReaderContext {
            book_id,
            chapter,
            source,
        }) = self.pending_reader.take()
        else {
            return;
        };
        if self.book_id != book_id {
            if self.editor.as_ref().is_some_and(Editor::unsaved) {
                self.status = "请先保存或放弃当前项目的编辑，再打开另一本书的项目。".into();
                return;
            }
            self.book_id = book_id;
            self.pages.clear();
            self.editor = None;
            self.preview = None;
            self.reload_project = true;
            self.confirm_remove = false;
            self.status.clear();
        }
        self.reader_chapter = chapter;
        self.truncated = source.chars().take(MAX_SOURCE + 1).count() > MAX_SOURCE;
        self.reader_source = source.chars().take(MAX_SOURCE).collect();
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ImportRequest> {
        let mut import = None;
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
                    self.pause_render = None;
                    if service.is_some() {
                        self.service = service;
                    }
                    match output {
                        Ok(Output::Loaded(settings)) => {
                            self.keys = vec![String::new(); settings.connections.len()];
                            self.settings = settings;
                            self.status.clear();
                        }
                        Ok(Output::Project {
                            pages,
                            selection,
                            status,
                        }) => {
                            self.editor = selection.and_then(|id| {
                                pages
                                    .iter()
                                    .find(|page| page.id == id)
                                    .cloned()
                                    .map(Editor::saved)
                            });
                            self.pages = pages;
                            self.preview = None;
                            self.confirm_remove = false;
                            self.status = status;
                        }
                        Ok(Output::File(path, pages)) => {
                            self.status = format!("已导出 {pages} 页 CBZ：{}", path.display());
                            import = Some(ImportRequest::File(path));
                        }
                        Ok(Output::Preview(image)) => {
                            self.preview = Some(ctx.load_texture(
                                "manga-page-preview",
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width as usize, image.height as usize],
                                    &image.pixels,
                                ),
                                egui::TextureOptions::LINEAR,
                            ));
                            self.status.clear();
                        }
                        Ok(Output::Cancelled) => self.status = "已取消保存。".into(),
                        Err(error) => self.status = error,
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.result = None;
                    self.pause_render = None;
                    self.status = "任务意外结束。".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        self.apply_pending_reader();
        if import.is_some() {
            return import;
        }
        if self.open && self.loaded && self.reload_project && self.result.is_none() {
            self.reload_project = false;
            self.start(ctx, Action::Open);
        }

        let mut open = self.open;
        let mut action = None;
        let busy = self.result.is_some();
        egui::Window::new("小说转漫画")
            .open(&mut open)
            .resizable(true)
            .default_width(630.0)
            .max_height(ctx.screen_rect().height() * 0.85)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::CollapsingHeader::new("模型池")
                            .default_open(self.settings.connections[0].endpoint.is_empty())
                            .show(ui, |ui| self.edit_connections(ui));
                        action = self.show_project(ui);
                    });
                    if busy {
                        ui.spinner();
                    }
                    if self.pending_reader.is_some() {
                        ui.label("当前任务完成后会打开所选书籍或章节。");
                    }
                    if let Some(pause) = &self.pause_render {
                        let paused = pause.load(Ordering::Relaxed);
                        if ui
                            .add_enabled(!paused, egui::Button::new("当前页完成后暂停"))
                            .clicked()
                        {
                            pause.store(true, Ordering::Relaxed);
                        }
                    }
                    if !self.status.is_empty() {
                        ui.label(&self.status);
                    }
                });
            });
        self.open = open;
        if let Some(action) = action {
            self.start(ctx, action);
        }
        None
    }

    fn show_project(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let pending = self.pages.iter().filter(|page| !page.rendered).count();
        let pending_ready = self
            .pages
            .iter()
            .all(|page| page.rendered || page.draft.storyboard.is_some());
        let dirty = self.editor.as_ref().is_some_and(Editor::unsaved);
        let can_image = self.model_ready(true);
        let can_text = self.model_ready(false);
        ui.label(format!(
            "漫画项目 · {} 页 · {} 页待出图",
            self.pages.len(),
            pending
        ));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!dirty, egui::Button::new("从当前章节新建页"))
                .clicked()
            {
                self.new_page();
            }
            if ui
                .add_enabled(
                    !dirty && pending > 0 && pending_ready && can_image,
                    egui::Button::new("生成待出图页"),
                )
                .clicked()
            {
                action = Some(Action::RenderPending);
            }
            if ui
                .add_enabled(
                    !dirty && !self.pages.is_empty() && pending == 0,
                    egui::Button::new("导出整部 CBZ"),
                )
                .clicked()
            {
                action = Some(Action::Export);
            }
            if ui
                .add_enabled(!dirty, egui::Button::new("重新读取项目"))
                .clicked()
            {
                action = Some(Action::Open);
            }
        });

        let selected = self
            .editor
            .as_ref()
            .and_then(|editor| editor.page.as_ref().map(|page| page.id));
        if !self.pages.is_empty() {
            let mut picked = None;
            egui::ScrollArea::horizontal()
                .id_salt("manga-pages")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, page) in self.pages.iter().enumerate() {
                            let label = if page.rendered { "已出图" } else { "草稿" };
                            if ui
                                .add_enabled(
                                    !dirty,
                                    egui::Button::new(format!("第 {} 页 · {label}", index + 1))
                                        .selected(selected == Some(page.id)),
                                )
                                .clicked()
                            {
                                picked = Some(page.clone());
                            }
                        }
                    });
                });
            if let Some(page) = picked {
                self.editor = Some(Editor::saved(page));
                self.preview = None;
                self.confirm_remove = false;
            }
        }
        let Some(editor) = &mut self.editor else {
            return action;
        };
        ui.separator();
        ui.label(format!("原文 · 第 {} 章", editor.draft.chapter + 1));
        ui.label("生成分镜会发送正文，出图会发送分镜的画面描述。");
        if editor.page.is_none() && self.truncated {
            ui.label("已载入选段的前 12000 字。");
        }
        if ui
            .add(
                egui::TextEdit::multiline(&mut editor.draft.source)
                    .char_limit(MAX_SOURCE)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            editor.draft.storyboard = None;
        }

        egui::ComboBox::from_id_salt("manga-layout")
            .selected_text(layout_label(editor.draft.layout))
            .show_ui(ui, |ui| {
                for layout in [
                    Layout::Automatic,
                    Layout::Rows,
                    Layout::Columns,
                    Layout::LeadPanel,
                ] {
                    ui.selectable_value(&mut editor.draft.layout, layout, layout_label(layout));
                }
            });
        if let Some(board) = &mut editor.draft.storyboard {
            edit_panels(ui, board);
        }
        let unsaved = editor.unsaved();
        let has_source = !editor.draft.source.trim().is_empty();
        let valid_board = editor
            .draft
            .storyboard
            .as_ref()
            .is_some_and(|board| board.validate(&editor.draft.source).is_ok());
        let can_render = can_image && valid_board;
        let page_index = editor
            .page
            .as_ref()
            .and_then(|page| self.pages.iter().position(|item| item.id == page.id));
        let rendered = editor.page.as_ref().is_some_and(|page| page.rendered);
        let mut discard = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(has_source && unsaved, egui::Button::new("保存当前页"))
                .clicked()
            {
                action = Some(Action::Save);
            }
            if ui
                .add_enabled(can_text && has_source, egui::Button::new("生成分镜"))
                .clicked()
            {
                action = Some(Action::Storyboard);
            }
            if ui
                .add_enabled(can_render, egui::Button::new("生成当前页图像"))
                .clicked()
            {
                action = Some(Action::Render);
            }
            if ui
                .add_enabled(rendered && !unsaved, egui::Button::new("预览当前页"))
                .clicked()
            {
                action = Some(Action::Preview);
            }
            discard = ui
                .add_enabled(
                    unsaved || editor.page.is_none(),
                    egui::Button::new("放弃本次编辑"),
                )
                .clicked();
        });
        if discard {
            self.editor = editor.page.clone().map(Editor::saved);
            self.confirm_remove = false;
        }
        if let Some(preview) = self.preview.as_ref().filter(|_| !unsaved) {
            let size = preview.size_vec2();
            let scale = (ui.available_width().min(480.0) / size.x).min(1.0);
            ui.vertical_centered(|ui| ui.image((preview.id(), size * scale)));
        }
        if let Some(index) = page_index {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!unsaved && index > 0, egui::Button::new("前移此页"))
                    .clicked()
                {
                    action = Some(Action::Move(false));
                }
                if ui
                    .add_enabled(
                        !unsaved && index + 1 < self.pages.len(),
                        egui::Button::new("后移此页"),
                    )
                    .clicked()
                {
                    action = Some(Action::Move(true));
                }
                if ui
                    .add_enabled(!unsaved, egui::Button::new("删除此页"))
                    .clicked()
                {
                    self.confirm_remove = true;
                }
                if self.confirm_remove {
                    if ui
                        .add_enabled(!unsaved, egui::Button::new("确认删除"))
                        .clicked()
                    {
                        action = Some(Action::Remove);
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_remove = false;
                    }
                }
            });
        }
        action
    }

    fn model_ready(&self, image: bool) -> bool {
        let connections = &self.settings.connections;
        connections.iter().all(|connection| {
            !connection.endpoint.trim().is_empty()
                && (!connection.text_model.trim().is_empty()
                    || !connection.image_model.trim().is_empty())
        }) && connections.iter().any(|connection| {
            let model = if image {
                &connection.image_model
            } else {
                &connection.text_model
            };
            !model.trim().is_empty()
        })
    }

    fn new_page(&mut self) {
        self.editor = Some(Editor {
            page: None,
            draft: Draft {
                chapter: self.reader_chapter,
                source: self.reader_source.clone(),
                layout: Layout::Automatic,
                storyboard: None,
            },
        });
        self.preview = None;
        self.confirm_remove = false;
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

    fn start(&mut self, ctx: &egui::Context, action: Action) {
        let settings = self.settings.clone();
        let path = self.settings_path.clone();
        let keys = self.keys.clone();
        let mut service = self.service.clone();
        let project_path = self.project_path.clone();
        let book_id = self.book_id.clone();
        let saved = self.editor.as_ref().and_then(|editor| editor.page.clone());
        let edit = self.editor.as_ref().map(|editor| editor.draft.clone());
        let pause =
            matches!(action, Action::RenderPending).then(|| Arc::new(AtomicBool::new(false)));
        self.pause_render = pause.clone();
        let label = match action {
            Action::Open => "正在读取漫画项目…",
            Action::Save | Action::Move(_) | Action::Remove => "正在保存项目…",
            Action::Storyboard => "正在生成分镜…",
            Action::Render => "正在生成当前页…",
            Action::RenderPending => "正在生成待出图页…",
            Action::Preview => "正在读取预览…",
            Action::Export => "正在导出 CBZ…",
        };
        self.spawn(ctx, label, move || {
            let output = (|| {
                let needs_model = matches!(
                    action,
                    Action::Storyboard | Action::Render | Action::RenderPending
                );
                if needs_model && service.is_none() {
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
                if needs_model {
                    write_settings(&path, &settings)?;
                }
                let mut project =
                    Project::open(&project_path, &book_id).map_err(|error| error.to_string())?;
                project_action(
                    &mut project,
                    action,
                    saved,
                    edit,
                    service.as_deref(),
                    pause.as_deref(),
                )
            })();
            Outcome { service, output }
        });
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
            Err(error) => {
                self.pause_render = None;
                self.status = error.to_string();
            }
        }
    }
}

fn layout_label(layout: Layout) -> &'static str {
    match layout {
        Layout::Automatic => "自动布局",
        Layout::Rows => "横向分层",
        Layout::Columns => "双列布局",
        Layout::LeadPanel => "首格通栏",
    }
}

fn edit_panels(ui: &mut egui::Ui, board: &mut Storyboard) {
    let total = board.panels.len();
    let mut reorder = None;
    let mut remove = None;
    for (index, panel) in board.panels.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("第 {} 格", index + 1));
                if ui
                    .add_enabled(index > 0, egui::Button::new("前移"))
                    .clicked()
                {
                    reorder = Some((index, index - 1));
                }
                if ui
                    .add_enabled(index + 1 < total, egui::Button::new("后移"))
                    .clicked()
                {
                    reorder = Some((index, index + 1));
                }
                if ui
                    .add_enabled(total > 2, egui::Button::new("移除"))
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            ui.label(&panel.source_quote);
            ui.add(
                egui::TextEdit::multiline(&mut panel.visual)
                    .char_limit(400)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        });
    }
    if let Some((left, right)) = reorder {
        board.panels.swap(left, right);
    } else if let Some(index) = remove {
        board.panels.remove(index);
    }
}

fn project_action(
    project: &mut Project,
    action: Action,
    saved: Option<Page>,
    edit: Option<Draft>,
    service: Option<&ComicService>,
    pause: Option<&AtomicBool>,
) -> Result<Output, String> {
    let mut selection = saved.as_ref().map(|page| page.id);
    let status = match action {
        Action::Open => "".into(),
        Action::Export => return export_comic(project),
        Action::Preview => {
            return project
                .preview(saved.as_ref().ok_or("请先选择漫画页。")?)
                .map(Output::Preview)
                .map_err(|error| error.to_string());
        }
        Action::Save | Action::Storyboard | Action::Render => {
            let mut draft = edit.ok_or("请先选择漫画页。")?;
            if matches!(action, Action::Storyboard) {
                draft.storyboard = Some(
                    service
                        .ok_or("模型池不可用。")?
                        .storyboard(&draft.source)
                        .map_err(|error| error.to_string())?,
                );
            }
            let page = if let Some(mut page) = saved {
                page.draft = draft;
                project.save(page)
            } else {
                project.add(draft)
            }
            .map_err(|error| error.to_string())?;
            selection = Some(page.id);
            if matches!(action, Action::Render) {
                match render_page(project, service.ok_or("模型池不可用。")?, &page) {
                    Ok(()) => "当前页已生成。".into(),
                    Err(error) => format!("草稿已保存，出图未完成：{error}"),
                }
            } else if matches!(action, Action::Storyboard) {
                "分镜草稿已保存。可编辑画面描述和选择布局，再生成图像。".into()
            } else {
                "漫画页已保存。".into()
            }
        }
        Action::RenderPending => {
            let service = service.ok_or("模型池不可用。")?;
            let mut rendered = 0;
            let mut error = None;
            for page in project
                .pages()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|page| !page.rendered)
            {
                if pause.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    break;
                }
                if let Err(failure) = render_page(project, service, &page) {
                    error = Some(failure);
                    break;
                }
                rendered += 1;
            }
            if let Some(error) = error {
                format!("本次生成 {rendered} 页，已保存进度。其余页未生成：{error}")
            } else if pause.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                format!("已暂停，保存了本次生成的 {rendered} 页。")
            } else {
                format!("已生成 {rendered} 页。")
            }
        }
        Action::Move(later) => {
            project
                .move_page(saved.as_ref().ok_or("请先保存漫画页。")?, later)
                .map_err(|error| error.to_string())?;
            "页序已保存。".into()
        }
        Action::Remove => {
            project
                .remove(saved.as_ref().ok_or("请先选择漫画页。")?)
                .map_err(|error| error.to_string())?;
            selection = None;
            "漫画页已移除。".into()
        }
    };
    let pages = project.pages().map_err(|error| error.to_string())?;
    if selection.is_none() || matches!(action, Action::Open) {
        selection = selection
            .filter(|id| pages.iter().any(|page| page.id == *id))
            .or_else(|| pages.first().map(|page| page.id));
    }
    Ok(Output::Project {
        pages,
        selection,
        status,
    })
}

fn render_page(project: &mut Project, service: &ComicService, page: &Page) -> Result<(), String> {
    let board = page
        .draft
        .storyboard
        .as_ref()
        .ok_or("请先为当前页生成分镜。")?;
    board
        .validate(&page.draft.source)
        .map_err(|error| error.to_string())?;
    let rendered = service
        .render_layout(board, page.draft.layout)
        .map_err(|error| error.to_string())?;
    project
        .store_render(page, &rendered)
        .map_err(|error| error.to_string())
}

fn export_comic(project: &mut Project) -> Result<Output, String> {
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
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    let pages = project
        .export_cbz(file.as_file_mut())
        .map_err(|error| error.to_string())?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist_noclobber(&path)
        .map_err(|error| error.to_string())?;
    Ok(Output::File(path, pages))
}
