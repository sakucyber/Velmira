//! Native chapter pagination and Swash glyph rasterization.
//!
//! Keep the paginator on a background thread. It owns the font database and
//! raster cache. Layouts hold compact glyph positions for one chapter and
//! never copy its source text. Pixel buffers are created only for requested pages.

use cosmic_text::{
    fontdb, Align, Attrs, AttrsList, Buffer, BufferLine, Family, FontSystem, LineEnding, LineIter,
    Metrics, PhysicalGlyph, Shaping, SwashCache, SwashContent, SwashImage,
};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
pub use vm_core::Loc;

const MAX_CHAPTER_BYTES: usize = 1024 * 1024;
const MAX_CHAPTER_GLYPHS: usize = 250_000;
const MAX_CHAPTER_LINES: usize = 50_000;
const MAX_SHAPED_CACHE_CHARS: usize = 8_192;
const MAX_RASTER_PIXELS: usize = 8 * 1024 * 1024;
const MAX_GLYPH_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_GLYPH_CACHE_ENTRIES: usize = 4096;
static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

/// Text area dimensions and metrics, in raster pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
    pub line_height: f32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("invalid text area or font metrics")]
    InvalidPageBox,
    #[error("chapter exceeds the native layout budget")]
    ChapterTooLarge,
    #[error("no fonts are available; install a system font or supply font data")]
    NoFonts,
    #[error("page index out of range")]
    NoSuchPage,
    #[error("page layout belongs to a different font system")]
    FontSystemMismatch,
    #[error("could not allocate a page buffer")]
    OutOfMemory,
}

impl PageBox {
    fn pixel_dimensions(self) -> Result<(u32, u32), LayoutError> {
        if !(1.0..=4096.0).contains(&self.width)
            || !(1.0..=4096.0).contains(&self.height)
            || !(1.0..=128.0).contains(&self.font_size)
            || !(self.font_size..=256.0).contains(&self.line_height)
            || self.width < self.font_size
            || self.height < self.line_height
        {
            return Err(LayoutError::InvalidPageBox);
        }
        let width = self.width.ceil() as u32;
        let height = self.height.ceil() as u32;
        if width as usize * height as usize > MAX_RASTER_PIXELS {
            return Err(LayoutError::InvalidPageBox);
        }
        Ok((width, height))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug)]
struct Glyph {
    physical: PhysicalGlyph,
    start: u32,
    end: u32,
    x: f32,
    width: f32,
    rtl: bool,
}

impl Glyph {
    fn visual_edges(&self) -> [(f32, u32); 2] {
        let (left, right) = if self.rtl {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        };
        [(self.x, left), (self.x + self.width, right)]
    }
}

#[derive(Debug)]
struct Line {
    para: u32,
    start: u32,
    y: f32,
    height: f32,
    baseline: i32,
    glyphs: Vec<Glyph>,
}

#[derive(Debug)]
struct Page {
    start: Loc,
    lines: Vec<Line>,
}

#[derive(Debug)]
pub struct PageLayout {
    engine: u64,
    chapter: u32,
    paragraph_lengths: Vec<u32>,
    width: u32,
    height: u32,
    pages: Vec<Page>,
    missing_glyphs: usize,
}

impl PageLayout {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_start(&self, index: usize) -> Option<Loc> {
        self.pages.get(index).map(|page| page.start)
    }

    /// Find a saved position by its Unicode scalar offset, independent of view
    /// width, height, or font size.
    pub fn page_for(&self, loc: Loc) -> Option<usize> {
        if loc.chapter != self.chapter
            || loc.off > *self.paragraph_lengths.get(loc.para as usize)?
        {
            return None;
        }
        Some(
            self.pages
                .partition_point(|page| location_key(page.start) <= location_key(loc))
                .saturating_sub(1),
        )
    }

    /// Resolve a pointer to the nearest visual glyph-cluster edge.
    pub fn hit_test(&self, index: usize, x: f32, y: f32) -> Option<Loc> {
        if !(x.is_finite() && y.is_finite()) {
            return None;
        }
        let page = self.pages.get(index)?;
        let line = match page.lines.as_slice() {
            [] => return Some(page.start),
            lines => {
                let index = lines
                    .partition_point(|line| line.y + line.height <= y)
                    .min(lines.len() - 1);
                &lines[index]
            }
        };
        let off = line
            .glyphs
            .iter()
            .flat_map(Glyph::visual_edges)
            .min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs()))
            .map_or(line.start, |edge| edge.1);
        Some(Loc {
            chapter: self.chapter,
            para: line.para,
            off,
        })
    }

    /// Produce per-line highlight rectangles for the half-open logical range.
    /// A cluster is highlighted as a unit when the range intersects it.
    pub fn range_rects(&self, index: usize, range: (Loc, Loc)) -> Vec<Rect> {
        let mut out = Vec::new();
        let (mut start, mut end) = range;
        if location_key(start) > location_key(end) {
            std::mem::swap(&mut start, &mut end);
        }
        if location_key(start) == location_key(end) {
            return out;
        }
        let Some(page) = self.pages.get(index) else {
            return out;
        };
        for line in &page.lines {
            let prefix = (self.chapter, line.para);
            let mut spans: Vec<_> = line
                .glyphs
                .iter()
                .filter(|g| {
                    g.width > 0.0
                        && (prefix.0, prefix.1, g.start) < location_key(end)
                        && (prefix.0, prefix.1, g.end) > location_key(start)
                })
                .map(|g| (g.x, g.x + g.width))
                .collect();
            spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            let mut active: Option<(f32, f32)> = None;
            for (left, right) in spans {
                if let Some((first, last)) = active {
                    if left <= last + 0.5 {
                        active = Some((first, last.max(right)));
                        continue;
                    }
                    out.push(line.rectangle(first, last));
                }
                active = Some((left, right));
            }
            if let Some((first, last)) = active {
                out.push(line.rectangle(first, last));
            }
        }
        out
    }

    /// Number of laid-out glyphs the font fallback could not resolve.
    pub fn missing_glyphs(&self) -> usize {
        self.missing_glyphs
    }
}

impl Line {
    fn rectangle(&self, left: f32, right: f32) -> Rect {
        Rect {
            x: left,
            y: self.y,
            width: right - left,
            height: self.height,
        }
    }
}

fn location_key(loc: Loc) -> (u32, u32, u32) {
    (loc.chapter, loc.para, loc.off)
}

/// An unpremultiplied RGBA image with a transparent background.
#[derive(Debug)]
pub struct RasterPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Shaping and rasterization context. Reuse it across layouts.
pub struct Paginator {
    id: u64,
    fonts: FontSystem,
    glyphs: SwashCache,
    glyph_cache_bytes: usize,
    paragraph_buffers: Vec<Buffer>,
}

impl Default for Paginator {
    fn default() -> Self {
        Self::new()
    }
}

impl Paginator {
    pub fn new() -> Self {
        Self::new_with_font_data(None)
    }

    /// Load system fonts and optionally prefer a provided font (e.g. Noto SC
    /// on mobile). Unreadable font bytes simply leave system fallback enabled.
    pub fn new_with_font_data(data: Option<Vec<u8>>) -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self::from_font_db(db, data)
    }

    /// Use only provided font bytes. This avoids system font discovery on a
    /// platform that already manages font assets, or for reproducible layout.
    pub fn from_font_data(data: Vec<u8>) -> Self {
        Self::from_font_db(fontdb::Database::new(), Some(data))
    }

    fn from_font_db(mut db: fontdb::Database, data: Option<Vec<u8>>) -> Self {
        let embedded = data.and_then(|bytes| {
            let ids = db.load_font_source(fontdb::Source::Binary(std::sync::Arc::new(bytes)));
            ids.first()
                .and_then(|id| db.face(*id))
                .and_then(|face| face.families.first())
                .map(|(family, _)| family.clone())
        });
        if let Some(family) = embedded.or_else(|| preferred_family(&db)) {
            db.set_sans_serif_family(family);
        }
        Self {
            id: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed),
            fonts: FontSystem::new_with_locale_and_db("zh-CN".into(), db),
            glyphs: SwashCache::new(),
            glyph_cache_bytes: 0,
            paragraph_buffers: Vec::new(),
        }
    }

    pub fn paginate(
        &mut self,
        chapter: u32,
        paragraphs: &[impl AsRef<str>],
        page: PageBox,
    ) -> Result<PageLayout, LayoutError> {
        self.paginate_with_justification(chapter, paragraphs, page, false)
    }

    pub fn paginate_with_justification(
        &mut self,
        chapter: u32,
        paragraphs: &[impl AsRef<str>],
        page: PageBox,
        justify: bool,
    ) -> Result<PageLayout, LayoutError> {
        let (width, height) = page.pixel_dimensions()?;
        if self.fonts.db().is_empty() {
            return Err(LayoutError::NoFonts);
        }
        let mut bytes = 0usize;
        let mut chars = 0usize;
        for paragraph in paragraphs {
            let paragraph = paragraph.as_ref();
            bytes = bytes.saturating_add(paragraph.len());
            if bytes > MAX_CHAPTER_BYTES {
                return Err(LayoutError::ChapterTooLarge);
            }
            chars = chars.saturating_add(paragraph.chars().count());
            if chars > MAX_CHAPTER_GLYPHS {
                return Err(LayoutError::ChapterTooLarge);
            }
        }
        if paragraphs.len() > MAX_CHAPTER_LINES {
            return Err(LayoutError::ChapterTooLarge);
        }
        // Keep a small chapter's shaped paragraphs across relayout, without
        // retaining exceptionally long chapters' buffers between requests.
        let reuse_paragraphs = chars <= MAX_SHAPED_CACHE_CHARS && paragraphs.len() <= 256;
        if reuse_paragraphs {
            self.paragraph_buffers.truncate(paragraphs.len());
        } else {
            self.paragraph_buffers.clear();
        }

        let mut result = PageLayout {
            engine: self.id,
            chapter,
            paragraph_lengths: Vec::with_capacity(paragraphs.len().max(1)),
            width,
            height,
            pages: vec![Page {
                start: Loc {
                    chapter,
                    para: 0,
                    off: 0,
                },
                lines: Vec::new(),
            }],
            missing_glyphs: 0,
        };
        let mut y = 0.0;
        let mut line_count = 0;
        let mut glyph_count = 0;
        let metrics = Metrics::new(page.font_size, page.line_height);
        let mut scratch = Buffer::new_empty(metrics);
        let attrs = Attrs::new().family(Family::SansSerif);

        for (para, paragraph) in paragraphs.iter().enumerate() {
            let buffer = if reuse_paragraphs {
                if self.paragraph_buffers.len() == para {
                    self.paragraph_buffers.push(Buffer::new_empty(metrics));
                }
                &mut self.paragraph_buffers[para]
            } else {
                &mut scratch
            };
            let text = paragraph.as_ref();
            let offsets = CharOffsets::new(text);
            let mut logical_line_start = 0;
            let mut logical_line_index = 0;
            result.paragraph_lengths.push(offsets.count());
            prepare_paragraph(
                buffer,
                text,
                &attrs,
                justify,
                MAX_CHAPTER_LINES - line_count,
            )?;
            buffer.set_metrics_and_size(&mut self.fonts, metrics, Some(page.width), None);
            buffer.shape_until_scroll(&mut self.fonts, true);

            for run in buffer.layout_runs() {
                line_count += 1;
                glyph_count += run.glyphs.len();
                if line_count > MAX_CHAPTER_LINES || glyph_count > MAX_CHAPTER_GLYPHS {
                    return Err(LayoutError::ChapterTooLarge);
                }
                while logical_line_index < run.line_i {
                    let line = &buffer.lines[logical_line_index];
                    logical_line_start += line.text().len() + line.ending().as_str().len();
                    logical_line_index += 1;
                }
                let line_start = run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.start)
                    .min()
                    .unwrap_or(0);
                let start = offsets.at(logical_line_start + line_start);
                let loc = Loc {
                    chapter,
                    para: para as u32,
                    off: start,
                };
                if y > 0.0 && y + run.line_height > page.height + 0.001 {
                    result.pages.push(Page {
                        start: loc,
                        lines: Vec::new(),
                    });
                    y = 0.0;
                }
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|g| {
                        if g.glyph_id == 0
                            && run.text.get(g.start..g.end).is_some_and(|cluster| {
                                cluster
                                    .chars()
                                    .any(|c| !(c.is_whitespace() || c.is_control()))
                            })
                        {
                            result.missing_glyphs += 1;
                        }
                        Glyph {
                            physical: g.physical((0.0, 0.0), 1.0),
                            start: offsets.at(logical_line_start + g.start),
                            end: offsets.at(logical_line_start + g.end),
                            x: g.x,
                            width: g.w,
                            rtl: g.level.is_rtl(),
                        }
                    })
                    .collect();
                result
                    .pages
                    .last_mut()
                    .expect("first page")
                    .lines
                    .push(Line {
                        para: loc.para,
                        start,
                        y,
                        height: run.line_height,
                        baseline: (y + run.line_y - run.line_top) as i32,
                        glyphs,
                    });
                y += run.line_height;
            }
        }
        if paragraphs.is_empty() {
            result.paragraph_lengths.push(0);
        }
        Ok(result)
    }

    pub fn rasterize_page(
        &mut self,
        layout: &PageLayout,
        index: usize,
        text_rgba: [u8; 4],
    ) -> Result<RasterPage, LayoutError> {
        if layout.engine != self.id {
            return Err(LayoutError::FontSystemMismatch);
        }
        let page = layout.pages.get(index).ok_or(LayoutError::NoSuchPage)?;
        let stride = layout.width as usize * 4;
        let len = stride * layout.height as usize;
        if len > 8 * 1024 * 1024 {
            log::debug!("allocating {len} bytes for reading page");
        }
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(len)
            .map_err(|_| LayoutError::OutOfMemory)?;
        rgba.resize(len, 0);
        if text_rgba[3] == 0 {
            return Ok(RasterPage {
                width: layout.width,
                height: layout.height,
                rgba,
            });
        }
        for line in &page.lines {
            for glyph in &line.glyphs {
                let PhysicalGlyph { cache_key, x, y } = glyph.physical;
                if !self.glyphs.image_cache.contains_key(&cache_key) {
                    let bytes = self
                        .glyphs
                        .get_image(&mut self.fonts, cache_key)
                        .as_ref()
                        .map_or(0, |image| image.data.len());
                    self.glyph_cache_bytes += bytes;
                }
                if let Some(image) = &self.glyphs.image_cache[&cache_key] {
                    // Clip once per glyph, avoiding per-pixel boundary checks.
                    blit_glyph(
                        &mut rgba,
                        (layout.width, layout.height),
                        (x, line.baseline + y),
                        image,
                        text_rgba,
                    );
                }
                if self.glyph_cache_bytes > MAX_GLYPH_CACHE_BYTES
                    || self.glyphs.image_cache.len() > MAX_GLYPH_CACHE_ENTRIES
                {
                    self.glyphs.image_cache.clear();
                    self.glyph_cache_bytes = 0;
                }
            }
        }
        Ok(RasterPage {
            width: layout.width,
            height: layout.height,
            rgba,
        })
    }
}

fn prepare_paragraph(
    buffer: &mut Buffer,
    text: &str,
    attrs: &Attrs<'_>,
    justify: bool,
    max_lines: usize,
) -> Result<(), LayoutError> {
    // The synthetic empty line is only needed when the input has no lines.
    let lines = LineIter::new(text).chain(text.is_empty().then_some((0..0, LineEnding::None)));
    let mut count = 0;
    for (range, ending) in lines {
        if count == max_lines {
            return Err(LayoutError::ChapterTooLarge);
        }
        let attrs = AttrsList::new(attrs);
        let text = &text[range];
        if let Some(line) = buffer.lines.get_mut(count) {
            line.set_text(text, ending, attrs);
            line.set_align(justify.then_some(Align::Justified));
        } else {
            let mut line = BufferLine::new(text, ending, attrs, Shaping::Advanced);
            line.set_align(justify.then_some(Align::Justified));
            buffer.lines.push(line);
        }
        count += 1;
    }
    buffer.lines.truncate(count);
    Ok(())
}

struct CharOffsets(Vec<u32>);

impl CharOffsets {
    fn new(text: &str) -> Self {
        Self(
            text.char_indices()
                .map(|(i, _)| i as u32)
                .chain([text.len() as u32])
                .collect(),
        )
    }

    fn at(&self, byte: usize) -> u32 {
        match self.0.binary_search(&(byte as u32)) {
            Ok(index) => index as u32,
            Err(next) => next.saturating_sub(1) as u32,
        }
    }

    fn count(&self) -> u32 {
        (self.0.len() - 1) as u32
    }
}

fn preferred_family(db: &fontdb::Database) -> Option<String> {
    let wanted = [
        "Microsoft YaHei",
        "Microsoft YaHei UI",
        "PingFang SC",
        "Noto Sans CJK SC",
        "Noto Sans SC",
        "Droid Sans Fallback",
        "WenQuanYi Zen Hei",
        "Segoe UI",
        "Arial",
        "Liberation Sans",
        "DejaVu Sans",
    ];
    wanted
        .iter()
        .find(|name| {
            db.faces()
                .any(|face| face.families.iter().any(|(family, _)| family == *name))
        })
        .map(|name| (*name).into())
        .or_else(|| {
            db.faces()
                .next()?
                .families
                .first()
                .map(|(family, _)| family.clone())
        })
}

fn blit_glyph(
    dst: &mut [u8],
    (width, height): (u32, u32),
    (x, y): (i32, i32),
    image: &SwashImage,
    color: [u8; 4],
) {
    if matches!(image.content, SwashContent::SubpixelMask) {
        // The scaler is configured for grayscale coverage.
        return;
    }
    let left = x + image.placement.left;
    let top = y - image.placement.top;
    let right = (left + image.placement.width as i32).min(width as i32);
    let bottom = (top + image.placement.height as i32).min(height as i32);
    let (x0, y0) = (left.max(0), top.max(0));
    if x0 >= right || y0 >= bottom {
        return;
    }
    let image_stride = image.placement.width as usize;
    for row in y0..bottom {
        let src = (row - top) as usize * image_stride + (x0 - left) as usize;
        let start = (row as usize * width as usize + x0 as usize) * 4;
        let pixels = (right - x0) as usize;
        let out = dst[start..start + pixels * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut();
        if matches!(image.content, SwashContent::Mask) {
            for (dst, coverage) in out.zip(&image.data[src..src + pixels]) {
                let mut pixel = color;
                pixel[3] = scale_alpha(*coverage, color[3]);
                blend(dst, pixel);
            }
        } else {
            let data = &image.data[src * 4..(src + pixels) * 4];
            for (dst, src) in out.zip(data.as_chunks::<4>().0) {
                blend(dst, [src[0], src[1], src[2], scale_alpha(src[3], color[3])]);
            }
        }
    }
}

fn scale_alpha(coverage: u8, opacity: u8) -> u8 {
    if opacity == 255 {
        coverage
    } else {
        ((u16::from(coverage) * u16::from(opacity) + 127) / 255) as u8
    }
}

fn blend(dst: &mut [u8], src: [u8; 4]) {
    if src[3] == 0 {
        return;
    }
    if dst[3] == 0 || src[3] == 255 {
        dst.copy_from_slice(&src);
        return;
    }
    let old_alpha = u32::from(dst[3]) * (255 - u32::from(src[3]));
    let new_alpha = 255 * u32::from(src[3]);
    let alpha = old_alpha + new_alpha;
    for channel in 0..3 {
        let value = u32::from(dst[channel]) * old_alpha + u32::from(src[channel]) * new_alpha;
        dst[channel] = ((value + alpha / 2) / alpha) as u8;
    }
    dst[3] = ((alpha + 127) / 255) as u8;
}
