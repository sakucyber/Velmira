use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use vm_ai::{analysis::ChapterAnalysis, service::ComicService};
use vm_core::Loc;

#[derive(Clone, PartialEq)]
struct Chapter {
    book: String,
    index: u32,
}

enum Request {
    Analyze,
    #[cfg(not(target_os = "android"))]
    Ask(String),
}

enum Outcome {
    Analysis(ChapterAnalysis),
    #[cfg(not(target_os = "android"))]
    Answer(String),
}

pub struct Analysis {
    library: PathBuf,
    cache: PathBuf,
    chapter: Option<Chapter>,
    title: String,
    open: bool,
    use_cache: bool,
    focus: Loc,
    #[cfg(not(target_os = "android"))]
    question: String,
    #[cfg(not(target_os = "android"))]
    answer: Option<String>,
    result: Option<ChapterAnalysis>,
    error: Option<String>,
    work: Option<Receiver<Result<Outcome, String>>>,
}

impl Analysis {
    pub fn new(data: &Path) -> Self {
        Self {
            library: data.join("velmira.db"),
            cache: data.join("ai-chapter-analysis.db"),
            chapter: None,
            title: String::new(),
            open: false,
            use_cache: true,
            focus: Loc::default(),
            #[cfg(not(target_os = "android"))]
            question: String::new(),
            #[cfg(not(target_os = "android"))]
            answer: None,
            result: None,
            error: None,
            work: None,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(not(target_os = "android"))]
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn prepare(&mut self, book: &str, focus: Loc, title: &str) {
        self.open = true;
        let chapter = Chapter {
            book: book.to_owned(),
            index: focus.chapter,
        };
        if self.work.is_none() && self.chapter.as_ref() != Some(&chapter) {
            self.chapter = Some(chapter);
            self.title = title.to_owned();
            self.result = None;
            self.error = None;
            #[cfg(not(target_os = "android"))]
            {
                self.answer = None;
                self.question.clear();
            }
        }
        if self.work.is_none() {
            self.focus = focus;
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pool: impl FnOnce() -> Result<Arc<ComicService>, String>,
    ) -> Option<(String, Loc)> {
        self.poll();
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut jump = None;
        egui::Window::new("章节分析")
            .open(&mut open)
            .default_width(560.0)
            .max_height(ctx.screen_rect().height() * 0.83)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| jump = self.content(ui, pool));
            });
        self.open = open && jump.is_none();
        jump
    }

    pub fn poll(&mut self) {
        if let Some(work) = &self.work {
            match work.try_recv() {
                Ok(result) => {
                    self.work = None;
                    match result {
                        Ok(Outcome::Analysis(result)) => self.result = Some(result),
                        #[cfg(not(target_os = "android"))]
                        Ok(Outcome::Answer(text)) => self.answer = Some(text),
                        Err(error) => self.error = Some(error),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.work = None;
                    self.error = Some("分析任务已中止。".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    pub fn content(
        &mut self,
        ui: &mut egui::Ui,
        pool: impl FnOnce() -> Result<Arc<ComicService>, String>,
    ) -> Option<(String, Loc)> {
        let mut request = None;
        let mut jump = None;
        let chapter = self.chapter.clone()?;
        {
            ui.label(egui::RichText::new(&self.title).strong());
            ui.label("分析会把本章正文分段发送到模型池。");
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.use_cache, "复用七天内的分析结果");
                if ui
                    .add_enabled(self.work.is_none(), egui::Button::new("分析本章"))
                    .clicked()
                {
                    request = Some(Request::Analyze);
                }
                if self.work.is_some() {
                    ui.spinner();
                }
            });
            #[cfg(not(target_os = "android"))]
            ui.collapsing("小说问答", |ui| {
                ui.small("根据当前阅读位置附近的原文回答。");
                ui.add(
                    egui::TextEdit::multiline(&mut self.question)
                        .char_limit(800)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                if ui
                    .add_enabled(
                        self.work.is_none() && !self.question.trim().is_empty(),
                        egui::Button::new("提问"),
                    )
                    .clicked()
                {
                    request = Some(Request::Ask(self.question.clone()));
                }
                if let Some(answer) = &self.answer {
                    ui.label(answer);
                }
            });
            if let Some(error) = &self.error {
                ui.label(error);
            }
            if let Some(analysis) = &self.result {
                if analysis.cached {
                    ui.small("已使用本地缓存");
                }
                ui.heading("剧情与事件");
                ui.label(&analysis.summary);
                ui.heading("章节连续性");
                ui.label(&analysis.continuity);
                if !analysis.characters.is_empty() {
                    ui.heading("人物与原文证据");
                }
                for (index, item) in analysis.characters.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.separator();
                        ui.label(egui::RichText::new(&item.name).strong());
                        if !item.aliases.is_empty() {
                            ui.small(item.aliases.join("、"));
                        }
                        ui.label(&item.description);
                        if ui.link(&item.quote).on_hover_text("跳到原文").clicked() {
                            jump = Some((chapter.book.clone(), item.loc));
                        }
                    });
                }
            }
        }
        if let Some(request) = request {
            self.error = None;
            match pool() {
                Ok(service) => self.start(ui.ctx(), chapter, service, request),
                Err(error) => self.error = Some(error),
            }
        }
        jump
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        chapter: Chapter,
        service: Arc<ComicService>,
        request: Request,
    ) {
        let ctx = ctx.clone();
        let library = self.library.clone();
        let cache = self.cache.clone();
        let use_cache = self.use_cache;
        #[cfg(not(target_os = "android"))]
        let focus = self.focus;
        let (send, receive) = bounded(1);
        match std::thread::Builder::new()
            .name("velmira-chapter-analysis".into())
            .spawn(move || {
                let result = vm_store::read_chapter(&library, &chapter.book, chapter.index)
                    .map_err(|error| error.to_string())
                    .and_then(|source| match request {
                        Request::Analyze => vm_ai::analysis::run(
                            &cache,
                            &chapter.book,
                            chapter.index,
                            &source,
                            &service,
                            use_cache,
                        )
                        .map(Outcome::Analysis),
                        #[cfg(not(target_os = "android"))]
                        Request::Ask(question) => {
                            vm_ai::analysis::answer(&service, &source, &question, focus)
                                .map(Outcome::Answer)
                        }
                    });
                let _ = send.send(result);
                ctx.request_repaint();
            }) {
            Ok(_) => self.work = Some(receive),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}
