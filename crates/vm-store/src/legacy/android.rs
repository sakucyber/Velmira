//! The Android 0.7 offline library stored normalized chapters and records as JSON.

use rusqlite::{params, Connection};
use serde::Deserialize;
use vm_core::Loc;

use crate::{database, Error, Result};

use super::{insert_bookmark, insert_progress, parse_loc, text_identity, MigrationReport};

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
    source_key: &str,
) -> Result<MigrationReport> {
    let mut books = old.prepare(
        "SELECT id, sha, CASE WHEN length(CAST(json AS BLOB)) <= 65536 THEN json END, archived
         FROM books ORDER BY id",
    )?;
    let mut rows = books.query([])?;
    let mut report = MigrationReport::default();
    while let Some(row) = rows.next()? {
        let old_id = database::text_column(row, 0)?;
        let fingerprint = database::text_column(row, 1)?;
        let metadata: Book = read_json(row.get_ref(2)?.as_str().ok(), "book metadata")?;
        let archived = row.get::<_, Option<i64>>(3)?.is_some();
        if !matches!(metadata.format.as_str(), "txt" | "epub") || metadata.chapter_count == 0 {
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
        // Android's parser can split the same source differently from the
        // desktop parser. Keep its reading positions in their own coordinate space.
        let book_id = text_identity(
            &metadata.format,
            &format!("android:{}", fingerprint.to_ascii_lowercase()),
        );
        let status = if archived { "removed" } else { "active" };
        let inserted = new.execute(
            "INSERT INTO book(id, title, author, format, encoding, chapter_count, added_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO NOTHING",
            params![
                book_id,
                metadata.title,
                metadata.author,
                metadata.format,
                metadata.encoding,
                metadata.chapter_count,
                metadata.created_at,
                status
            ],
        )? != 0;
        if inserted {
            copy_chapters(old, new, old_id, &book_id, metadata.chapter_count)?;
            report.books += 1;
        } else if !chapters_match(old, new, old_id, &book_id, metadata.chapter_count)? {
            report.skipped_books += 1;
            continue;
        }
        new.execute(
            "INSERT INTO legacy_book(source, legacy_id, book_id) VALUES (?1, ?2, ?3)",
            params![source_key, old_id, book_id],
        )?;
        copy_records(old, new, old_id, &book_id, &mut report)?;
    }
    Ok(report)
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
