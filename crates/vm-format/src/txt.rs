use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use encoding_rs::{GB18030, UTF_16BE, UTF_16LE};
use regex::Regex;
use vm_core::{ImportedChapter, ImportedText, TxtEncoding};

use crate::FormatError;

pub const MAX_TXT_BYTES: usize = 64 * 1024 * 1024;
const FALLBACK_CHUNK_CHARS: usize = 3000;
const CHAPTER_TITLE_MAX_LEN: usize = 40;

/// The fields needed to compare Rust imports with the 0.7 importer.
#[derive(Debug)]
pub struct ParsedTxt {
    pub text: ImportedText,
    pub author: Option<String>,
    pub legacy_encoding: &'static str,
    pub encoding_degraded: bool,
    pub total_chars: usize,
    pub paragraph_count: usize,
}

pub fn parse_txt(bytes: &[u8], file_name: &str) -> Result<ImportedText, FormatError> {
    Ok(parse_txt_detailed(bytes, file_name)?.text)
}

pub fn read_txt(path: &Path) -> Result<ImportedText, FormatError> {
    Ok(read_txt_detailed(path)?.text)
}

pub fn read_txt_detailed(path: &Path) -> Result<ParsedTxt, FormatError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_TXT_BYTES as u64 {
        return Err(FormatError::TooLarge);
    }
    let mut bytes = Vec::new();
    // Check the actual bytes too, in case the file changed after the metadata read.
    file.take(MAX_TXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let file_name = path.file_name().map(|name| name.to_string_lossy());
    parse_txt_detailed(&bytes, file_name.as_deref().unwrap_or_default())
}

pub fn parse_txt_detailed(bytes: &[u8], file_name: &str) -> Result<ParsedTxt, FormatError> {
    if bytes.len() > MAX_TXT_BYTES {
        return Err(FormatError::TooLarge);
    }
    let (decoded, encoding, legacy_encoding, encoding_degraded) = decode(bytes);
    let text = normalize(&decoded);
    if trim(&text).is_empty() {
        return Err(FormatError::Empty);
    }

    let (fallback_title, fallback_author) = title_author_from_filename(file_name);
    let (title, author) = extract_meta(&text);
    let title = title.unwrap_or(fallback_title).to_owned();
    let author = author.or(fallback_author).map(str::to_owned);

    let chapters = if is_chaptered(&text) {
        split_chapters(&text)
    } else {
        split_fixed(&text)
    };
    Ok(ParsedTxt {
        paragraph_count: chapters
            .iter()
            .map(|chapter| chapter.paragraphs.len())
            .sum(),
        text: ImportedText {
            title,
            encoding,
            chapters,
        },
        author,
        legacy_encoding,
        encoding_degraded,
        total_chars: text.chars().count(),
    })
}

pub(crate) fn decode(bytes: &[u8]) -> (Cow<'_, str>, TxtEncoding, &'static str, bool) {
    // Use the legacy deterministic probe order so reimport preserves chunk and annotation offsets.
    if let Ok(text) = std::str::from_utf8(bytes) {
        let encoding = if bytes.starts_with(b"\xef\xbb\xbf") {
            "utf-8-sig"
        } else {
            "utf-8"
        };
        return (Cow::Borrowed(text), TxtEncoding::Utf8, encoding, false);
    }
    for (bom, decoder, encoding, name) in [
        ([0xff, 0xfe], UTF_16LE, TxtEncoding::Utf16Le, "utf-16-le"),
        ([0xfe, 0xff], UTF_16BE, TxtEncoding::Utf16Be, "utf-16-be"),
    ] {
        if let Some(text) = bytes
            .strip_prefix(&bom)
            .and_then(|body| decoder.decode_without_bom_handling_and_without_replacement(body))
        {
            return (text, encoding, name, false);
        }
    }
    if let Some(text) = decode_legacy_gb18030(bytes) {
        return (text, TxtEncoding::Gb18030, "gb18030", false);
    }
    (
        String::from_utf8_lossy(bytes),
        TxtEncoding::Utf8,
        "utf-8",
        true,
    )
}

// Python 3.11 uses the original GB18030 mappings. Current encoding_rs follows the
// 2022 revision and also accepts the browser-compatible single-byte Euro. Apply
// only the raw-byte differences so neighboring GB18030 sequences remain unchanged.
fn decode_legacy_gb18030(bytes: &[u8]) -> Option<Cow<'_, str>> {
    let mut result = None::<String>;
    let (mut start, mut pos) = (0, 0);
    while pos < bytes.len() {
        let first = bytes[pos];
        if first == 0x80 {
            return None;
        }
        let (width, replacement) = if first.is_ascii() {
            (1, None)
        } else if (0x81..=0xfe).contains(&first) {
            let second = *bytes.get(pos + 1)?;
            match second {
                b'0'..=b'9' => {
                    let third = *bytes.get(pos + 2)?;
                    let fourth = *bytes.get(pos + 3)?;
                    if !(0x81..=0xfe).contains(&third) || !fourth.is_ascii_digit() {
                        return None;
                    }
                    let legacy = (first, second, third, fourth) == (0x81, 0x35, 0xf4, 0x37);
                    (4, legacy.then_some('\u{1e3f}'))
                }
                0x40..=0x7e | 0x80..=0xfe => (2, legacy_pair(first, second)),
                _ => return None,
            }
        } else {
            return None;
        };
        if let Some(replacement) = replacement {
            let unchanged =
                GB18030.decode_without_bom_handling_and_without_replacement(&bytes[start..pos])?;
            let output = result.get_or_insert_with(|| String::with_capacity(bytes.len()));
            output.push_str(&unchanged);
            output.push(replacement);
            start = pos + width;
        }
        pos += width;
    }
    let trailing = GB18030.decode_without_bom_handling_and_without_replacement(&bytes[start..])?;
    if let Some(mut result) = result {
        result.push_str(&trailing);
        Some(Cow::Owned(result))
    } else {
        Some(trailing)
    }
}

fn legacy_pair(first: u8, second: u8) -> Option<char> {
    Some(match u16::from_be_bytes([first, second]) {
        0xa3a0 => '\u{e5e5}',
        0xa6d9 => '\u{e78d}',
        0xa6da => '\u{e78e}',
        0xa6db => '\u{e78f}',
        0xa6dc => '\u{e790}',
        0xa6dd => '\u{e791}',
        0xa6de => '\u{e792}',
        0xa6df => '\u{e793}',
        0xa6ec => '\u{e794}',
        0xa6ed => '\u{e795}',
        0xa6f3 => '\u{e796}',
        0xa8bc => '\u{e7c7}',
        0xfe59 => '\u{e81e}',
        0xfe61 => '\u{e826}',
        0xfe66 => '\u{e82b}',
        0xfe67 => '\u{e82c}',
        0xfe6d => '\u{e832}',
        0xfe7e => '\u{e843}',
        0xfe90 => '\u{e854}',
        0xfea0 => '\u{e864}',
        _ => return None,
    })
}

fn normalize(text: &str) -> Cow<'_, str> {
    fn is_zero_width(ch: char) -> bool {
        matches!(ch, '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}')
    }

    if !text.contains(|ch| ch == '\r' || is_zero_width(ch)) {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            normalized.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else if !is_zero_width(ch) {
            normalized.push(ch);
        }
    }
    Cow::Owned(normalized)
}

fn python_whitespace(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}')
}

fn trim(text: &str) -> &str {
    text.trim_matches(python_whitespace)
}

static META: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(书名|作者)[\s\x1C-\x1F]*[：:]?[\s\x1C-\x1F]*(.*)$").unwrap());

fn extract_meta(text: &str) -> (Option<&str>, Option<&str>) {
    let (mut title, mut author) = (None, None);
    for line in text.split('\n').take(40).map(trim) {
        let Some(captures) = META.captures(line) else {
            continue;
        };
        let (_, [key, value]) = captures.extract();
        let value = trim(value);
        if value.is_empty() {
            continue;
        }
        if key == "书名" {
            title.get_or_insert(value);
        } else {
            author.get_or_insert(value);
        }
    }
    (title, author)
}

pub(crate) fn title_author_from_filename(file_name: &str) -> (&str, Option<&str>) {
    let base = file_name
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    // Keep leading-dot names and trailing dots, matching pathlib's suffix rules.
    let stem = match base.rsplit_once('.') {
        Some((left, right)) if !left.is_empty() && !right.is_empty() => left,
        _ => base,
    };
    let stem = if stem.is_empty() || stem == "." {
        "未命名"
    } else {
        stem
    };
    let (title, author) = match stem.split_once(" - ") {
        Some((left, right)) => {
            let left = trim(left);
            let right = trim(right);
            (
                if left.is_empty() { trim(stem) } else { left },
                Some(right).filter(|a| !a.is_empty()),
            )
        }
        None => (trim(stem), None),
    };
    (title, author)
}

static ARABIC_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^第[\s\x1C-\x1F]*[0-9]+[\s\x1C-\x1F]*[章回卷节]").unwrap());
static CHINESE_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^第[\s\x1C-\x1F]*[零一二三四五六七八九十百千万两]+[\s\x1C-\x1F]*[章回卷节]")
        .unwrap()
});
static ENGLISH_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    // Python's \w is alphanumeric + underscore, including numeric letters and symbols.
    Regex::new(r"(?i)^Chapter[\s\x1C-\x1F]+\d+").unwrap()
});
static SPECIAL_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:楔子|序章|序言|引子|前言|尾声|后记|番外(?:篇)?)(?:[\s\x1C-\x1F]*[^\s\x1C-\x1F]{1,24})?$").unwrap()
});
static NUMBERED_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{1,4}[\s\x1C-\x1F]*[、.．][\s\x1C-\x1F]*[^\s\x1C-\x1F]{1,30}$").unwrap()
});
static CHAPTER_PREAMBLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:【[^】]*】|[-=*—_]{3,})$").unwrap());

fn heading_delimiter(rest: &str) -> bool {
    rest.chars().next().is_none_or(|ch| {
        python_whitespace(ch)
            || matches!(ch, '：' | ':' | '、' | '，' | ',' | '．' | '.' | '-' | '—')
    })
}

fn heading(line: &str) -> Option<bool> {
    if line.is_empty() || line.chars().nth(CHAPTER_TITLE_MAX_LEN).is_some() {
        return None;
    }
    // Most prose cannot match any heading. Keep regex work off that path.
    let first = line.chars().next()?;
    let strong = match first {
        '第' => {
            ARABIC_HEADING
                .find(line)
                .is_some_and(|m| !m.as_str().ends_with('节') || heading_delimiter(&line[m.end()..]))
                || CHINESE_HEADING
                    .find(line)
                    .is_some_and(|m| heading_delimiter(&line[m.end()..]))
        }
        'c' | 'C' => ENGLISH_HEADING.find(line).is_some_and(|m| {
            line[m.end()..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
        }),
        '楔' | '序' | '引' | '前' | '尾' | '后' | '番' => SPECIAL_HEADING.is_match(line),
        _ => false,
    };
    if strong {
        Some(true)
    } else if first.is_numeric() && NUMBERED_HEADING.is_match(line) {
        Some(false)
    } else {
        None
    }
}

fn is_chaptered(text: &str) -> bool {
    text.split('\n')
        .filter_map(|line| heading(trim(line)))
        .take(3)
        .filter(|strong| *strong)
        .take(2)
        .count()
        == 2
}

fn push_paragraph(chapter: &mut ImportedChapter, line: &str) {
    if !line.is_empty() && !(chapter.paragraphs.is_empty() && CHAPTER_PREAMBLE.is_match(line)) {
        chapter.paragraphs.push(line.to_owned());
    }
}

fn split_chapters(text: &str) -> Vec<ImportedChapter> {
    let mut chapters: Vec<ImportedChapter> = Vec::new();
    for line in text.split('\n').map(trim) {
        if heading(line).is_some() {
            chapters.push(ImportedChapter {
                title: line.to_owned(),
                paragraphs: Vec::new(),
            });
        } else if let Some(chapter) = chapters.last_mut() {
            push_paragraph(chapter, line);
        }
    }
    chapters
}

fn split_fixed(mut remaining: &str) -> Vec<ImportedChapter> {
    let mut chapters = Vec::new();
    while !remaining.is_empty() {
        let mut end = remaining.len();
        let mut newline = None;
        for (index, (byte, ch)) in remaining.char_indices().enumerate() {
            if index == FALLBACK_CHUNK_CHARS {
                end = newline.unwrap_or(byte);
                break;
            }
            if index > FALLBACK_CHUNK_CHARS / 2 && ch == '\n' {
                newline = Some(byte);
            }
        }
        let (chunk, rest) = remaining.split_at(end);
        remaining = rest;
        let mut chapter = ImportedChapter {
            title: String::new(),
            paragraphs: Vec::new(),
        };
        for line in chunk.split('\n').map(trim) {
            push_paragraph(&mut chapter, line);
        }
        if !chapter.paragraphs.is_empty() {
            chapter.title = format!("第{}部分", chapters.len() + 1);
            chapters.push(chapter);
        }
    }
    chapters
}
