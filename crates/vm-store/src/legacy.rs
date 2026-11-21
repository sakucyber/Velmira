//! Read-only import of the previous desktop library's normalized text and reading records.

use std::{fs, path::Path, time::Duration};

use rusqlite::{
    params, types::ValueRef, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use vm_core::Loc;

use crate::{database, Error, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub books: u32,
    pub bookmarks: u32,
    pub annotations: u32,
    pub progress: u32,
    pub skipped_books: u32,
    pub skipped_positions: u32,
    pub already_migrated: bool,
}

pub(super) fn migrate(db: &mut Connection, data_dir: &Path) -> Result<MigrationReport> {
    let source = if data_dir.is_dir() {
        data_dir.join("app.db")
    } else {
        data_dir.to_owned()
    };
    let source = fs::canonicalize(source)?;
    let source_key = source
        .to_str()
        .ok_or_else(|| Error::Invalid("legacy database path is not valid UTF-8".into()))?;
    let marker = format!("migrated_from:v1-text:{source_key}");
    let mut old = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    old.busy_timeout(Duration::from_secs(5))?;
    old.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let snapshot = old.transaction()?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx
        .query_row("SELECT 1 FROM meta WHERE key = ?1", [&marker], |_| Ok(()))
        .optional()?
        .is_some()
    {
        return Ok(MigrationReport {
            already_migrated: true,
            ..MigrationReport::default()
        });
    }
    // Querying the fixed v0.7 table names also rejects unrelated databases.
    let mut books = snapshot.prepare(
        "SELECT id, title, author, format, sha256, encoding, chapter_count, created_at
         FROM books ORDER BY id",
    )?;
    let mut rows = books.query([])?;
    let mut report = MigrationReport::default();
    let has_state = has_table(&snapshot, "library_state")?;
    let has_progress = has_table(&snapshot, "progress")?;
    let has_bookmarks = has_table(&snapshot, "bookmarks")?;
    let has_annotations = has_table(&snapshot, "annotations")?;

    while let Some(row) = rows.next()? {
        let format = database::text_column(row, 3)?;
        if !matches!(format, "txt" | "epub") {
            report.skipped_books += 1;
            continue;
        }
        let old_id: String = row.get(0)?;
        let title: String = row.get(1)?;
        let author: Option<String> = row.get(2)?;
        let fingerprint = database::text_column(row, 4)?;
        let encoding: Option<String> = row.get(5)?;
        let count: u32 = row.get(6)?;
        let created_at: i64 = row.get(7)?;
        if count == 0 {
            report.skipped_books += 1;
            continue;
        }
        let status = if has_state
            && snapshot
                .query_row(
                    "SELECT 1 FROM library_state WHERE book_id = ?1",
                    [&old_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
        {
            "removed"
        } else {
            "active"
        };
        let mut hasher = blake3::Hasher::new_derive_key("Velmira legacy database book identity");
        hasher.update(format.as_bytes());
        hasher.update(&[0]);
        hasher.update(fingerprint.as_bytes());
        let book_id = hasher.finalize().to_hex().to_string();
        let inserted = tx.execute(
            "INSERT INTO book(id, title, author, format, encoding, chapter_count, added_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO NOTHING",
            params![book_id, title, author, format, encoding, count, created_at, status],
        )? != 0;
        if inserted {
            copy_chapters(&snapshot, &tx, &old_id, &book_id, count)?;
            report.books += 1;
        }
        tx.execute(
            "INSERT INTO legacy_book(source, legacy_id, book_id) VALUES (?1, ?2, ?3)",
            params![source_key, old_id, book_id],
        )?;
        if has_progress {
            copy_progress(&snapshot, &tx, &old_id, &book_id, &mut report)?;
        }
        if has_bookmarks {
            copy_bookmarks(&snapshot, &tx, &old_id, &book_id, &mut report)?;
        }
        if has_annotations {
            copy_annotations(&snapshot, &tx, &old_id, &book_id, &mut report)?;
        }
    }
    tx.execute("INSERT INTO meta(key, value) VALUES (?1, '1')", [&marker])?;
    tx.commit()?;
    Ok(report)
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn copy_chapters(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    count: u32,
) -> Result<()> {
    let mut stmt = old.prepare_cached(
        "SELECT idx, title, CASE WHEN length(CAST(paras_json AS BLOB)) <= 67108864
                                THEN paras_json END
         FROM chapters WHERE book_id = ?1 ORDER BY idx",
    )?;
    let mut rows = stmt.query([old_id])?;
    let mut seen = 0;
    while let Some(row) = rows.next()? {
        let idx = row.get(0)?;
        if idx != seen || idx >= count {
            return Err(Error::Invalid(
                "legacy chapter indices are not contiguous".into(),
            ));
        }
        let title = database::text_column(row, 1)?;
        if matches!(row.get_ref(2)?, ValueRef::Null) {
            return Err(Error::Invalid("legacy chapter JSON exceeds 64 MiB".into()));
        }
        let json = database::text_column(row, 2)?;
        let paragraphs: Vec<String> = serde_json::from_str(json)?;
        database::insert_chapter(new, book_id, idx, title, &paragraphs)?;
        seen += 1;
    }
    if seen != count {
        return Err(Error::Invalid(
            "legacy chapter count does not match the book".into(),
        ));
    }
    Ok(())
}

fn copy_progress(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    report: &mut MigrationReport,
) -> Result<()> {
    let item = old
        .prepare_cached(
            "SELECT chapter, para, offset, updated_at FROM progress WHERE book_id = ?1",
        )?
        .query_row([old_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .optional()?;
    let Some((chapter, para, off, updated_at)) = item else {
        return Ok(());
    };
    let Some(loc) = parse_loc(chapter, para, off) else {
        report.skipped_positions += 1;
        return Ok(());
    };
    // Legacy progress could save an out-of-bounds paragraph/offset. Preserve
    // valid locations exactly and ignore invalid data instead of drifting.
    let valid = match database::paragraph_len(new, book_id, &loc)? {
        Some(len) => loc.off <= len,
        None => {
            loc.para == 0
                && loc.off == 0
                && new
                    .query_row(
                        "SELECT 1 FROM chapter WHERE book_id = ?1 AND idx = ?2",
                        params![book_id, loc.chapter],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some()
        }
    };
    if !valid {
        report.skipped_positions += 1;
        return Ok(());
    }
    report.progress += new.execute(
        "INSERT INTO progress(book_id, chapter, para, off, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(book_id) DO NOTHING",
        params![book_id, loc.chapter, loc.para, loc.off, updated_at],
    )? as u32;
    Ok(())
}

fn parse_loc(chapter: i64, para: i64, off: i64) -> Option<Loc> {
    Some(Loc {
        chapter: chapter.try_into().ok()?,
        para: para.try_into().ok()?,
        off: off.try_into().ok()?,
    })
}

fn copy_bookmarks(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    report: &mut MigrationReport,
) -> Result<()> {
    let mut stmt = old.prepare_cached(
        "SELECT chapter, para, offset, label, created_at FROM bookmarks WHERE book_id = ?1
         ORDER BY created_at DESC, id",
    )?;
    let mut rows = stmt.query([old_id])?;
    while let Some(row) = rows.next()? {
        let Some(loc) = parse_loc(row.get(0)?, row.get(1)?, row.get(2)?) else {
            report.skipped_positions += 1;
            continue;
        };
        if database::paragraph_len(new, book_id, &loc)?.is_none_or(|len| loc.off > len) {
            report.skipped_positions += 1;
            continue;
        }
        report.bookmarks += new.execute(
            "INSERT INTO bookmark(book_id, chapter, para, off, label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(book_id, chapter, para) DO NOTHING",
            params![
                book_id,
                loc.chapter,
                loc.para,
                loc.off,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?
            ],
        )? as u32;
    }
    Ok(())
}

fn copy_annotations(
    old: &Connection,
    new: &Connection,
    old_id: &str,
    book_id: &str,
    report: &mut MigrationReport,
) -> Result<()> {
    let mut stmt = old.prepare_cached(
        "SELECT id, chapter, para, start_offset, end_offset, text, note, color, created_at, updated_at
         FROM annotations WHERE book_id = ?1 ORDER BY id",
    )?;
    let mut rows = stmt.query([old_id])?;
    while let Some(row) = rows.next()? {
        let Some(loc) = parse_loc(row.get(1)?, row.get(2)?, row.get(3)?) else {
            report.skipped_positions += 1;
            continue;
        };
        let end: i64 = row.get(4)?;
        let color = database::text_column(row, 7)?;
        let selected = database::text_column(row, 5)?;
        if end <= i64::from(loc.off)
            || database::paragraph_len(new, book_id, &loc)?.is_none_or(|len| end > i64::from(len))
            || selected.chars().count() as i64 != end - i64::from(loc.off)
            || !matches!(color, "yellow" | "green" | "blue" | "pink")
        {
            report.skipped_positions += 1;
            continue;
        }
        let id = format!("{book_id}:{}", database::text_column(row, 0)?);
        report.annotations += new.execute(
            "INSERT INTO annotation(id, book_id, chapter, para, start_off, end_off, text, note, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(id) DO NOTHING",
            params![id, book_id, loc.chapter, loc.para, loc.off, end, selected,
                    row.get::<_, Option<String>>(6)?, color, row.get::<_, i64>(8)?, row.get::<_, i64>(9)?],
        )? as u32;
    }
    Ok(())
}
