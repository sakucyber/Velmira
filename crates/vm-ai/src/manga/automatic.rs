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
}

impl Plan {
    pub const MAX_CHAPTERS: u32 = MAX_PAGES;
    pub fn count(&self) -> Option<u32> {
        self.last
            .checked_sub(self.first)?
            .checked_add(1)
            .filter(|n| *n <= MAX_PAGES)
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
        if pages.len() > self.plan.count().expect("validated plan") as usize
            || pages
                .iter()
                .enumerate()
                .any(|(index, page)| page.draft.chapter != self.plan.first + index as u32)
        {
            return Err(ProjectError::InvalidDraft);
        }
        let done = pages.iter().take_while(|page| page.rendered).count() as u32;
        Ok(Progress {
            done,
            total: self.plan.count().expect("validated plan"),
        })
    }

    pub fn run(
        &mut self,
        service: &ComicService,
        source: impl FnMut(u32) -> Result<String, String>,
        paused: impl Fn() -> bool,
        notify: impl FnMut(Progress, u32, Stage),
    ) -> Result<Progress, String> {
        self.run_with(
            source,
            |text| service.storyboard(text).map_err(|error| error.to_string()),
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
        mut source: impl FnMut(u32) -> Result<String, String>,
        mut storyboard: impl FnMut(&str) -> Result<Storyboard, String>,
        mut render: impl FnMut(&Storyboard, Layout, &str) -> Result<ComicPage, String>,
        paused: impl Fn() -> bool,
        mut notify: impl FnMut(Progress, u32, Stage),
    ) -> Result<Progress, String> {
        let error = |error: ProjectError| error.to_string();
        let mut pages = self.project.pages().map_err(error)?.into_iter().peekable();
        let mut progress = Progress {
            done: 0,
            total: self.plan.count().expect("validated plan"),
        };
        for chapter in self.plan.first..=self.plan.last {
            let existing = pages.next_if(|page| page.draft.chapter == chapter);
            if existing.as_ref().is_some_and(|page| page.rendered) {
                progress.done += 1;
                continue;
            }
            if paused() {
                break;
            }
            if existing.is_none() && pages.peek().is_some() {
                return Err("漫画项目中的章节顺序不一致".into());
            }
            let result = (|| {
                let mut page = if let Some(page) = existing {
                    page
                } else {
                    notify(progress, chapter, Stage::Reading);
                    let text = source(chapter)?;
                    self.project
                        .add(Draft {
                            chapter,
                            source: text,
                            layout: self.plan.layout,
                            storyboard: None,
                        })
                        .map_err(error)?
                };
                if paused() {
                    return Ok(false);
                }
                if page.draft.storyboard.is_none() {
                    notify(progress, chapter, Stage::Storyboard);
                    page.draft.storyboard = Some(storyboard(&page.draft.source)?);
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
                break;
            }
            progress.done += 1;
        }
        Ok(progress)
    }

    pub fn export(&mut self, output: impl Write + Seek) -> Result<u32, ProjectError> {
        let progress = self.progress()?;
        if progress.done != progress.total {
            return Err(ProjectError::Incomplete);
        }
        self.project.export_cbz(output)
    }

    pub fn clear(self) -> Result<(), ProjectError> {
        self.project.clear()
    }
}
