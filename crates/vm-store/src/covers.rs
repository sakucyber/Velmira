use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use vm_core::{BookFormat, BookSummary};
use vm_format::EpubReader;
use vm_media::{derive_cover, CoverVariants};

use crate::{database, Result};

const MAX_SHELF_BYTES: usize = 1024 * 1024;
const MAX_DETAIL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CoverSize {
    #[default]
    Shelf,
    Detail,
}

pub(super) fn import_epub(db: &mut Connection, path: &Path) -> Result<(BookSummary, bool)> {
    let mut reader = EpubReader::open(path)?;
    let name = path.file_name().map(|name| name.to_string_lossy());
    let parsed = reader.parse(name.as_deref().unwrap_or_default())?;
    let book_id = database::text_id(&parsed.text, BookFormat::Epub);
    let cover = if exists(db, &book_id)? {
        None
    } else {
        parsed
            .cover_candidates
            .iter()
            .find_map(|image| match reader.read_image(image) {
                Ok(bytes) => thumbnail(&bytes),
                Err(error) => {
                    eprintln!("Cannot read an EPUB cover candidate: {error}");
                    None
                }
            })
    };
    database::insert_covered_text(
        db,
        parsed.text,
        BookFormat::Epub,
        parsed.author,
        cover.as_ref(),
    )
}

pub(super) fn exists(db: &Connection, book_id: &str) -> Result<bool> {
    Ok(db
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM book_cover WHERE book_id = ?1)")?
        .query_row([book_id], |row| row.get(0))?)
}

pub(super) fn thumbnail(bytes: &[u8]) -> Option<CoverVariants> {
    match derive_cover(bytes) {
        Ok(cover)
            if cover.shelf_webp.len() <= MAX_SHELF_BYTES
                && cover.detail_webp.len() <= MAX_DETAIL_BYTES =>
        {
            Some(cover)
        }
        Ok(_) => {
            eprintln!("Skipping cover: derived WebP exceeds its storage limit");
            None
        }
        Err(error) => {
            eprintln!("Skipping cover: {error}");
            None
        }
    }
}

pub(super) fn insert(db: &Connection, book_id: &str, cover: Option<&CoverVariants>) -> Result<()> {
    if let Some(cover) = cover {
        db.prepare_cached(
            "INSERT INTO book_cover(book_id, shelf_webp, detail_webp)
             VALUES (?1, ?2, ?3) ON CONFLICT(book_id) DO NOTHING",
        )?
        .execute(params![book_id, cover.shelf_webp, cover.detail_webp])?;
    }
    Ok(())
}

pub(super) fn read(db: &Connection, book_id: &str, size: CoverSize) -> Result<Option<Vec<u8>>> {
    let column = match size {
        CoverSize::Shelf => "shelf_webp",
        CoverSize::Detail => "detail_webp",
    };
    let sql = format!("SELECT {column} FROM book_cover WHERE book_id = ?1");
    Ok(db
        .prepare_cached(&sql)?
        .query_row([book_id], |row| row.get(0))
        .optional()?)
}
