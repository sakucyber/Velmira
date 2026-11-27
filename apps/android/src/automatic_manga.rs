use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui::{self, RichText};
use vm_ai::{
    manga::{
        automatic::{chapter_excerpts, Job, Plan, Progress, Stage},
        Layout,
    },
    service::ComicService,
};

use crate::theme;

const MAX_CHAPTERS: u32 = Plan::MAX_CHAPTERS;

struct Book {
    id: String,
    title: String,
    chapters: u32,
    first: u32,
    last: u32,
    pages: u8,
}

enum Work {
    Inspect,
    Generate {
        plan: Plan,
        service: Arc<ComicService>,
    },
    Export,
    Reset,
}

#[derive(Default)]
struct State {
    plan: Option<Plan>,
    progress: Option<Progress>,
    status: String,
}

enum Update {
    Stage(Progress, u32, Stage),
    Finished(State, Option<Export>),
}

pub struct Export {
    pub file: tempfile::TempPath,
    #[cfg(not(target_os = "android"))]
    pub book: String,
}

struct Task {
    pause: Arc<AtomicBool>,
    generation: bool,
    receive: Receiver<Update>,
}

pub struct AutomaticManga {
    data: PathBuf,
    book: Option<Book>,
    state: State,
    task: Option<Task>,
    export: Option<Export>,
    confirm_reset: bool,
    #[cfg(not(target_os = "android"))]
    pub visible: bool,
}

impl AutomaticManga {
    pub fn new(data: &Path) -> Self {
        Self {
            data: data.to_owned(),
            book: None,
            state: State::default(),
            task: None,
            export: None,
            confirm_reset: false,
            #[cfg(not(target_os = "android"))]
            visible: false,
        }
    }

    pub fn open(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        title: &str,
        chapter: u32,
        chapters: u32,
    ) -> bool {
        if self.task.is_some() {
            return self.book.as_ref().is_some_and(|book| book.id == id);
        }
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || chapters == 0
        {
            return false;
        }
        if self.book.as_ref().is_some_and(|book| book.id == id) {
            self.confirm_reset = false;
            return true;
        }
        let first = chapter.min(chapters - 1);
        self.book = Some(Book {
            id: id.into(),
            title: title.into(),
            chapters,
            first,
            last: first.saturating_add(2).min(chapters - 1),
            pages: 1,
        });
        self.confirm_reset = false;
        self.state = State::default();
        self.start(ctx, Work::Inspect);
        true
    }

    pub fn poll(&mut self) {
        if let Some(task) = &self.task {
            loop {
                match task.receive.try_recv() {
                    Ok(Update::Stage(progress, chapter, stage)) => {
                        self.state.progress = Some(progress);
                        let name = match stage {
                            Stage::Reading => "正在选取原文",
                            Stage::Storyboard => "正在生成分镜",
                            Stage::Rendering => "正在生成画面",
                        };
                        self.state.status = format!("第 {} 章 · {name}", chapter + 1);
                    }
                    Ok(Update::Finished(state, file)) => {
                        self.state = state;
                        self.export = file;
                        self.task = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.task = None;
                        self.book = None;
                        self.state.status = "任务意外结束。重新打开面板以读取已保存进度。".into();
                        break;
                    }
                }
            }
        }
    }

    pub fn take_export(&mut self) -> Option<Export> {
        self.export.take()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        layout: Layout,
        style: &str,
        service: impl FnOnce() -> Result<Arc<ComicService>, String>,
    ) {
        let Some(book) = &mut self.book else {
            if self.state.status.is_empty() {
                ui.label("先打开要改编的小说。");
            } else {
                ui.label(&self.state.status);
            }
            return;
        };
        let p = theme::palette(ui.visuals().dark_mode);
        ui.label(RichText::new(&book.title).size(17.0).strong().color(p.ink));
        let mut operation = None;
        if let Some(plan) = &self.state.plan {
            ui.label(format!(
                "第 {}–{} 章 · 每章 {} 页",
                plan.first + 1,
                plan.last + 1,
                plan.pages_per_chapter
            ));
            if !plan.style.is_empty() {
                ui.label(RichText::new(&plan.style).color(p.muted));
            }
        } else {
            ui.add_enabled_ui(self.task.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("每章页数");
                    for pages in [1, 2, 3] {
                        ui.selectable_value(&mut book.pages, pages, pages.to_string());
                    }
                });
                let max_chapters = MAX_CHAPTERS / u32::from(book.pages);
                let mut first = book.first + 1;
                let mut last = book.last + 1;
                chapter_control(ui, "起始章节", &mut first, 1, book.chapters);
                last = last.clamp(
                    first,
                    first.saturating_add(max_chapters - 1).min(book.chapters),
                );
                chapter_control(
                    ui,
                    "结束章节",
                    &mut last,
                    first,
                    first.saturating_add(max_chapters - 1).min(book.chapters),
                );
                book.first = first - 1;
                book.last = last - 1;
                if ui
                    .add_sized(
                        [132.0, 46.0],
                        egui::Button::new("选满后续章节").frame(false),
                    )
                    .clicked()
                {
                    book.last =
                        (book.chapters - 1).min(book.first.saturating_add(max_chapters - 1));
                }
                ui.label(format!(
                    "将生成 {} 页，沿用漫画与画风设置。",
                    (book.last - book.first + 1) * u32::from(book.pages)
                ));
            });
        }
        if let Some(progress) = self.state.progress {
            ui.add(
                egui::ProgressBar::new(progress.done as f32 / progress.total as f32)
                    .text(format!("{} / {} 页", progress.done, progress.total)),
            );
        }
        if !self.state.status.is_empty() {
            ui.label(&self.state.status);
        }
        if let Some(task) = &self.task {
            ui.spinner();
            if task.generation {
                let pending = task.pause.load(Ordering::Relaxed);
                if ui
                    .add_enabled(
                        !pending,
                        egui::Button::new(if pending {
                            "等待当前请求结束"
                        } else {
                            "暂停"
                        })
                        .min_size(egui::vec2(100.0, 48.0)),
                    )
                    .clicked()
                {
                    task.pause.store(true, Ordering::Relaxed);
                }
            }
            return;
        }
        let complete = self
            .state
            .progress
            .is_some_and(|value| value.done == value.total);
        if complete {
            if ui
                .add(
                    egui::Button::new(RichText::new("导出 CBZ 并加入书架").color(p.on_accent))
                        .fill(p.accent)
                        .min_size(egui::vec2(ui.available_width(), 40.0)),
                )
                .clicked()
            {
                operation = Some(Work::Export);
            }
        } else {
            let start = if self.state.plan.is_some() {
                "继续自动漫画项目"
            } else {
                "开始自动漫画项目"
            };
            if ui
                .add(
                    egui::Button::new(RichText::new(start).color(p.on_accent))
                        .fill(p.accent)
                        .min_size(egui::vec2(ui.available_width(), 40.0)),
                )
                .clicked()
            {
                let plan = self.state.plan.clone().unwrap_or_else(|| Plan {
                    title: book.title.clone(),
                    first: book.first,
                    last: book.last,
                    layout,
                    style: style.to_owned(),
                    pages_per_chapter: book.pages,
                });
                match service().and_then(|service| {
                    if service.pool().has_task("planner") && service.pool().has_task("manga_image")
                    {
                        Ok(service)
                    } else {
                        Err("自动漫画项目需要配置分镜和生图模型。".into())
                    }
                }) {
                    Ok(service) => operation = Some(Work::Generate { plan, service }),
                    Err(error) => self.state.status = error,
                }
            }
        }
        if self.state.plan.is_some() {
            if self.confirm_reset {
                ui.label("清空后需要重新生成此项目的所有页。");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("确认清空").clicked() {
                        operation = Some(Work::Reset);
                        self.confirm_reset = false;
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_reset = false;
                    }
                });
            } else if ui
                .add_sized([102.0, 44.0], egui::Button::new("清空项目").frame(false))
                .clicked()
            {
                self.confirm_reset = true;
            }
        }
        if let Some(work) = operation {
            self.start(ui.ctx(), work);
        }
    }

    fn start(&mut self, ctx: &egui::Context, work: Work) {
        let Some(book) = &self.book else {
            return;
        };
        let book_id = book.id.clone();
        let data = self.data.clone();
        let folder = data.join("automatic-manga").join(&book_id);
        let pause = Arc::new(AtomicBool::new(false));
        let worker_pause = Arc::clone(&pause);
        let generation = matches!(work, Work::Generate { .. });
        let ctx = ctx.clone();
        let (send, receive) = bounded(8);
        match thread::Builder::new()
            .name("velmira-automatic-manga".into())
            .spawn(move || {
                let result = process(
                    &data,
                    &folder,
                    &book_id,
                    work,
                    &worker_pause,
                    |progress, chapter, stage| {
                        let _ = send.try_send(Update::Stage(progress, chapter, stage));
                        ctx.request_repaint();
                    },
                );
                let mut state = match inspect(&folder) {
                    Ok(state) => state,
                    Err(error) => State {
                        status: error,
                        ..State::default()
                    },
                };
                let export = match result {
                    Ok((status, file)) => {
                        if state.status.is_empty() {
                            state.status = status;
                        }
                        file.map(|file| Export {
                            file,
                            #[cfg(not(target_os = "android"))]
                            book: book_id.clone(),
                        })
                    }
                    Err(error) => {
                        state.status = error;
                        None
                    }
                };
                let _ = send.send(Update::Finished(state, export));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.task = Some(Task {
                    pause,
                    generation,
                    receive,
                })
            }
            Err(error) => self.state.status = error.to_string(),
        }
    }
}

fn chapter_control(ui: &mut egui::Ui, label: &str, value: &mut u32, min: u32, max: u32) {
    ui.label(format!("{label}  {value}"));
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                *value > min,
                egui::Button::new("−").min_size(egui::vec2(42.0, 46.0)),
            )
            .clicked()
        {
            *value -= 1;
        }
        let width = (ui.available_width() - 50.0).clamp(24.0, 440.0);
        ui.spacing_mut().slider_width = width;
        ui.add(egui::Slider::new(value, min..=max).show_value(false));
        if ui
            .add_enabled(
                *value < max,
                egui::Button::new("＋").min_size(egui::vec2(42.0, 46.0)),
            )
            .clicked()
        {
            *value += 1;
        }
    });
}

fn process(
    data: &Path,
    folder: &Path,
    book_id: &str,
    work: Work,
    pause: &AtomicBool,
    notify: impl FnMut(Progress, u32, Stage),
) -> Result<(String, Option<tempfile::TempPath>), String> {
    let file_error = |error: std::io::Error| error.to_string();
    let project_path = folder.join("pages.sqlite3");
    match work {
        Work::Inspect => Ok((String::new(), None)),
        Work::Reset => {
            let plan = load_plan(folder)?.ok_or("没有可清空的漫画项目")?;
            Job::open(&project_path, plan)
                .and_then(Job::clear)
                .map_err(|error| error.to_string())?;
            std::fs::remove_file(folder.join("plan.json")).map_err(file_error)?;
            Ok(("已清空项目".into(), None))
        }
        Work::Generate { plan, service } => {
            let plan = if let Some(existing) = load_plan(folder)? {
                existing
            } else {
                save_plan(folder, &plan)?;
                plan
            };
            let mut job = Job::open(&project_path, plan).map_err(|error| error.to_string())?;
            let library = data.join("velmira.db");
            let analysis = data.join("ai-chapter-analysis.db");
            let mut chapter_memory: Option<(u32, String)> = None;
            let progress = job.run(
                &service,
                |chapter, pages| {
                    let source = vm_store::read_chapter(&library, book_id, chapter)
                        .map_err(|error| error.to_string())?;
                    chapter_excerpts(chapter, &source.paragraphs, pages)
                },
                |chapter| {
                    if let Some((_, text)) = chapter_memory
                        .as_ref()
                        .filter(|(previous, _)| *previous == chapter)
                    {
                        return text.clone();
                    }
                    let text = vm_ai::analysis::prior_continuity(&analysis, book_id, chapter)
                        .unwrap_or_default();
                    chapter_memory = Some((chapter, text.clone()));
                    text
                },
                || pause.load(Ordering::Relaxed),
                notify,
            )?;
            Ok((
                if progress.done == progress.total {
                    "已生成全部漫画页"
                } else {
                    "已暂停，进度已保存"
                }
                .into(),
                None,
            ))
        }
        Work::Export => {
            let plan = load_plan(folder)?.ok_or("没有可导出的漫画项目")?;
            let mut job =
                Job::open(&project_path, plan.clone()).map_err(|error| error.to_string())?;
            let stem: String = plan
                .title
                .chars()
                .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
                .take(32)
                .collect();
            let prefix = format!("{stem} · 自动漫画-");
            let mut file = tempfile::Builder::new()
                .prefix(&prefix)
                .suffix(".cbz")
                .tempfile_in(data)
                .map_err(file_error)?;
            job.export(file.as_file_mut(), book_id)
                .map_err(|error| error.to_string())?;
            Ok((
                "CBZ 已生成，正在加入书架".into(),
                Some(file.into_temp_path()),
            ))
        }
    }
}

fn load_plan(folder: &Path) -> Result<Option<Plan>, String> {
    let file = match File::open(folder.join("plan.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(32 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 32 * 1024 {
        return Err("自动漫画项目配置过大".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn save_plan(folder: &Path, plan: &Plan) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(folder).map_err(|error| error.to_string())?;
    serde_json::to_writer(temp.as_file_mut(), plan).map_err(|error| error.to_string())?;
    temp.flush()
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temp.persist(folder.join("plan.json"))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn inspect(folder: &Path) -> Result<State, String> {
    let Some(plan) = load_plan(folder)? else {
        return Ok(State::default());
    };
    let job = Job::open(&folder.join("pages.sqlite3"), plan.clone())
        .map_err(|error| error.to_string())?;
    let progress = job.progress().map_err(|error| error.to_string())?;
    Ok(State {
        plan: Some(plan),
        progress: Some(progress),
        status: String::new(),
    })
}
