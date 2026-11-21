//! Format readers that treat input files as data and never modify the source.

mod archive;
mod cbz;
mod epub;
mod txt;

pub use cbz::{CbzPage, CbzReader};
pub use epub::{parse_epub, read_epub, EpubIllustration, EpubImage, EpubReader, ParsedEpub};
pub use txt::{
    parse_txt, parse_txt_detailed, read_txt, read_txt_detailed, ParsedTxt, MAX_TXT_BYTES,
};
pub use vm_core::{ImportedChapter, ImportedText, TxtEncoding};

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("TXT 文件超过 64 MiB 上限")]
    TooLarge,
    #[error("文件内容为空（解码后无有效文本）")]
    Empty,
    #[error("无法读取文件：{0}")]
    Io(#[from] std::io::Error),
    #[error("压缩包超出限制：{0}")]
    ArchiveLimit(&'static str),
    #[error("无效的压缩包：{0}")]
    InvalidArchive(&'static str),
    #[error("无法读取压缩包：{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("漫画页不存在：{0}")]
    PageOutOfRange(usize),
    #[error("EPUB 中缺少 {0}")]
    MissingEpubEntry(&'static str),
    #[error("EPUB XML 无效：{0}")]
    EpubXml(#[from] roxmltree::Error),
    #[error("EPUB 插图不存在：{0}")]
    MissingImage(String),
}
