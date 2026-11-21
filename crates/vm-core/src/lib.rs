//! Shared data types for the native reader.

use serde::{Deserialize, Serialize};

pub type BookId = String;

/// A stable position in text. `off` counts Unicode scalar values, independent
/// of the font, pixel size and UTF-8 byte length.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Loc {
    pub chapter: u32,
    pub para: u32,
    pub off: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookFormat {
    Txt,
    Epub,
    Cbz,
}

impl BookFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Epub => "epub",
            Self::Cbz => "cbz",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxtEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gb18030,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedText {
    pub title: String,
    pub encoding: TxtEncoding,
    pub chapters: Vec<ImportedChapter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedChapter {
    pub title: String,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookSummary {
    pub id: BookId,
    pub title: String,
    pub chapter_count: u32,
    pub progress: Option<Loc>,
    pub format: BookFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub paragraphs: Vec<String>,
}
