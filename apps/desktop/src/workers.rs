use crossbeam_channel::{bounded, Receiver, Sender};
use eframe::egui;
use std::{path::PathBuf, sync::Arc, thread, time::Instant};
use vm_core::{ImportedText, Loc};
use vm_text::{PageBox, PageLayout, Paginator, RasterPage};

pub const CJK_FONT: &[u8] = include_bytes!("../assets/VelmiraCJK-Regular.ttf");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flip {
    Keep(Loc),
    First,
    Last,
    Page(usize),
}

pub enum LayoutCommand {
    Reflow {
        chapter: u32,
        paragraphs: Arc<[String]>,
        page: PageBox,
        position: Flip,
        color: [u8; 4],
    },
    Flip {
        position: Flip,
        color: [u8; 4],
    },
}

pub struct RenderedPage {
    pub index: usize,
    pub count: usize,
    pub start: Loc,
    pub raster: RasterPage,
    pub missing_glyphs: usize,
    pub layout: Arc<PageLayout>,
    pub layout_ms: Option<f64>,
    pub raster_ms: f64,
}

pub struct LayoutWorker {
    pub send: Sender<(u64, LayoutCommand)>,
    pub receive: Receiver<(u64, Result<RenderedPage, String>)>,
}

fn page_index(layout: &PageLayout, position: Flip) -> usize {
    match position {
        Flip::Keep(loc) => layout.page_for(loc).unwrap_or(0),
        Flip::First => 0,
        Flip::Last => layout.page_count().saturating_sub(1),
        Flip::Page(index) => index.min(layout.page_count().saturating_sub(1)),
    }
}

impl LayoutWorker {
    pub fn spawn(ctx: egui::Context) -> std::io::Result<Self> {
        let (send, commands) = bounded::<(u64, LayoutCommand)>(2);
        let (results, receive) = bounded(2);
        thread::Builder::new()
            .name("velmira-layout".into())
            .spawn(move || {
                let mut paginator = None;
                let mut layout = None;
                for (request, command) in commands {
                    let paginator = paginator.get_or_insert_with(|| {
                        if let Some(font) = custom_font() {
                            Paginator::new_with_font_data(Some(font))
                        } else {
                            Paginator::from_font_data(CJK_FONT.to_vec())
                        }
                    });
                    let result = (|| {
                        let (position, color, layout_ms) = match command {
                            LayoutCommand::Reflow {
                                chapter,
                                paragraphs,
                                page,
                                position,
                                color,
                            } => {
                                let started = Instant::now();
                                layout = None;
                                layout = Some(Arc::new(
                                    paginator
                                        .paginate(chapter, &paragraphs, page)
                                        .map_err(|error| error.to_string())?,
                                ));
                                (
                                    position,
                                    color,
                                    Some(started.elapsed().as_secs_f64() * 1_000.0),
                                )
                            }
                            LayoutCommand::Flip { position, color } => (position, color, None),
                        };
                        let layout = layout.as_ref().ok_or("当前章节还未排版")?;
                        let index = page_index(layout, position);
                        let start = layout.page_start(index).ok_or("没有可显示的页面")?;
                        let started = Instant::now();
                        let raster = paginator
                            .rasterize_page(layout, index, color)
                            .map_err(|error| error.to_string())?;
                        Ok(RenderedPage {
                            index,
                            start,
                            raster,
                            layout_ms,
                            count: layout.page_count(),
                            missing_glyphs: layout.missing_glyphs(),
                            layout: Arc::clone(layout),
                            raster_ms: started.elapsed().as_secs_f64() * 1_000.0,
                        })
                    })();
                    // Wake on every result. A disconnected UI also releases a full queue.
                    if results.send((request, result)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self { send, receive })
    }
}

pub enum ImportRequest {
    Pick,
    File(PathBuf),
}

pub enum ImportBook {
    Text(ImportedText),
    Epub(PathBuf),
    Comic(PathBuf),
}

fn import_book(path: PathBuf) -> Result<ImportBook, String> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt") => vm_format::read_txt(&path)
            .map(ImportBook::Text)
            .map_err(|e| e.to_string()),
        Some("epub") => Ok(ImportBook::Epub(path)),
        Some("cbz") => Ok(ImportBook::Comic(path)),
        _ => Err("Select a TXT, EPUB, or CBZ file.".into()),
    }
}

type ImportResult = Result<Option<ImportBook>, String>;

pub struct ImportWorker {
    pub send: Sender<ImportRequest>,
    pub receive: Receiver<ImportResult>,
}

impl ImportWorker {
    pub fn spawn(ctx: egui::Context) -> std::io::Result<Self> {
        let (send, requests) = bounded(1);
        let (results, receive) = bounded(1);
        thread::Builder::new()
            .name("velmira-import".into())
            .spawn(move || {
                for request in requests {
                    let path = match request {
                        ImportRequest::Pick => rfd::FileDialog::new()
                            .add_filter("Books", &["txt", "epub", "cbz"])
                            .pick_file(),
                        ImportRequest::File(path) => Some(path),
                    };
                    let result = path.map(import_book).transpose();
                    if results.send(result).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self { send, receive })
    }
}

fn custom_font() -> Option<Vec<u8>> {
    let path = PathBuf::from(std::env::var_os("VELMIRA_FONT")?);
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return None;
    }
    std::fs::read(path).ok()
}
