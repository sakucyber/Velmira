use std::{fs, path::Path, time::Duration};

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use vm_core::{BookFormat, BookSummary, Chapter, ImportedText, Loc, TxtEncoding};

use crate::{covers, Error, ReadingStatus, Result, SearchHit, ShelfFilter, ShelfSort, ShelfStatus};

const SCHEMA_VERSION: u32 = 6;
const SHELF_PAGE: u32 = 60;
const MAX_SHELF_QUERY_BYTES: usize = 1024;
const MAX_TITLE_BYTES: usize = 4096;
const CHAPTER_LIMIT: usize = 64 * 1024 * 1024;

pub(super) fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -4096;
         PRAGMA mmap_size = 0;",
    )?;
    let schema = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = schema.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
    if version == 0 {
        let table_exists = schema.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if table_exists {
            return Err(Error::Invalid(
                "select a separate path for the Rust library".into(),
            ));
        }
        schema.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    } else if version > SCHEMA_VERSION {
        return Err(Error::Invalid(format!("database schema version {version}")));
    }
    if version < 2 {
        schema.execute_batch(include_str!("../migrations/0002_annotation_revision.sql"))?;
    }
    if version < 3 {
        schema.execute_batch(include_str!("../migrations/0003_comic_archive.sql"))?;
    }
    if version < 4 {
        schema.execute_batch(include_str!("../migrations/0004_cover.sql"))?;
    }
    if version < 5 {
        schema.execute_batch(include_str!("../migrations/0005_epub_illustrations.sql"))?;
    }
    if version < 6 {
        schema.execute_batch(include_str!("../migrations/0006_sync.sql"))?;
        schema.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    schema.commit()?;
    db.execute_batch("PRAGMA journal_mode = WAL;")?;
    Ok(db)
}

const SUMMARY_COLUMNS: &str = "b.id, b.title, b.format, b.chapter_count, p.chapter, p.para, p.off";

fn summary_row(row: &Row<'_>) -> rusqlite::Result<BookSummary> {
    let format = match text_column(row, 2)? {
        "txt" => BookFormat::Txt,
        "epub" => BookFormat::Epub,
        "cbz" => BookFormat::Cbz,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let progress = if let Some(chapter) = row.get::<_, Option<u32>>(4)? {
        Some(Loc {
            chapter,
            para: row.get(5)?,
            off: row.get(6)?,
        })
    } else {
        None
    };
    Ok(BookSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        format,
        chapter_count: row.get(3)?,
        progress,
    })
}

pub(super) fn text_column<'row>(row: &'row Row<'_>, index: usize) -> rusqlite::Result<&'row str> {
    let value = row.get_ref(index)?;
    value.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, value.data_type(), Box::new(error))
    })
}

pub(super) fn list_books(db: &Connection, offset: u32) -> Result<(Vec<BookSummary>, bool)> {
    list_shelf(db, offset, &ShelfFilter::default())
}

pub(super) fn list_shelf(
    db: &Connection,
    offset: u32,
    filter: &ShelfFilter,
) -> Result<(Vec<BookSummary>, bool)> {
    let query = filter.query.trim();
    if query.len() > MAX_SHELF_QUERY_BYTES || query.contains('\0') {
        return Err(Error::Invalid(format!(
            "shelf search is limited to {MAX_SHELF_QUERY_BYTES} bytes without NUL"
        )));
    }
    let order = match filter.sort {
        ShelfSort::Newest => "b.added_at DESC, b.id",
        ShelfSort::Title => "b.title COLLATE NOCASE ASC, b.id",
        ShelfSort::LastRead => "p.updated_at DESC, b.added_at DESC, b.id",
    };
    let reading = match filter.reading {
        ReadingStatus::All => "1",
        ReadingStatus::Unread => "p.book_id IS NULL",
        ReadingStatus::Reading => "p.chapter < b.chapter_count - 1",
        ReadingStatus::LastChapter => "p.chapter = b.chapter_count - 1",
    };
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS}
         FROM book b LEFT JOIN progress p ON p.book_id = b.id
         WHERE b.status = ?1 AND (?2 IS NULL OR b.format = ?2)
           AND (?3 = '' OR instr(lower(b.title), lower(?3)) > 0)
           AND ({reading})
         ORDER BY {order} LIMIT ?4 OFFSET ?5"
    );
    let mut stmt = db.prepare_cached(&sql)?;
    let mut books = stmt
        .query_map(
            params![
                filter.status.as_str(),
                filter.format.map(BookFormat::as_str),
                query,
                SHELF_PAGE + 1,
                offset
            ],
            summary_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = books.len() > SHELF_PAGE as usize;
    books.truncate(SHELF_PAGE as usize);
    Ok((books, has_more))
}

pub(super) fn rename_book(
    db: &mut Connection,
    book_id: &str,
    title: &str,
) -> Result<(BookSummary, ShelfStatus)> {
    let title = title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.contains('\0') {
        return Err(Error::Invalid(format!(
            "book title must be 1 to {MAX_TITLE_BYTES} bytes without NUL"
        )));
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let status = tx
        .prepare_cached("UPDATE book SET title = ?1 WHERE id = ?2 RETURNING status")?
        .query_row(params![title, book_id], |row| match text_column(row, 0)? {
            "active" => Ok(ShelfStatus::Active),
            "removed" => Ok(ShelfStatus::Removed),
            _ => Err(rusqlite::Error::InvalidQuery),
        })?;
    let book = summary(&tx, book_id)?;
    tx.commit()?;
    Ok((book, status))
}

pub(super) fn set_book_status(
    db: &mut Connection,
    book_id: &str,
    status: ShelfStatus,
) -> Result<BookSummary> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.prepare_cached("UPDATE book SET status = ?1 WHERE id = ?2")?
        .execute(params![status.as_str(), book_id])?;
    let book = summary(&tx, book_id)?;
    tx.commit()?;
    Ok(book)
}

pub(super) fn summary(db: &Connection, book_id: &str) -> Result<BookSummary> {
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS}
         FROM book b LEFT JOIN progress p ON p.book_id = b.id WHERE b.id = ?1"
    );
    Ok(db.prepare_cached(&sql)?.query_row([book_id], summary_row)?)
}

pub(super) fn toc(db: &Connection, book_id: &str, count: u32) -> Result<Vec<String>> {
    let mut stmt =
        db.prepare_cached("SELECT idx, title FROM chapter WHERE book_id = ?1 ORDER BY idx")?;
    let mut rows = stmt.query([book_id])?;
    let mut titles = Vec::new();
    while let Some(row) = rows.next()? {
        if row.get::<_, usize>(0)? != titles.len() {
            return Err(Error::Invalid("chapter indices are not contiguous".into()));
        }
        titles.push(row.get(1)?);
    }
    if titles.len() != count as usize {
        return Err(Error::Invalid(
            "chapter count does not match the book".into(),
        ));
    }
    Ok(titles)
}

pub(super) fn chapter(db: &Connection, book_id: &str, idx: u32) -> Result<Chapter> {
    let (title, raw_len, body): (String, usize, Vec<u8>) = db
        .prepare_cached(
            "SELECT title, byte_len, body FROM chapter WHERE book_id = ?1 AND idx = ?2",
        )?
        .query_row(params![book_id, idx], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if raw_len > CHAPTER_LIMIT {
        return Err(Error::Invalid(
            "chapter exceeds the uncompressed size limit".into(),
        ));
    }
    let body = zstd::bulk::decompress(&body, raw_len)?;
    if body.len() != raw_len {
        return Err(Error::Invalid(
            "chapter size does not match its index".into(),
        ));
    }
    let prose = std::str::from_utf8(&body).map_err(|error| Error::Invalid(error.to_string()))?;
    let mut stmt = db.prepare_cached(
        "SELECT idx, byte_off, byte_len FROM paragraph
         WHERE book_id = ?1 AND chapter = ?2 ORDER BY idx",
    )?;
    let mut rows = stmt.query(params![book_id, idx])?;
    let mut paragraphs = Vec::new();
    let mut next_off = 0;
    while let Some(row) = rows.next()? {
        let para = row.get::<_, usize>(0)?;
        let off = row.get::<_, usize>(1)?;
        let len = row.get::<_, usize>(2)?;
        let end = off
            .checked_add(len)
            .ok_or_else(|| Error::Invalid("paragraph bounds overflow".into()))?;
        if off != next_off || para != paragraphs.len() {
            return Err(Error::Invalid(
                "paragraph indices are not contiguous".into(),
            ));
        }
        paragraphs.push(
            prose
                .get(off..end)
                .ok_or_else(|| Error::Invalid("paragraph bounds are invalid".into()))?
                .to_owned(),
        );
        next_off = end;
    }
    if next_off != prose.len() {
        return Err(Error::Invalid("paragraphs do not cover the chapter".into()));
    }
    Ok(Chapter { title, paragraphs })
}

pub(super) fn insert_text(
    db: &mut Connection,
    book: ImportedText,
    format: BookFormat,
    author: Option<String>,
) -> Result<(BookSummary, bool)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = insert_covered_text(&tx, book, format, author, None)?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn insert_covered_text(
    db: &Connection,
    book: ImportedText,
    format: BookFormat,
    author: Option<String>,
    cover: Option<&vm_media::CoverVariants>,
) -> Result<(BookSummary, bool)> {
    if book.chapters.is_empty() {
        return Err(Error::Invalid(
            "a book must contain at least one chapter".into(),
        ));
    }
    let count = u32::try_from(book.chapters.len())
        .map_err(|_| Error::Invalid("too many chapters".into()))?;
    let id = text_id(&book, format);
    let inserted = db.execute(
        "INSERT INTO book(id, title, format, encoding, chapter_count, author)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO NOTHING",
        params![
            id,
            book.title,
            format.as_str(),
            encoding_name(book.encoding),
            count,
            author
        ],
    )? != 0;
    if inserted {
        for (idx, chapter) in book.chapters.into_iter().enumerate() {
            insert_chapter(db, &id, idx as u32, &chapter.title, &chapter.paragraphs)?;
        }
    } else {
        restore_reimport(db, &id)?;
    }
    covers::insert(db, &id, cover)?;
    Ok((summary(db, &id)?, !inserted))
}

pub(super) fn restore_reimport(db: &Connection, book_id: &str) -> Result<()> {
    db.prepare_cached("UPDATE book SET status = 'active' WHERE id = ?1 AND status = 'removed'")?
        .execute([book_id])?;
    Ok(())
}

fn encoding_name(encoding: TxtEncoding) -> &'static str {
    match encoding {
        TxtEncoding::Utf8 => "utf-8",
        TxtEncoding::Utf16Le => "utf-16le",
        TxtEncoding::Utf16Be => "utf-16be",
        TxtEncoding::Gb18030 => "gb18030",
    }
}

pub(super) fn text_id(book: &ImportedText, format: BookFormat) -> String {
    // Keep the existing TXT identity stable across schema upgrades.
    let domain = match format {
        BookFormat::Txt => "Velmira v1 normalized TXT book identity",
        BookFormat::Epub => "Velmira v1 normalized EPUB book identity",
        BookFormat::Cbz => unreachable!("comics are not imported as text"),
    };
    let mut hash = blake3::Hasher::new_derive_key(domain);
    fn hash_field(hash: &mut blake3::Hasher, field: &str) {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hash_field(&mut hash, &book.title);
    hash.update(&(book.chapters.len() as u64).to_le_bytes());
    for chapter in &book.chapters {
        hash_field(&mut hash, &chapter.title);
        hash.update(&(chapter.paragraphs.len() as u64).to_le_bytes());
        for para in &chapter.paragraphs {
            hash_field(&mut hash, para);
        }
    }
    hash.finalize().to_hex().to_string()
}

pub(super) fn insert_chapter(
    db: &Connection,
    book_id: &str,
    idx: u32,
    title: &str,
    paragraphs: &[String],
) -> Result<()> {
    let len = paragraphs
        .iter()
        .try_fold(0usize, |size, para| size.checked_add(para.len()))
        .filter(|len| *len <= CHAPTER_LIMIT)
        .ok_or_else(|| Error::Invalid("chapter exceeds the uncompressed size limit".into()))?;
    let mut body = Vec::new();
    body.try_reserve_exact(len)
        .map_err(|error| Error::Invalid(error.to_string()))?;
    for para in paragraphs {
        body.extend_from_slice(para.as_bytes());
    }
    let compressed = zstd::bulk::compress(&body, 3)?;
    drop(body);
    db.prepare_cached(
        "INSERT INTO chapter(book_id, idx, title, byte_len, body) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![book_id, idx, title, len, compressed])?;

    let mut paragraph = db.prepare_cached(
        "INSERT INTO paragraph(book_id, chapter, idx, byte_off, byte_len, char_len)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut fts = db.prepare_cached(
        "INSERT INTO paragraph_fts(body, book_id, chapter, para) VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut off = 0;
    for (para_idx, text) in paragraphs.iter().enumerate() {
        paragraph.execute(params![
            book_id,
            idx,
            para_idx,
            off,
            text.len(),
            text.chars().count()
        ])?;
        fts.execute(params![text, book_id, idx, para_idx])?;
        off += text.len();
    }
    Ok(())
}

pub(super) fn paragraph_len(db: &Connection, book_id: &str, loc: &Loc) -> Result<Option<u32>> {
    Ok(db
        .prepare_cached(
            "SELECT char_len FROM paragraph WHERE book_id = ?1 AND chapter = ?2 AND idx = ?3",
        )?
        .query_row(params![book_id, loc.chapter, loc.para], |row| row.get(0))
        .optional()?)
}

pub(super) fn valid_progress(db: &Connection, book_id: &str, loc: &Loc) -> Result<bool> {
    if let Some(char_len) = paragraph_len(db, book_id, loc)? {
        return Ok(loc.off <= char_len);
    }
    if loc.off != 0 {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM chapter c
            WHERE c.book_id=?1 AND c.idx=?2 AND (?3=0 OR EXISTS(
                SELECT 1 FROM epub_illustration i
                WHERE i.book_id=c.book_id AND i.chapter=c.idx AND i.before_para=?3
            )))",
        params![book_id, loc.chapter, loc.para],
        |row| row.get(0),
    )?)
}

pub(super) fn save_progress(db: &Connection, book_id: &str, loc: &Loc) -> Result<()> {
    if !valid_progress(db, book_id, loc)? {
        return Err(Error::Invalid(
            "reading position is outside a paragraph".into(),
        ));
    }
    db.prepare_cached(
        "INSERT INTO progress(book_id, chapter, para, off) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(book_id) DO UPDATE SET
           chapter = excluded.chapter, para = excluded.para,
           off = excluded.off, updated_at = unixepoch()",
    )?
    .execute(params![book_id, loc.chapter, loc.para, loc.off])?;
    Ok(())
}

pub(super) fn search(
    db: &Connection,
    book_id: &str,
    chapter: Option<u32>,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    let query = query.trim();
    let len = query.chars().count();
    if len == 0 || limit == 0 {
        return Ok(Vec::new());
    }
    if len > 256 || query.contains('\0') {
        return Err(Error::Invalid(
            "search terms are limited to 256 characters without NUL".into(),
        ));
    }
    if len == 1 && chapter.is_none() {
        return Err(Error::Invalid(
            "search a single character within one chapter".into(),
        ));
    }
    let limit = limit.min(200);
    let (sql, pattern) = if len >= 3 {
        (
            "SELECT chapter, para, body FROM paragraph_fts
             WHERE book_id = ?1 AND (?2 IS NULL OR chapter = ?2) AND paragraph_fts MATCH ?3
             ORDER BY bm25(paragraph_fts), chapter, para LIMIT ?4",
            format!("\"{}\"", query.replace('"', "\"\"")),
        )
    } else {
        (
            "SELECT chapter, para, body FROM paragraph_fts
             WHERE book_id = ?1 AND (?2 IS NULL OR chapter = ?2) AND instr(lower(body), lower(?3)) > 0
             ORDER BY chapter, para LIMIT ?4",
            query.to_owned(),
        )
    };
    let mut stmt = db.prepare_cached(sql)?;
    let mut rows = stmt.query(params![book_id, chapter, pattern, limit])?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        let matched = text_column(row, 2)?;
        let (off, excerpt) = excerpt(matched, query);
        hits.push(SearchHit {
            loc: Loc {
                chapter: row.get(0)?,
                para: row.get(1)?,
                off,
            },
            excerpt,
        });
    }
    Ok(hits)
}

fn excerpt(paragraph: &str, query: &str) -> (u32, String) {
    // FTS5 folds case. Track scalar offsets in the original paragraph so
    // byte lengths and multibyte case mappings cannot change saved positions.
    let start = paragraph
        .find(query)
        .map(|index| paragraph[..index].chars().count())
        .or_else(|| {
            let folded_query = query.to_lowercase();
            paragraph
                .char_indices()
                .enumerate()
                .find_map(|(ordinal, (byte, ch))| {
                    ch.to_lowercase()
                        .eq(folded_query.chars().take(1))
                        .then_some(byte)
                        .filter(|byte| {
                            paragraph[*byte..]
                                .chars()
                                .flat_map(char::to_lowercase)
                                .take(folded_query.chars().count())
                                .eq(folded_query.chars())
                        })
                        .map(|_| ordinal)
                })
        })
        .unwrap_or(0);
    let from = start.saturating_sub(24);
    let take = query.chars().count() + 64;
    let mut excerpt = String::new();
    if from != 0 {
        excerpt.push('…');
    }
    let mut chars = paragraph.chars().skip(from);
    excerpt.extend(chars.by_ref().take(take));
    if chars.next().is_some() {
        excerpt.push('…');
    }
    (start as u32, excerpt)
}
