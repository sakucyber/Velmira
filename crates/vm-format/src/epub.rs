use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use percent_encoding::percent_decode_str;
use roxmltree::{Document, ParsingOptions};
use vm_core::{ImportedChapter, ImportedText, TxtEncoding};
use zip::ZipArchive;

use crate::archive::{
    check_raster_signature, image_media_type, open_archive, read_member, MAX_IMAGE_BYTES,
};
use crate::txt::{decode, title_author_from_filename};
use crate::FormatError;

const CONTAINER: &str = "META-INF/container.xml";
const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHAPTER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubImage {
    pub path: String,
    pub media_type: &'static str,
}

/// Position immediately before paragraph `before_para`, or at the end of the chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubIllustration {
    pub chapter: usize,
    pub before_para: usize,
    pub image: EpubImage,
    pub alt: String,
}

#[derive(Debug)]
pub struct ParsedEpub {
    pub text: ImportedText,
    pub author: Option<String>,
    pub illustrations: Vec<EpubIllustration>,
    /// Candidates in priority order. Consumers can skip corrupted or undecodable images.
    pub cover_candidates: Vec<EpubImage>,
}

pub struct EpubReader<R> {
    members: Members<R>,
}

impl EpubReader<File> {
    pub fn open(path: &Path) -> Result<Self, FormatError> {
        Self::new(File::open(path)?)
    }
}

impl<R: Read + Seek> EpubReader<R> {
    pub fn new(reader: R) -> Result<Self, FormatError> {
        Ok(Self {
            members: Members::new(open_archive(reader)?)?,
        })
    }

    pub fn parse(&mut self, file_name: &str) -> Result<ParsedEpub, FormatError> {
        let opf_path = self.members.opf_path()?;
        let opf = self.members.read(&opf_path, MAX_XML_BYTES)?;
        let package = parse_package(&opf_path, &opf)?;
        let nav_titles = self.members.navigation(&package)?;
        let mut cover_candidates = self.members.covers(&package)?;
        let declared_media: HashMap<&str, &str> = package
            .items
            .values()
            .map(|item| (item.path.as_str(), item.media.as_str()))
            .collect();

        let (fallback_title, fallback_author) = title_author_from_filename(file_name);
        let title = package
            .title
            .as_deref()
            .filter(|title| !title.is_empty())
            .unwrap_or(fallback_title);
        let author = package
            .author
            .or_else(|| fallback_author.map(str::to_owned));
        let mut chapters = Vec::new();
        let mut illustrations = Vec::new();
        let mut read_bytes = 0;
        for id in &package.spine {
            let Some(item) = package.items.get(id) else {
                continue;
            };
            if !item.is_text() || !self.members.has(&item.path) {
                continue;
            }
            let remaining = MAX_TEXT_BYTES - read_bytes;
            let raw = self
                .members
                .read(&item.path, remaining.min(MAX_CHAPTER_BYTES))?;
            read_bytes += raw.len() as u64;
            let mut body = tokenize(&decode(&raw).0, Html::body());
            let had_heading = usize::from(body.blocks.first().is_some_and(|block| block.0));
            let title = if had_heading == 1 {
                body.blocks.remove(0).1
            } else {
                nav_titles
                    .get(&item.path)
                    .cloned()
                    .unwrap_or_else(|| format!("第{}部分", chapters.len() + 1))
            };
            let paragraphs = body
                .blocks
                .into_iter()
                .map(|block| block.1)
                .collect::<Vec<_>>();
            let images = body
                .images
                .into_iter()
                .filter_map(|image| {
                    let path = resolve_href(directory(&item.path), &image.href)?;
                    let image_info = self
                        .members
                        .image(&path, declared_media.get(path.as_str()).copied())?;
                    Some(EpubIllustration {
                        chapter: chapters.len(),
                        before_para: image
                            .before
                            .saturating_sub(had_heading)
                            .min(paragraphs.len()),
                        image: image_info,
                        alt: image.alt,
                    })
                })
                .collect::<Vec<_>>();
            if is_front_matter(&item.path, &title, paragraphs.first())
                || (paragraphs.is_empty() && images.is_empty())
            {
                continue;
            }
            illustrations.extend(images);
            chapters.push(ImportedChapter { title, paragraphs });
        }
        if chapters.is_empty() {
            return Err(FormatError::Empty);
        }
        let mut seen = HashSet::new();
        cover_candidates.retain(|image| seen.insert(image.path.clone()));
        Ok(ParsedEpub {
            text: ImportedText {
                title: title.to_owned(),
                encoding: TxtEncoding::Utf8,
                chapters,
            },
            author,
            illustrations,
            cover_candidates,
        })
    }

    /// Reads one image directly from the archive. The caller owns decoded-image limits.
    pub fn read_image(&mut self, image: &EpubImage) -> Result<Vec<u8>, FormatError> {
        let bytes = self.members.read(&image.path, MAX_IMAGE_BYTES)?;
        check_raster_signature(&bytes, image.media_type)?;
        Ok(bytes)
    }
}

pub fn parse_epub(bytes: &[u8], file_name: &str) -> Result<ParsedEpub, FormatError> {
    EpubReader::new(Cursor::new(bytes))?.parse(file_name)
}

pub fn read_epub(path: &Path) -> Result<ParsedEpub, FormatError> {
    let file_name = path.file_name().map(|name| name.to_string_lossy());
    EpubReader::open(path)?.parse(file_name.as_deref().unwrap_or_default())
}

struct Members<R> {
    archive: ZipArchive<R>,
    exact: HashMap<String, (usize, u64)>,
    lowercase: HashMap<String, String>,
    names: Vec<String>,
}

impl<R: Read + Seek> Members<R> {
    fn new(mut archive: ZipArchive<R>) -> Result<Self, FormatError> {
        let mut exact = HashMap::with_capacity(archive.len());
        let mut lowercase = HashMap::with_capacity(archive.len());
        let mut names = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            exact.insert(name.clone(), (index, entry.size()));
            lowercase.insert(name.to_lowercase(), name.clone());
            names.push(name);
        }
        Ok(Self {
            archive,
            exact,
            lowercase,
            names,
        })
    }

    fn name<'a>(&'a self, path: &'a str) -> Option<&'a str> {
        if self.exact.contains_key(path) {
            Some(path)
        } else {
            self.lowercase.get(&path.to_lowercase()).map(String::as_str)
        }
    }

    fn has(&self, path: &str) -> bool {
        self.name(path).is_some()
    }

    fn read(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, FormatError> {
        let index = self
            .name(path)
            .map(|name| self.exact[name].0)
            .ok_or_else(|| FormatError::MissingImage(path.to_owned()))?;
        read_member(self.archive.by_index(index)?, limit)
    }

    fn image(&self, path: &str, media_type: Option<&str>) -> Option<EpubImage> {
        let path = self.name(path)?;
        if self.exact[path].1 > MAX_IMAGE_BYTES {
            return None;
        }
        let media_type = media_type
            .and_then(supported_media)
            .or_else(|| image_media_type(path))?;
        Some(EpubImage {
            path: path.to_owned(),
            media_type,
        })
    }

    fn opf_path(&mut self) -> Result<String, FormatError> {
        if self.has(CONTAINER) {
            let data = self.read(CONTAINER, MAX_XML_BYTES)?;
            let xml = decode(&data).0;
            if let Ok(doc) = xml_doc(&xml) {
                if let Some(path) = doc
                    .descendants()
                    .filter(|node| node.has_tag_name("rootfile"))
                    .find_map(|node| {
                        node.attribute("full-path")
                            .and_then(|path| resolve_href("", path))
                    })
                {
                    if self.has(&path) {
                        return Ok(path);
                    }
                }
            }
        }
        self.names
            .iter()
            .find(|name| name.to_ascii_lowercase().ends_with(".opf"))
            .cloned()
            .ok_or(FormatError::MissingEpubEntry("OPF"))
    }

    fn navigation(&mut self, package: &Package) -> Result<HashMap<String, String>, FormatError> {
        let mut titles = HashMap::new();
        for id in &package.manifest_order {
            let item = &package.items[id];
            if !self.has(&item.path) {
                continue;
            }
            if item
                .properties
                .split_whitespace()
                .any(|value| value == "nav")
            {
                let data = self.read(&item.path, MAX_XML_BYTES)?;
                let html = tokenize(&decode(&data).0, Html::nav());
                for (href, title) in html.anchors {
                    if let Some(path) = resolve_href(directory(&item.path), &href) {
                        titles.insert(path, title);
                    }
                }
            } else if item.media == "application/x-dtbncx+xml" {
                let data = self.read(&item.path, MAX_XML_BYTES)?;
                let xml = decode(&data).0;
                if let Ok(doc) = xml_doc(&xml) {
                    for point in doc
                        .descendants()
                        .filter(|node| node.has_tag_name("navPoint"))
                    {
                        let label = point.children().find(|node| node.has_tag_name("navLabel"));
                        let content = point.children().find(|node| node.has_tag_name("content"));
                        if let (Some(label), Some(content)) = (label, content) {
                            let title = label
                                .descendants()
                                .find(|node| node.has_tag_name("text"))
                                .and_then(|node| node.text());
                            let path = content
                                .attribute("src")
                                .and_then(|href| resolve_href(directory(&item.path), href));
                            if let (Some(title), Some(path)) = (title, path) {
                                titles.entry(path).or_insert_with(|| clean(title));
                            }
                        }
                    }
                }
            }
        }
        titles.retain(|_, title| !title.is_empty());
        Ok(titles)
    }

    fn covers(&mut self, package: &Package) -> Result<Vec<EpubImage>, FormatError> {
        let mut candidates = Vec::new();
        if let Some(item) = package
            .cover_id
            .as_ref()
            .and_then(|id| package.items.get(id))
        {
            candidates.extend(self.cover_item(item)?);
        }
        for id in &package.manifest_order {
            let item = &package.items[id];
            if item
                .properties
                .split_whitespace()
                .any(|property| property == "cover-image")
            {
                candidates.extend(self.cover_item(item)?);
            }
        }
        if let Some(path) = &package.guide_cover {
            let item = Item {
                path: path.clone(),
                media: String::new(),
                properties: String::new(),
            };
            candidates.extend(self.cover_item(&item)?);
        }
        if let Some(item) = package.spine.first().and_then(|id| package.items.get(id)) {
            candidates.extend(self.cover_item(item)?);
        }
        Ok(candidates)
    }

    fn cover_item(&mut self, item: &Item) -> Result<Option<EpubImage>, FormatError> {
        if let Some(image) = self.image(&item.path, Some(&item.media)) {
            return Ok(Some(image));
        }
        if !item.is_text() || !self.has(&item.path) {
            return Ok(None);
        }
        let data = self.read(&item.path, MAX_CHAPTER_BYTES)?;
        let html = tokenize(&decode(&data).0, Html::body());
        if !html.blocks.is_empty() {
            // The spine fallback should not turn a regular chapter illustration into a cover.
            return Ok(None);
        }
        Ok(html.images.iter().find_map(|image| {
            let path = resolve_href(directory(&item.path), &image.href)?;
            self.image(&path, None)
        }))
    }
}

fn xml_doc(text: &str) -> Result<Document<'_>, roxmltree::Error> {
    Document::parse_with_options(
        text.trim_start_matches('\u{feff}'),
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
        },
    )
}

#[derive(Default)]
struct Package {
    title: Option<String>,
    author: Option<String>,
    items: HashMap<String, Item>,
    manifest_order: Vec<String>,
    spine: Vec<String>,
    cover_id: Option<String>,
    guide_cover: Option<String>,
}

struct Item {
    path: String,
    media: String,
    properties: String,
}

impl Item {
    fn is_text(&self) -> bool {
        matches!(
            self.media.as_str(),
            "text/html" | "application/xhtml+xml" | "application/x-dtbook+xml"
        ) || self.path.rsplit_once('.').is_some_and(|(_, ext)| {
            matches!(ext.to_ascii_lowercase().as_str(), "xhtml" | "html" | "htm")
        })
    }
}

fn parse_package(path: &str, data: &[u8]) -> Result<Package, FormatError> {
    let xml = decode(data).0;
    let doc = xml_doc(&xml)?;
    let mut package = Package::default();
    for node in doc.descendants().filter(|node| node.is_element()) {
        let parent = node
            .parent()
            .filter(|n| n.is_element())
            .map(|n| n.tag_name().name());
        match (parent, node.tag_name().name()) {
            (Some("metadata"), "title") => {
                if let Some(title) = node.text().map(clean).filter(|title| !title.is_empty()) {
                    package.title.get_or_insert(title);
                }
            }
            (Some("metadata"), "creator") => {
                if let Some(author) = node.text().map(clean).filter(|author| !author.is_empty()) {
                    package.author.get_or_insert(author);
                }
            }
            (Some("metadata"), "meta") if node.attribute("name") == Some("cover") => {
                if let Some(id) = node.attribute("content") {
                    package.cover_id.get_or_insert(id.to_owned());
                }
            }
            (Some("manifest"), "item") => {
                if let (Some(id), Some(href)) = (node.attribute("id"), node.attribute("href")) {
                    if let Some(path) = resolve_href(directory(path), href) {
                        let item = Item {
                            path,
                            media: node
                                .attribute("media-type")
                                .unwrap_or("")
                                .to_ascii_lowercase(),
                            properties: node.attribute("properties").unwrap_or("").to_owned(),
                        };
                        if package.items.insert(id.to_owned(), item).is_some() {
                            return Err(FormatError::InvalidArchive("EPUB manifest 包含重复 ID"));
                        }
                        package.manifest_order.push(id.to_owned());
                    }
                }
            }
            (Some("spine"), "itemref") => {
                if let Some(id) = node.attribute("idref") {
                    package.spine.push(id.to_owned());
                }
            }
            (Some("guide"), "reference") if node.attribute("type") == Some("cover") => {
                package.guide_cover = node
                    .attribute("href")
                    .and_then(|href| resolve_href(directory(path), href));
            }
            _ => {}
        }
    }
    Ok(package)
}

fn supported_media(value: &str) -> Option<&'static str> {
    match value {
        "image/jpeg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn resolve_href(base: &str, href: &str) -> Option<String> {
    let href = href.split('#').next()?.trim();
    let href = percent_decode_str(href).decode_utf8().ok()?;
    if href.is_empty() || href.starts_with('/') || href.contains([':', '?', '\\', '\0']) {
        return None;
    }
    let mut components = base
        .split('/')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    for part in href.split('/') {
        match part {
            ".." => {
                components.pop()?;
            }
            "." | "" => {}
            _ => components.push(part),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn is_front_matter(path: &str, title: &str, first: Option<&String>) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    let stem = basename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(basename)
        .to_ascii_lowercase();
    matches!(title, "书籍信息" | "图书信息" | "版权信息")
        || matches!(
            stem.as_str(),
            "cover"
                | "coverpage"
                | "cover-page"
                | "info"
                | "bookinfo"
                | "book-info"
                | "titlepage"
                | "title-page"
                | "toc"
                | "nav"
                | "contents"
                | "copyright"
        )
        || first.is_some_and(|p| {
            ["书籍信息", "图书信息", "版权信息"]
                .iter()
                .any(|prefix| p.starts_with(prefix))
                || ["书名", "作者"].iter().any(|prefix| {
                    p.strip_prefix(prefix)
                        .is_some_and(|rest| rest.trim_start().starts_with(['：', ':']))
                })
        })
}

fn clean(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Html {
    buf: String,
    blocks: Vec<(bool, String)>,
    images: Vec<ImageReference>,
    anchors: Vec<(String, String)>,
    anchor: Option<(String, String)>,
    in_body: bool,
    heading: bool,
    skip_depth: usize,
    nav: bool,
}

struct ImageReference {
    before: usize,
    href: String,
    alt: String,
}

impl Html {
    fn body() -> Self {
        Self::default()
    }

    fn nav() -> Self {
        Self {
            nav: true,
            ..Self::default()
        }
    }

    fn flush(&mut self) {
        let block = clean(&self.buf);
        self.buf.clear();
        let heading = std::mem::take(&mut self.heading);
        if !block.is_empty() {
            self.blocks.push((heading, block));
        }
    }

    fn finish_anchor(&mut self) {
        if let Some((href, text)) = self.anchor.take() {
            let text = clean(&text);
            if !href.is_empty() && !text.is_empty() {
                self.anchors.push((href, text));
            }
        }
    }

    fn tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        let name = tag.name.as_ref();
        let start = tag.kind == TagKind::StartTag;
        if matches!(
            name,
            "script" | "style" | "noscript" | "svg" | "math" | "template" | "iframe" | "object"
        ) {
            if start && !tag.self_closing {
                self.skip_depth += 1;
                match name {
                    "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
                    "style" | "iframe" => return TokenSinkResult::RawData(RawKind::Rawtext),
                    _ => {}
                }
            } else if !start {
                self.skip_depth = self.skip_depth.saturating_sub(1);
            }
            return TokenSinkResult::Continue;
        }
        if self.skip_depth > 0 {
            return TokenSinkResult::Continue;
        }
        let attr = |name| {
            tag.attrs
                .iter()
                .find(|attr| attr.name.local.as_ref() == name)
                .map(|attr| attr.value.as_ref())
                .unwrap_or("")
        };
        if name == "a" && self.nav {
            self.finish_anchor();
            if start && !tag.self_closing {
                self.anchor = Some((attr("href").to_owned(), String::new()));
            }
        } else if name == "body" {
            self.flush();
            self.in_body = start;
        } else if self.in_body && !self.nav {
            if start && name == "img" {
                let before = self.blocks.len() + usize::from(!self.buf.trim().is_empty());
                let alt = clean(attr("alt")).chars().take(300).collect();
                self.images.push(ImageReference {
                    before,
                    href: attr("src").to_owned(),
                    alt,
                });
            } else if is_block(name) {
                self.flush();
                self.heading = start && !tag.self_closing && matches!(name, "h1" | "h2" | "h3");
            }
        }
        TokenSinkResult::Continue
    }

    fn characters(&mut self, value: &str) {
        if self.skip_depth == 0 {
            if let Some((_, text)) = self.anchor.as_mut() {
                text.push_str(value);
            } else if self.in_body && !self.nav {
                self.buf.push_str(value);
            }
        }
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "dl"
            | "dt"
            | "dd"
            | "blockquote"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "hr"
            | "pre"
            | "figure"
            | "figcaption"
            | "center"
            | "address"
    )
}

struct Sink(RefCell<Html>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut state = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => return state.tag(tag),
            Token::CharacterTokens(text) => state.characters(&text),
            Token::NullCharacterToken => state.characters("\u{fffd}"),
            Token::EOFToken => {
                state.flush();
                state.finish_anchor();
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn tokenize(text: &str, state: Html) -> Html {
    let queue = BufferQueue::default();
    queue.push_back(text.into());
    let tokenizer = Tokenizer::new(Sink(RefCell::new(state)), TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}
