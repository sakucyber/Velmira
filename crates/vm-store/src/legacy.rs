//! Read-only import of previous desktop and Android libraries.

mod android;

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{
    params, types::ValueRef, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use sha2::{Digest, Sha256};
use vm_core::Loc;
use vm_format::EpubReader;

use crate::{comics, covers, database, Error, Result};

const MAX_EPUB_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub books: u32,
    pub bookmarks: u32,
    pub annotations: u32,
    pub progress: u32,
    pub skipped_books: u32,
    pub skipped_positions: u32,
    pub covers: u32,
    pub pending_comics: u32,
    pub already_migrated: bool,
}

pub(super) fn migrate(
    db: &mut Connection,
    blobs: &Path,
    data_dir: &Path,
) -> Result<MigrationReport> {
    let source = if data_dir.is_dir() {
        library_database(data_dir)?
    } else {
        data_dir.to_owned()
    };
    let source = fs::canonicalize(source)?;
    let source_key = source
        .to_str()
        .ok_or_else(|| Error::Invalid("legacy database path is not valid UTF-8".into()))?;
    let library = source
        .parent()
        .ok_or_else(|| Error::Invalid("legacy database has no parent directory".into()))?;
    let mut old = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    old.busy_timeout(Duration::from_secs(5))?;
    old.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let snapshot = old.transaction()?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let android = snapshot.query_row(
        "SELECT COUNT(*) = 2 FROM pragma_table_info('books') WHERE name IN ('sha', 'json')",
        [],
        |row| row.get::<_, bool>(0),
    )? && has_table(&snapshot, "records")?;
    let revision = if android {
        "android-v2-media"
    } else {
        "v2-media"
    };
    let marker = format!("migrated_from:{revision}:{source_key}");
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
    if android {
        let report = android::migrate(&snapshot, &tx, blobs, library, source_key)?;
        if report.pending_comics == 0 {
            tx.execute("INSERT INTO meta(key, value) VALUES (?1, '1')", [&marker])?;
        }
        tx.commit()?;
        return Ok(report);
    }
    // Earlier schema fixtures and installations can have no source-file column.
    let has_file_path = snapshot.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('books') WHERE name = 'file_path')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let path_column = if has_file_path { "file_path" } else { "NULL" };
    // Querying the fixed v0.7 table names also rejects unrelated databases.
    let mut books = snapshot.prepare(&format!(
        "SELECT id, title, author, format, sha256, encoding, chapter_count, created_at, {path_column}
         FROM books ORDER BY id",
    ))?;
    let mut rows = books.query([])?;
    let mut report = MigrationReport::default();
    let has_state = has_table(&snapshot, "library_state")?;
    let has_progress = has_table(&snapshot, "progress")?;
    let has_bookmarks = has_table(&snapshot, "bookmarks")?;
    let has_annotations = has_table(&snapshot, "annotations")?;

    while let Some(row) = rows.next()? {
        let format = database::text_column(row, 3)?;
        if !matches!(format, "txt" | "epub" | "cbz") {
            report.skipped_books += 1;
            continue;
        }
        let old_id: String = row.get(0)?;
        let title: String = row.get(1)?;
        let author: Option<String> = row.get(2)?;
        let fingerprint = database::text_column(row, 4)?;
        let count: u32 = row.get(6)?;
        let created_at: i64 = row.get(7)?;
        let stored_path: Option<String> = row.get(8)?;
        if count == 0 {
            report.skipped_books += 1;
            continue;
        }
        let mapped_id = tx
            .prepare_cached("SELECT book_id FROM legacy_book WHERE source = ?1 AND legacy_id = ?2")?
            .query_row(params![source_key, old_id], |row| row.get::<_, String>(0))
            .optional()?;
        if let Some(mapped_id) = mapped_id {
            // v1 already migrated the normalized EPUB text. Recover its cover in place
            // without changing identifiers, edits or positions made in the native library.
            if format == "epub" {
                report.covers += u32::from(copy_epub_cover(
                    &tx,
                    library,
                    &old_id,
                    stored_path.as_deref(),
                    fingerprint,
                    &mapped_id,
                )?);
            }
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
        let book_id = if format == "cbz" {
            let Some((path, input)) = legacy_file(library, &old_id, "cbz", stored_path.as_deref())
            else {
                report.skipped_books += 1;
                report.pending_comics += 1;
                continue;
            };
            let mut comic = match comics::prepare(&tx, blobs, input, &path, Some(fingerprint)) {
                Ok(comic) if comic.count == count as usize => comic,
                Ok(_) | Err(Error::Invalid(_) | Error::Format(_)) => {
                    report.skipped_books += 1;
                    report.pending_comics += 1;
                    continue;
                }
                Err(err) => return Err(err),
            };
            comic.title = title;
            comic.author = author;
            report.books += u32::from(comic.insert(&tx, Some(created_at), status)?);
            comic.id
        } else {
            let encoding: Option<String> = row.get(5)?;
            let book_id = text_identity(format, fingerprint);
            let inserted = tx.execute(
                "INSERT INTO book(id, title, author, format, encoding, chapter_count, added_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO NOTHING",
                params![book_id, title, author, format, encoding, count, created_at, status],
            )? != 0;
            if inserted {
                copy_chapters(&snapshot, &tx, &old_id, &book_id, count)?;
                report.books += 1;
            }
            if format == "epub" {
                report.covers += u32::from(copy_epub_cover(
                    &tx,
                    library,
                    &old_id,
                    stored_path.as_deref(),
                    fingerprint,
                    &book_id,
                )?);
            }
            book_id
        };
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
    // Books already linked in legacy_book are skipped on retry. Keep missing
    // comic archives retryable without duplicating newly migrated books or edits.
    if report.pending_comics == 0 {
        tx.execute("INSERT INTO meta(key, value) VALUES (?1, '1')", [&marker])?;
    }
    tx.commit()?;
    Ok(report)
}

fn library_database(root: &Path) -> Result<PathBuf> {
    let mut candidates = [
        root.join("app.db"),
        root.join("velmira-local-reader.db"),
        root.join("databases").join("velmira-local-reader.db"),
        root.join("db").join("velmira-local-reader.db"),
    ]
    .into_iter()
    .filter(|path| path.is_file());
    let source = candidates.next().ok_or_else(|| {
        Error::Invalid("legacy database not found in the selected directory".into())
    })?;
    if candidates.next().is_some() {
        return Err(Error::Invalid(
            "multiple legacy databases found; select the database file".into(),
        ));
    }
    Ok(source)
}

fn text_identity(format: &str, fingerprint: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("Velmira legacy database book identity");
    hasher.update(format.as_bytes());
    hasher.update(&[0]);
    hasher.update(fingerprint.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn legacy_file(
    root: &Path,
    id: &str,
    extension: &str,
    stored: Option<&str>,
) -> Option<(PathBuf, File)> {
    let mut candidates = Vec::with_capacity(2);
    // Older versions recorded absolute paths. Use the distribution's fixed
    // relative layout so a copied database cannot redirect reads outside its library.
    if safe_file_stem(id) {
        candidates.push(root.join("books").join(format!("{id}.{extension}")));
    }
    if extension == "cbz" {
        if let Some(stem) = stored
            .and_then(|name| name.rsplit(['/', '\\']).next())
            .and_then(|name| name.strip_suffix(".cbz"))
            .filter(|stem| stem.starts_with("edition_") && safe_file_stem(stem))
        {
            candidates.push(root.join("exports").join(format!("{stem}.cbz")));
        }
    }
    candidates.into_iter().find_map(|candidate| {
        let candidate = fs::canonicalize(candidate).ok()?;
        if !candidate.starts_with(root)
            || !candidate
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
            || !fs::metadata(&candidate).ok()?.is_file()
        {
            return None;
        }
        let input = File::open(&candidate).ok()?;
        Some((candidate, input))
    })
}

fn safe_file_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn copy_epub_cover(
    db: &Connection,
    root: &Path,
    old_id: &str,
    stored: Option<&str>,
    fingerprint: &str,
    book_id: &str,
) -> Result<bool> {
    if covers::exists(db, book_id)? {
        return Ok(false);
    }
    let Some((_, mut input)) = legacy_file(root, old_id, "epub", stored) else {
        return Ok(false);
    };
    let cover = (|| {
        // Hash the source stream with a fixed read budget before using its metadata.
        let mut hash = Sha256::new();
        let read = io::copy(&mut (&mut input).take(MAX_EPUB_SOURCE_BYTES + 1), &mut hash)?;
        if read > MAX_EPUB_SOURCE_BYTES
            || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(fingerprint)
        {
            return Ok::<_, Error>(None);
        }
        input.seek(SeekFrom::Start(0))?;
        let mut reader = EpubReader::new(input)?;
        let candidates = reader.cover_candidates()?;
        Ok(covers::epub_thumbnail(&mut reader, &candidates))
    })()
    .unwrap_or_else(|error| {
        eprintln!("Cannot read legacy EPUB cover: {error}");
        None
    });
    covers::insert(db, book_id, cover.as_ref())?;
    Ok(cover.is_some())
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
    insert_progress(
        new,
        book_id,
        parse_loc(chapter, para, off),
        updated_at,
        report,
    )
}

fn insert_progress(
    new: &Connection,
    book_id: &str,
    loc: Option<Loc>,
    updated_at: i64,
    report: &mut MigrationReport,
) -> Result<()> {
    let Some(loc) = loc else {
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
        let label: Option<String> = row.get(3)?;
        insert_bookmark(
            new,
            book_id,
            parse_loc(row.get(0)?, row.get(1)?, row.get(2)?),
            label.as_deref(),
            row.get(4)?,
            report,
        )?;
    }
    Ok(())
}

fn insert_bookmark(
    new: &Connection,
    book_id: &str,
    loc: Option<Loc>,
    label: Option<&str>,
    created_at: i64,
    report: &mut MigrationReport,
) -> Result<()> {
    let Some(loc) = loc else {
        report.skipped_positions += 1;
        return Ok(());
    };
    if database::paragraph_len(new, book_id, &loc)?.is_none_or(|len| loc.off > len) {
        report.skipped_positions += 1;
        return Ok(());
    }
    report.bookmarks += new.execute(
        "INSERT INTO bookmark(book_id, chapter, para, off, label, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(book_id, chapter, para, off) DO NOTHING",
        params![book_id, loc.chapter, loc.para, loc.off, label, created_at],
    )? as u32;
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
