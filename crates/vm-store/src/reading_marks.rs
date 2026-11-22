use rusqlite::{params, Connection, OptionalExtension, Row};
use vm_core::Loc;

use crate::{database, Error, Result};

const BOOKMARK_PAGE: u32 = 60;
const MAX_ANNOTATIONS_PER_CHAPTER: u32 = 2048;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_NOTE_BYTES: usize = 16 * 1024;
const MAX_SELECTION_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub loc: Loc,
    pub label: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationColor {
    Yellow,
    Green,
    Blue,
    Pink,
}

impl AnnotationColor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Pink => "pink",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub id: String,
    pub loc: Loc,
    pub end_off: u32,
    pub text: String,
    pub note: Option<String>,
    pub color: AnnotationColor,
    pub revision: u32,
}

pub(super) fn list_bookmarks(
    db: &Connection,
    book_id: &str,
    offset: u32,
) -> Result<(Vec<Bookmark>, bool)> {
    let mut stmt = db.prepare_cached(
        "SELECT chapter, para, off, label, created_at FROM bookmark
         WHERE book_id = ?1 ORDER BY created_at DESC, chapter, para, off LIMIT ?2 OFFSET ?3",
    )?;
    let mut bookmarks = stmt
        .query_map(params![book_id, BOOKMARK_PAGE + 1, offset], bookmark_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = bookmarks.len() > BOOKMARK_PAGE as usize;
    bookmarks.truncate(BOOKMARK_PAGE as usize);
    Ok((bookmarks, has_more))
}

fn bookmark_row(row: &Row<'_>) -> rusqlite::Result<Bookmark> {
    Ok(Bookmark {
        loc: Loc {
            chapter: row.get(0)?,
            para: row.get(1)?,
            off: row.get(2)?,
        },
        label: row.get(3)?,
        created_at: row.get(4)?,
    })
}

pub(super) fn add_bookmark(
    db: &mut Connection,
    book_id: &str,
    loc: Loc,
    label: Option<&str>,
) -> Result<Bookmark> {
    let label = checked_text(label, MAX_LABEL_BYTES, "bookmark label")?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if database::paragraph_len(&tx, book_id, &loc)?.is_none_or(|len| loc.off > len) {
        return Err(Error::Invalid("bookmark is outside a paragraph".into()));
    }
    tx.prepare_cached(
        "INSERT INTO bookmark(book_id, chapter, para, off, label)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (book_id, chapter, para, off) DO NOTHING",
    )?
    .execute(params![book_id, loc.chapter, loc.para, loc.off, label])?;
    let bookmark = tx
        .prepare_cached(
            "SELECT chapter, para, off, label, created_at FROM bookmark
         WHERE book_id = ?1 AND chapter = ?2 AND para = ?3 AND off = ?4",
        )?
        .query_row(
            params![book_id, loc.chapter, loc.para, loc.off],
            bookmark_row,
        )?;
    tx.commit()?;
    Ok(bookmark)
}

pub(super) fn delete_bookmark(db: &Connection, book_id: &str, loc: Loc) -> Result<()> {
    db.prepare_cached(
        "DELETE FROM bookmark WHERE book_id = ?1 AND chapter = ?2 AND para = ?3 AND off = ?4",
    )?
    .execute(params![book_id, loc.chapter, loc.para, loc.off])?;
    Ok(())
}

const ANNOTATION_COLUMNS: &str =
    "id, chapter, para, start_off, end_off, text, note, color, revision";

fn annotation_row(row: &Row<'_>) -> rusqlite::Result<Annotation> {
    let color = match database::text_column(row, 7)? {
        "yellow" => AnnotationColor::Yellow,
        "green" => AnnotationColor::Green,
        "blue" => AnnotationColor::Blue,
        "pink" => AnnotationColor::Pink,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(Annotation {
        id: row.get(0)?,
        loc: Loc {
            chapter: row.get(1)?,
            para: row.get(2)?,
            off: row.get(3)?,
        },
        end_off: row.get(4)?,
        text: row.get(5)?,
        note: row.get(6)?,
        color,
        revision: row.get(8)?,
    })
}

pub(super) fn list_annotations(
    db: &Connection,
    book_id: &str,
    chapter: u32,
) -> Result<Vec<Annotation>> {
    let sql = format!(
        "SELECT {ANNOTATION_COLUMNS} FROM annotation WHERE book_id = ?1 AND chapter = ?2
         ORDER BY para, start_off, id LIMIT ?3"
    );
    let mut stmt = db.prepare_cached(&sql)?;
    let annotations = stmt
        .query_map(
            params![book_id, chapter, MAX_ANNOTATIONS_PER_CHAPTER + 1],
            annotation_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if annotations.len() > MAX_ANNOTATIONS_PER_CHAPTER as usize {
        return Err(Error::Invalid(
            "chapter has too many annotations to display".into(),
        ));
    }
    Ok(annotations)
}

pub(super) fn add_annotation(
    db: &mut Connection,
    book_id: &str,
    loc: Loc,
    end_off: u32,
    note: Option<&str>,
    color: AnnotationColor,
) -> Result<Annotation> {
    let note = checked_text(note, MAX_NOTE_BYTES, "annotation note")?;
    if loc.off >= end_off {
        return Err(Error::Invalid(
            "annotation selection must not be empty".into(),
        ));
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let count = tx
        .prepare_cached("SELECT COUNT(*) FROM annotation WHERE book_id = ?1 AND chapter = ?2")?
        .query_row(params![book_id, loc.chapter], |row| row.get::<_, u32>(0))?;
    if count >= MAX_ANNOTATIONS_PER_CHAPTER {
        return Err(Error::Invalid(
            "chapter has reached its annotation limit".into(),
        ));
    }
    let paragraph = database::chapter(&tx, book_id, loc.chapter)?
        .paragraphs
        .into_iter()
        .nth(loc.para as usize)
        .ok_or_else(|| Error::Invalid("annotation is outside a paragraph".into()))?;
    let start = byte_at(&paragraph, loc.off)?;
    let end = byte_at(&paragraph, end_off)?;
    if end - start > MAX_SELECTION_BYTES {
        return Err(Error::Invalid("annotation selection is too long".into()));
    }
    let sql = format!(
        "INSERT INTO annotation(id, book_id, chapter, para, start_off, end_off, text, note, color, created_at, updated_at)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, unixepoch(), unixepoch())
         RETURNING {ANNOTATION_COLUMNS}"
    );
    let annotation = tx.prepare_cached(&sql)?.query_row(
        params![
            book_id,
            loc.chapter,
            loc.para,
            loc.off,
            end_off,
            &paragraph[start..end],
            note,
            color.as_str()
        ],
        annotation_row,
    )?;
    tx.commit()?;
    Ok(annotation)
}

pub(super) fn update_annotation(
    db: &Connection,
    book_id: &str,
    id: &str,
    expected_revision: u32,
    note: Option<&str>,
    color: AnnotationColor,
) -> Result<Annotation> {
    let note = checked_text(note, MAX_NOTE_BYTES, "annotation note")?;
    let sql = format!(
        "UPDATE annotation SET note = ?1, color = ?2, updated_at = unixepoch(), revision = revision + 1
         WHERE book_id = ?3 AND id = ?4 AND revision = ?5 RETURNING {ANNOTATION_COLUMNS}"
    );
    db.prepare_cached(&sql)?
        .query_row(
            params![note, color.as_str(), book_id, id, expected_revision],
            annotation_row,
        )
        .optional()?
        .ok_or(Error::AnnotationConflict)
}

pub(super) fn delete_annotation(
    db: &Connection,
    book_id: &str,
    id: &str,
    expected_revision: u32,
) -> Result<u32> {
    db.prepare_cached(
        "DELETE FROM annotation WHERE book_id = ?1 AND id = ?2 AND revision = ?3 RETURNING chapter",
    )?
    .query_row(params![book_id, id, expected_revision], |row| row.get(0))
    .optional()?
    .ok_or(Error::AnnotationConflict)
}

fn byte_at(text: &str, offset: u32) -> Result<usize> {
    let offset = offset as usize;
    if offset == text.chars().count() {
        return Ok(text.len());
    }
    text.char_indices()
        .nth(offset)
        .map(|(byte, _)| byte)
        .ok_or_else(|| Error::Invalid("annotation selection exceeds the paragraph".into()))
}

fn checked_text<'a>(
    value: Option<&'a str>,
    max_bytes: usize,
    name: &str,
) -> Result<Option<&'a str>> {
    let value = value.map(str::trim).filter(|text| !text.is_empty());
    if value.is_some_and(|text| text.len() > max_bytes || text.contains('\0')) {
        return Err(Error::Invalid(format!(
            "{name} must be at most {max_bytes} bytes without NUL"
        )));
    }
    Ok(value)
}
