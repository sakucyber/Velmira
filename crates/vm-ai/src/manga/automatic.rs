use std::{
    io::{Seek, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::{Draft, Layout, Project, ProjectError, MAX_PAGES};
use crate::{service::ComicService, ComicPage, Storyboard};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Plan {
    pub title: String,
    /// Inclusive, zero-based chapter range.
    pub first: u32,
    pub last: u32,
    pub layout: Layout,
    pub style: String,
    #[serde(default = "one_page")]
    pub pages_per_chapter: u8,
}

fn one_page() -> u8 {
    1
}

pub fn chapter_excerpts(
    chapter: u32,
    paragraphs: &[String],
    pages: u8,
) -> Result<Vec<String>, String> {
    let pages = usize::from(pages);
    if !(1..=3).contains(&pages) {
        return Err("每章页数须在 1 至 3 页之间".into());
    }
    let nonempty: Vec<_> = paragraphs
        .iter()
        .enumerate()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(index, _)| index)
        .collect();
    if nonempty.is_empty() {
        return Err("本章没有可用正文".into());
    }
    if nonempty.len() < pages {
        let text = paragraphs.join("\n");
        let count = text.chars().count();
        if count < pages {
            return Err("本章没有足够的正文分配给所选页数".into());
        }
        return (0..pages)
            .map(|page| {
                let start = page * count / pages;
                let length = (page + 1) * count / pages - start;
                Ok(text.chars().skip(start).take(length.min(4000)).collect())
            })
            .collect();
    }
    let mut excerpts = Vec::new();
    for page in 0..pages {
        let first = nonempty[page * nonempty.len() / pages];
        let end = nonempty
            .get((page + 1) * nonempty.len() / pages)
            .copied()
            .unwrap_or(paragraphs.len());
        let window = &paragraphs[first..end];
        let mut candidates = vm_core::segments::segment_chapter(chapter, window);
        vm_core::segments::rank_segments(&mut candidates, 1);
        let source = candidates
            .iter()
            .find(|segment| segment.selected)
            .map_or(window, |segment| {
                &window[segment.start_para..=segment.end_para]
            });
        let text: String = source
            .iter()
            .flat_map(|paragraph| paragraph.chars().chain(['\n']))
            .take(4000)
            .collect();
        if text.trim().is_empty() {
            return Err("本章没有足够的正文分配给所选页数".into());
        }
        excerpts.push(text);
    }
    Ok(excerpts)
}

impl Plan {
    pub const MAX_CHAPTERS: u32 = MAX_PAGES;
    pub fn count(&self) -> Option<u32> {
        self.last
            .checked_sub(self.first)?
            .checked_add(1)
            .filter(|n| {
                (1..=3).contains(&self.pages_per_chapter)
                    && *n <= Self::MAX_CHAPTERS / u32::from(self.pages_per_chapter)
            })
    }

    pub fn total(&self) -> u32 {
        self.count().unwrap_or_default() * u32::from(self.pages_per_chapter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Reading,
    Storyboard,
    Rendering,
}

pub struct Job {
    project: Project,
    plan: Plan,
}

impl Job {
    pub fn open(path: &Path, plan: Plan) -> Result<Self, ProjectError> {
        if plan.count().is_none() {
            return Err(ProjectError::InvalidDraft);
        }
        let project = Project::open(path, "automatic")?;
        Ok(Self { project, plan })
    }

    pub fn progress(&self) -> Result<Progress, ProjectError> {
        let pages = self.project.pages()?;
        if pages.len() > self.plan.total() as usize
            || pages.iter().enumerate().any(|(i, page)| {
                page.draft.chapter
                    != self.plan.first + i as u32 / u32::from(self.plan.pages_per_chapter)
            })
            || pages.len() % usize::from(self.plan.pages_per_chapter) != 0
        {
            return Err(ProjectError::InvalidDraft);
        }
        Ok(Progress {
            done: pages.iter().take_while(|page| page.rendered).count() as u32,
            total: self.plan.total(),
        })
    }

    pub fn run(
        &mut self,
        service: &ComicService,
        source: impl FnMut(u32, u8) -> Result<Vec<String>, String>,
        continuity: impl FnMut(u32) -> String,
        paused: impl Fn() -> bool,
        notify: impl FnMut(Progress, u32, Stage),
    ) -> Result<Progress, String> {
        let mut continuity = continuity;
        self.run_with(
            source,
            |chapter, text| {
                service
                    .storyboard_with_continuity(text, &continuity(chapter))
                    .map_err(|error| error.to_string())
            },
            |board, layout, style| {
                service
                    .render_styled(board, layout, style)
                    .map_err(|error| error.to_string())
            },
            paused,
            notify,
        )
    }

    fn run_with(
        &mut self,
        mut source: impl FnMut(u32, u8) -> Result<Vec<String>, String>,
        mut storyboard: impl FnMut(u32, &str) -> Result<Storyboard, String>,
        mut render: impl FnMut(&Storyboard, Layout, &str) -> Result<ComicPage, String>,
        paused: impl Fn() -> bool,
        mut notify: impl FnMut(Progress, u32, Stage),
    ) -> Result<Progress, String> {
        let error = |error: ProjectError| error.to_string();
        let mut progress = self.progress().map_err(error)?;
        progress.done = 0;
        let mut pages = self.project.pages().map_err(error)?.into_iter().peekable();
        let count = usize::from(self.plan.pages_per_chapter);
        for chapter in self.plan.first..=self.plan.last {
            let mut drafts = Vec::with_capacity(count);
            while let Some(page) = pages.next_if(|page| page.draft.chapter == chapter) {
                drafts.push(page);
            }
            if drafts.is_empty() {
                if paused() {
                    return Ok(progress);
                }
                notify(progress, chapter, Stage::Reading);
                let excerpts = source(chapter, self.plan.pages_per_chapter)
                    .map_err(|error| format!("第 {} 章：{error}", chapter + 1))?;
                if excerpts.len() != count {
                    return Err(format!("第 {} 章的分页数与计划不一致", chapter + 1));
                }
                let entries = excerpts
                    .into_iter()
                    .map(|text| Draft {
                        chapter,
                        source: text,
                        layout: self.plan.layout,
                        storyboard: None,
                    })
                    .collect();
                drafts = self.project.add_all(entries).map_err(error)?;
            }
            for mut page in drafts {
                if page.rendered {
                    progress.done += 1;
                    continue;
                }
                if paused() {
                    return Ok(progress);
                }
                let result = (|| {
                    if page.draft.storyboard.is_none() {
                        notify(progress, chapter, Stage::Storyboard);
                        page.draft.storyboard = Some(storyboard(chapter, &page.draft.source)?);
                        page = self.project.save(page).map_err(error)?;
                    }
                    if paused() {
                        return Ok(false);
                    }
                    notify(progress, chapter, Stage::Rendering);
                    let image = render(
                        page.draft.storyboard.as_ref().expect("stored storyboard"),
                        page.draft.layout,
                        &self.plan.style,
                    )?;
                    self.project.store_render(&page, &image).map_err(error)?;
                    Ok::<_, String>(true)
                })()
                .map_err(|error| format!("第 {} 章：{error}", chapter + 1))?;
                if !result {
                    return Ok(progress);
                }
                progress.done += 1;
            }
        }
        Ok(progress)
    }

    pub fn export(
        &mut self,
        output: impl Write + Seek,
        novel_id: &str,
    ) -> Result<u32, ProjectError> {
        let progress = self.progress()?;
        if progress.done != progress.total {
            return Err(ProjectError::Incomplete);
        }
        self.project.export_for_novel(output, novel_id)
    }

    pub fn clear(self) -> Result<(), ProjectError> {
        self.project.clear()
    }
}
