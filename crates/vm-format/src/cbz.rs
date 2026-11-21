use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use zip::ZipArchive;

use crate::archive::{
    check_raster_signature, image_media_type, open_archive, read_member, MAX_IMAGE_BYTES,
};
use crate::txt::title_author_from_filename;
use crate::FormatError;

const MAX_CBZ_PAGES: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzPage {
    pub path: String,
    pub byte_size: u64,
    pub media_type: &'static str,
    member_index: usize,
}

/// An indexed comic. Pages are decompressed on demand and are not extracted to disk.
pub struct CbzReader<R> {
    pub title: String,
    pub author: Option<String>,
    pages: Vec<CbzPage>,
    archive: ZipArchive<R>,
}

impl CbzReader<File> {
    pub fn open(path: &Path) -> Result<Self, FormatError> {
        let file_name = path.file_name().map(|name| name.to_string_lossy());
        Self::new(File::open(path)?, file_name.as_deref().unwrap_or_default())
    }
}

impl<R: Read + Seek> CbzReader<R> {
    pub fn new(reader: R, file_name: &str) -> Result<Self, FormatError> {
        let mut archive = open_archive(reader)?;
        let mut pages = Vec::new();
        for member_index in 0..archive.len() {
            let entry = archive.by_index_raw(member_index)?;
            let path = entry.name();
            if entry.is_dir() || path.starts_with("__MACOSX/") || basename(path).starts_with('.') {
                continue;
            }
            let Some(media_type) = image_media_type(path) else {
                continue;
            };
            let byte_size = entry.size();
            if byte_size > MAX_IMAGE_BYTES {
                return Err(FormatError::ArchiveLimit("单页解压后超过 32 MiB"));
            }
            if pages.len() == MAX_CBZ_PAGES {
                return Err(FormatError::ArchiveLimit("漫画页数超过 2000"));
            }
            pages.push(CbzPage {
                path: path.to_owned(),
                byte_size,
                media_type,
                member_index,
            });
        }
        if pages.is_empty() {
            return Err(FormatError::InvalidArchive("CBZ 中没有支持的图片"));
        }
        pages.sort_by(|a, b| {
            natural_cmp(basename(&a.path), basename(&b.path))
                .then_with(|| natural_cmp(&a.path, &b.path))
        });
        let (title, author) = title_author_from_filename(file_name);
        Ok(Self {
            title: title.to_owned(),
            author: author.map(str::to_owned),
            pages,
            archive,
        })
    }

    pub fn pages(&self) -> &[CbzPage] {
        &self.pages
    }

    pub fn read_page(&mut self, index: usize) -> Result<Vec<u8>, FormatError> {
        let page = self
            .pages
            .get(index)
            .ok_or(FormatError::PageOutOfRange(index))?;
        let bytes = read_member(self.archive.by_index(page.member_index)?, MAX_IMAGE_BYTES)?;
        check_raster_signature(&bytes, page.media_type)?;
        Ok(bytes)
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    // Compare numeric runs by length and then lexical order instead of parsing into a fixed integer.
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let mut a = a_lower.as_bytes();
    let mut b = b_lower.as_bytes();
    while !a.is_empty() && !b.is_empty() {
        let digits_a = a[0].is_ascii_digit();
        let digits_b = b[0].is_ascii_digit();
        if digits_a != digits_b {
            return digits_b.cmp(&digits_a);
        }
        let len_a = a
            .iter()
            .take_while(|value| value.is_ascii_digit() == digits_a)
            .count();
        let len_b = b
            .iter()
            .take_while(|value| value.is_ascii_digit() == digits_b)
            .count();
        let (chunk_a, rest_a) = a.split_at(len_a);
        let (chunk_b, rest_b) = b.split_at(len_b);
        let order = if digits_a {
            let chunk_a = drop_leading_zeroes(chunk_a);
            let chunk_b = drop_leading_zeroes(chunk_b);
            chunk_a
                .len()
                .cmp(&chunk_b.len())
                .then_with(|| chunk_a.cmp(chunk_b))
        } else {
            chunk_a.cmp(chunk_b)
        };
        if order != Ordering::Equal {
            return order;
        }
        a = rest_a;
        b = rest_b;
    }
    a.len().cmp(&b.len())
}

fn drop_leading_zeroes(bytes: &[u8]) -> &[u8] {
    let count = bytes.iter().take_while(|&&byte| byte == b'0').count();
    &bytes[count..]
}
