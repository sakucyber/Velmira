//! The Android 0.7 offline library stored normalized chapters and records as JSON.

use std::{
    cmp::Ordering,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use vm_core::Loc;
use vm_format::{CbzReader, FormatError};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

use crate::{comics, database, Error, Result};

use super::{
    has_table, insert_bookmark, insert_progress, legacy_file, parse_loc, safe_file_stem,
    text_identity, MigrationReport,
};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COMIC_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PAGES: u32 = 2_000;

#[derive(Deserialize)]
struct Book {
    title: String,
    author: Option<String>,
    format: String,
    encoding: Option<String>,
    created_at: i64,
    chapter_count: u32,
}

#[derive(Deserialize)]
struct Chapter {
    title: String,
    paragraphs: Vec<String>,
}

#[derive(Deserialize)]
struct Position {
    chapter: i64,
    para: i64,
    offset: i64,
}

impl Position {
    fn loc(&self) -> Option<Loc> {
        parse_loc(self.chapter, self.para, self.offset)
    }
}

#[derive(Deserialize)]
struct Progress {
    #[serde(flatten)]
    position: Position,
    updated_at: i64,
}

#[derive(Deserialize)]
struct Bookmark {
    #[serde(flatten)]
    position: Position,
    label: Option<String>,
    created_at: i64,
}

#[derive(Deserialize)]
struct Annotation {
    chapter: i64,
    para: i64,
    start_offset: i64,
    end_offset: u32,
    text: String,
    note: Option<String>,
    color: String,
    created_at: i64,
    updated_at: i64,
}

pub(super) fn migrate(
    old: &Connection,
    new: &Connection,
    blobs: &Path,
    library: &Path,
    source_key: &str,
) -> Result<MigrationReport> {
    let mut books = old.prepare(
        "SELECT id, sha, CASE WHEN length(CAST(json AS BLOB)) <= 65536 THEN json END, archived
         FROM books ORDER BY id",
    )?;
    let mut rows = books.query([])?;
    let mut report = MigrationReport::default();
    let has_media = has_table(old, "media")?;
    while let Some(row) = rows.next()? {
        let old_id = database::text_column(row, 0)?;
        if new
            .prepare_cached("SELECT 1 FROM legacy_book WHERE source = ?1 AND legacy_id = ?2")?
            .query_row(params![source_key, old_id], |_| Ok(()))
            .optional()?
            .is_some()
        {
            continue;
        }
        let fingerprint = database::text_column(row, 1)?;
        let metadata: Book = read_json(row.get_ref(2)?.as_str().ok(), "book metadata")?;
        if !matches!(metadata.format.as_str(), "txt" | "epub" | "cbz")
            || metadata.chapter_count == 0
        {
            report.skipped_books += 1;
            continue;
        }
        if metadata.chapter_count > 100_000 {
            return Err(Error::Invalid(
                "legacy Android chapter count exceeds limit".into(),
            ));
        }
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Invalid("invalid Android book fingerprint".into()));
        }
        let fingerprint = fingerprint.to_ascii_lowercase();
        let archived = row.get::<_, Option<i64>>(3)?.is_some();
        let status = if archived { "removed" } else { "active" };
        let book_id = if metadata.format == "cbz" {
            let count = metadata.chapter_count;
            if count > MAX_PAGES || !safe_file_stem(old_id) || !old_id.starts_with("b_") {
                report.skipped_books += 1;
                continue;
            }
            let book_dir = book_directory(library, old_id);
            let source = book_dir
                .as_deref()
                .and_then(|dir| open_backup_file(dir, "source"))
                .or_else(|| legacy_file(library, old_id, "cbz", None).map(|(_, file)| file));
            let media = if has_media {
                book_dir.as_deref().map(|dir| (old, old_id, dir))
            } else {
                None
            };
            let archive = match recover_comic(source, media, &fingerprint, count) {
                Ok(archive) => archive,
                Err(Error::Invalid(_) | Error::Format(_)) => {
                    report.skipped_books += 1;
                    report.pending_comics += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut comic =
                match comics::prepare(new, blobs, archive.reopen()?, archive.path(), None) {
                    Ok(comic) => comic,
                    Err(Error::Invalid(_) | Error::Format(_)) => {
                        report.skipped_books += 1;
                        report.pending_comics += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            comic.title = metadata.title;
            comic.author = metadata.author;
            report.books += u32::from(comic.insert(new, Some(metadata.created_at), status)?);
            comic.id
        } else {
            // Keep Android's chapter and paragraph coordinates separate from desktop imports.
            let book_id = text_identity(&metadata.format, &format!("android:{fingerprint}"));
            let inserted = new.execute(
                "INSERT INTO book(id, title, author, format, encoding, chapter_count, added_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO NOTHING",
                params![
                    book_id, metadata.title, metadata.author, metadata.format, metadata.encoding,
                    metadata.chapter_count, metadata.created_at, status
                ],
            )? != 0;
            if inserted {
                copy_chapters(old, new, old_id, &book_id, metadata.chapter_count)?;
                report.books += 1;
            } else if !chapters_match(old, new, old_id, &book_id, metadata.chapter_count)? {
                report.skipped_books += 1;
                continue;
            }
            book_id
        };
        new.execute(
            "INSERT INTO legacy_book(source, legacy_id, book_id) VALUES (?1, ?2, ?3)",
            params![source_key, old_id, book_id],
        )?;
        copy_records(old, new, old_id, &book_id, &mut report)?;
    }
    Ok(report)
}

fn book_directory(library: &Path, id: &str) -> Option<PathBuf> {
    let backup_root = match library.file_name().and_then(|name| name.to_str()) {
        Some("databases" | "db") => library.parent(),
        _ => None,
    };
    [Some(library), backup_root]
        .into_iter()
        .flatten()
        .find_map(|root| {
            ["files/local-books", "local-books", "f/local-books"]
                .into_iter()
                .find_map(|subdir| {
                    let book = fs::canonicalize(root.join(subdir).join(id)).ok()?;
                    (book.starts_with(root) && book.is_dir()).then_some(book)
                })
        })
}

fn open_backup_file(directory: &Path, name: &str) -> Option<File> {
    let path = fs::canonicalize(directory.join(name)).ok()?;
    if !path.starts_with(directory) || !path.is_file() {
        return None;
    }
    File::open(path).ok()
}

fn recover_comic(
    source: Option<File>,
    media: Option<(&Connection, &str, &Path)>,
    fingerprint: &str,
    count: u32,
) -> Result<NamedTempFile> {
    if let Some((old, id, directory)) = media {
        match comic_from_media(old, id, directory, fingerprint, count) {
            Ok(archive) => return Ok(archive),
            Err(Error::Invalid(_) | Error::Format(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let source = source.ok_or_else(|| unavailable("Android comic files are missing"))?;
    comic_from_source(source, fingerprint, count)
}

fn comic_from_media(
    old: &Connection,
    id: &str,
    directory: &Path,
    fingerprint: &str,
    count: u32,
) -> Result<NamedTempFile> {
    let mut stmt = old.prepare_cached(
        "SELECT id, type, file FROM media
         WHERE book = ?1 AND chapter = -1 ORDER BY CAST(id AS INTEGER) LIMIT ?2",
    )?;
    let mut rows = stmt.query(params![id, count + 1])?;
    let archive = build_comic(fingerprint, count, |index| {
        let row = rows
            .next()?
            .ok_or_else(|| unavailable("Android comic page is missing from media"))?;
        let file_name = format!("image_{index}");
        if database::text_column(row, 0)? != index.to_string()
            || database::text_column(row, 2)? != file_name
        {
            return Err(unavailable("Android comic media sequence is invalid"));
        }
        let file = open_backup_file(directory, &file_name)
            .ok_or_else(|| unavailable("Android comic page file is missing"))?;
        let bytes = read_limited(file, MAX_PAGE_BYTES)?;
        let extension = image_extension(&bytes)?;
        let media_type = database::text_column(row, 1)?;
        if media_type.strip_prefix("image/") != Some(extension) {
            return Err(unavailable(
                "Android comic media type differs from its file",
            ));
        }
        Ok(bytes)
    })?;
    if rows.next()?.is_some() {
        return Err(unavailable("Android comic has extra media rows"));
    }
    Ok(archive)
}

fn comic_from_source(source: File, fingerprint: &str, count: u32) -> Result<NamedTempFile> {
    let bytes = read_limited(source, MAX_SOURCE_BYTES)?;
    if format!("{:x}", Sha256::digest(&bytes)) != fingerprint {
        return Err(unavailable("Android comic source SHA-256 differs"));
    }

    // Use the native reader's path, entry-count, and size checks before
    // following the original central directory order for Android sort ties.
    CbzReader::new(Cursor::new(&bytes), "")?;
    let mut archive = ZipArchive::new(Cursor::new(&bytes)).map_err(FormatError::from)?;
    let mut pages = Vec::with_capacity(count as usize);
    for index in 0..archive.len() {
        let file = archive.by_index_raw(index).map_err(FormatError::from)?;
        let path = file.name();
        if file.is_dir()
            || path.starts_with("__MACOSX/")
            || path.starts_with('.')
            || path.contains("/.")
        {
            continue;
        }
        let Some((_, extension)) = path.rsplit_once('.') else {
            continue;
        };
        if matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpeg" | "jpg" | "png" | "gif" | "webp"
        ) {
            if pages.len() == count as usize {
                return Err(unavailable("Android comic archive page count differs"));
            }
            AndroidPage::new(index, path)?.insert_in_order(&mut pages)?;
        }
    }
    if pages.len() != count as usize {
        return Err(unavailable("Android comic archive page count differs"));
    }

    build_comic(fingerprint, count, |index| {
        let page = archive
            .by_index(pages[index as usize].archive_index)
            .map_err(FormatError::from)?;
        let expected = page.size();
        let bytes = read_limited(page, MAX_PAGE_BYTES)?;
        if bytes.len() as u64 != expected {
            return Err(unavailable(
                "Android comic page size differs from its archive",
            ));
        }
        Ok(bytes)
    })
}

fn build_comic(
    fingerprint: &str,
    count: u32,
    mut next_page: impl FnMut(u32) -> Result<Vec<u8>>,
) -> Result<NamedTempFile> {
    let mut output = NamedTempFile::new()?;
    let mut archive = ZipWriter::new(output.as_file_mut());
    archive.set_comment(format!("Velmira Android v0.7 source SHA-256 {fingerprint}"));
    let options = SimpleFileOptions::default()
        .last_modified_time(zip::DateTime::default())
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    let mut total = 0;
    for index in 0..count {
        let bytes = next_page(index)?;
        total += bytes.len() as u64;
        if total > MAX_COMIC_BYTES {
            return Err(unavailable("Android comic pages exceed 512 MiB"));
        }
        let extension = image_extension(&bytes)?;
        archive
            .start_file(format!("{index:04}.{extension}"), options)
            .map_err(FormatError::from)?;
        archive.write_all(&bytes)?;
    }
    archive.finish().map_err(FormatError::from)?;
    Ok(output)
}

fn read_limited(input: impl Read, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(FormatError::from)?;
    if bytes.len() as u64 > max {
        return Err(unavailable("Android comic file exceeds its size limit"));
    }
    Ok(bytes)
}

fn image_extension(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok("webp")
    } else {
        Err(unavailable("unknown Android comic page image type"))
    }
}

fn unavailable(message: &str) -> Error {
    Error::Invalid(message.into())
}

struct AndroidPage {
    archive_index: usize,
    path: Vec<u16>,
    basename_offset: usize,
}

impl AndroidPage {
    fn new(archive_index: usize, path: &str) -> Result<Self> {
        let path: Vec<_> = path.encode_utf16().collect();
        if path.len() > 2048 {
            return Err(unavailable("Android comic path exceeds its size limit"));
        }
        let basename_offset = path
            .iter()
            .rposition(|unit| *unit == u16::from(b'/'))
            .map_or(0, |slash| slash + 1);
        Ok(Self {
            archive_index,
            path,
            basename_offset,
        })
    }

    fn compare(&self, other: &Self) -> Ordering {
        android_natural_cmp(
            &self.path[self.basename_offset..],
            &other.path[other.basename_offset..],
        )
        .then_with(|| android_natural_cmp(&self.path, &other.path))
    }

    fn insert_in_order(self, pages: &mut Vec<Self>) -> Result<()> {
        // The old comparator can be inconsistent for some combinations of
        // names. Check the ordering without depending on sort to accept it.
        let mut insertion = None;
        for (index, page) in pages.iter().enumerate() {
            if self.compare(page).is_lt() {
                insertion.get_or_insert(index);
            } else if insertion.is_some() {
                return Err(unavailable("Android comic source page order is ambiguous"));
            }
        }
        pages.insert(insertion.unwrap_or(pages.len()), self);
        Ok(())
    }
}

fn android_natural_cmp(a: &[u16], b: &[u16]) -> Ordering {
    // Android compares Java UTF-16 chars and uses the full original length
    // to break numeric ties, including otherwise equivalent zero padding.
    let mut a_rest = a;
    let mut b_rest = b;
    while let (Some(&a_first), Some(&b_first)) = (a_rest.first(), b_rest.first()) {
        let order = if java_digit(a_first) && java_digit(b_first) {
            let (a_number, a_tail) =
                a_rest.split_at(a_rest.iter().take_while(|&&c| java_digit(c)).count());
            let (b_number, b_tail) =
                b_rest.split_at(b_rest.iter().take_while(|&&c| java_digit(c)).count());
            a_rest = a_tail;
            b_rest = b_tail;
            let a_number = trim_zeroes(a_number);
            let b_number = trim_zeroes(b_number);
            a_number
                .len()
                .cmp(&b_number.len())
                .then_with(|| a_number.cmp(b_number))
        } else {
            a_rest = &a_rest[1..];
            b_rest = &b_rest[1..];
            java_lower(a_first).cmp(&java_lower(b_first))
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

fn java_digit(unit: u16) -> bool {
    matches!(unit,
        0x0030..=0x0039 | 0x0660..=0x0669 | 0x06f0..=0x06f9 | 0x07c0..=0x07c9 |
        0x0966..=0x096f | 0x09e6..=0x09ef | 0x0a66..=0x0a6f | 0x0ae6..=0x0aef |
        0x0b66..=0x0b6f | 0x0be6..=0x0bef | 0x0c66..=0x0c6f | 0x0ce6..=0x0cef |
        0x0d66..=0x0d6f | 0x0de6..=0x0def | 0x0e50..=0x0e59 | 0x0ed0..=0x0ed9 |
        0x0f20..=0x0f29 | 0x1040..=0x1049 | 0x1090..=0x1099 | 0x17e0..=0x17e9 |
        0x1810..=0x1819 | 0x1946..=0x194f | 0x19d0..=0x19d9 | 0x1a80..=0x1a89 |
        0x1a90..=0x1a99 | 0x1b50..=0x1b59 | 0x1bb0..=0x1bb9 | 0x1c40..=0x1c49 |
        0x1c50..=0x1c59 | 0xa620..=0xa629 | 0xa8d0..=0xa8d9 | 0xa900..=0xa909 |
        0xa9d0..=0xa9d9 | 0xa9f0..=0xa9f9 | 0xaa50..=0xaa59 | 0xabf0..=0xabf9 |
        0xff10..=0xff19)
}

fn java_lower(unit: u16) -> u16 {
    char::from_u32(u32::from(unit))
        .and_then(|value| value.to_lowercase().next())
        .and_then(|value| u16::try_from(u32::from(value)).ok())
        .unwrap_or(unit)
}

fn trim_zeroes(number: &[u16]) -> &[u16] {
    let zero = u16::from(b'0');
    let skip = number.iter().take_while(|&&unit| unit == zero).count();
    &number[skip.min(number.len() - 1)..]
}

fn read_json<T: serde::de::DeserializeOwned>(raw: Option<&str>, section: &str) -> Result<T> {
    let text =
        raw.ok_or_else(|| Error::Invalid(format!("invalid Android {section} size or type")))?;
    Ok(serde_json::from_str(text)?)
}

fn each_chapter(
    old: &Connection,
    old_id: &str,
    count: u32,
    mut visit: impl FnMut(u32, Chapter) -> Result<()>,
) -> Result<()> {
    let mut stmt = old.prepare_cached(
        "SELECT idx, CASE WHEN length(CAST(json AS BLOB)) <= 67108864 THEN json END
         FROM chapters WHERE book = ?1 ORDER BY idx",
    )?;
    let mut rows = stmt.query([old_id])?;
    let mut seen = 0;
    while let Some(row) = rows.next()? {
        let idx = row.get::<_, u32>(0)?;
        if idx != seen || idx >= count {
            return Err(Error::Invalid(
                "legacy Android chapter indices are not contiguous".into(),
            ));
        }
        let chapter = read_json(row.get_ref(1)?.as_str().ok(), "chapter")?;
        visit(idx, chapter)?;
        seen += 1;
    }
    if seen != count {
        return Err(Error::Invalid(
            "legacy Android chapter count does not match the book".into(),
        ));
    }
    Ok(())
}

fn copy_chapters(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    count: u32,
) -> Result<()> {
    each_chapter(old, old_id, count, |index, chapter| {
        database::insert_chapter(new, book_id, index, &chapter.title, &chapter.paragraphs)
    })
}

fn chapters_match(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    count: u32,
) -> Result<bool> {
    if database::summary(new, book_id)?.chapter_count != count {
        return Ok(false);
    }
    let mut matches = true;
    each_chapter(old, old_id, count, |index, chapter| {
        matches &= database::chapter(new, book_id, index)?.paragraphs == chapter.paragraphs;
        Ok(())
    })?;
    Ok(matches)
}

fn copy_records(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    report: &mut MigrationReport,
) -> Result<()> {
    let mut stmt = old.prepare_cached(
        "SELECT kind, id, CASE WHEN length(CAST(json AS BLOB)) <= 131072 THEN json END
         FROM records WHERE book = ?1 AND kind IN ('progress', 'bookmark', 'annotation')
         ORDER BY kind, id",
    )?;
    let mut rows = stmt.query([old_id])?;
    while let Some(row) = rows.next()? {
        let value = row.get_ref(2)?;
        let Ok(raw) = value.as_str() else {
            report.skipped_positions += 1;
            continue;
        };
        let persisted = match database::text_column(row, 0)? {
            "progress" => serde_json::from_str::<Progress>(raw).ok().map(|item| {
                insert_progress(new, book_id, item.position.loc(), item.updated_at, report)
            }),
            "bookmark" => serde_json::from_str::<Bookmark>(raw).ok().map(|item| {
                insert_bookmark(
                    new,
                    book_id,
                    item.position.loc(),
                    item.label.as_deref(),
                    item.created_at,
                    report,
                )
            }),
            "annotation" => {
                let id = database::text_column(row, 1)?;
                serde_json::from_str::<Annotation>(raw)
                    .ok()
                    .map(|item| copy_annotation(new, book_id, id, item, report))
            }
            _ => unreachable!("SQL filters the record type"),
        };
        if let Some(result) = persisted {
            result?;
        } else {
            report.skipped_positions += 1;
        }
    }
    Ok(())
}

fn copy_annotation(
    db: &Connection,
    book_id: &str,
    record_id: &str,
    item: Annotation,
    report: &mut MigrationReport,
) -> Result<()> {
    let Some(loc) = parse_loc(item.chapter, item.para, item.start_offset) else {
        report.skipped_positions += 1;
        return Ok(());
    };
    if item.end_offset <= loc.off
        || database::paragraph_len(db, book_id, &loc)?.is_none_or(|len| item.end_offset > len)
        || !matches!(item.color.as_str(), "yellow" | "green" | "blue" | "pink")
    {
        report.skipped_positions += 1;
        return Ok(());
    }
    let chapter = database::chapter(db, book_id, loc.chapter)?;
    let selection = chapter.paragraphs[loc.para as usize]
        .chars()
        .skip(loc.off as usize)
        .take((item.end_offset - loc.off) as usize);
    if !selection.eq(item.text.chars()) {
        report.skipped_positions += 1;
        return Ok(());
    }
    report.annotations += db.execute(
        "INSERT INTO annotation(id, book_id, chapter, para, start_off, end_off, text, note, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(id) DO NOTHING",
        params![format!("{book_id}:android:{record_id}"), book_id, loc.chapter, loc.para, loc.off,
            item.end_offset, item.text, item.note, item.color, item.created_at, item.updated_at],
    )? as u32;
    Ok(())
}
