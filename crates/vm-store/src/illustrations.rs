use std::{
    collections::HashMap,
    io::{Read, Seek},
};

use rusqlite::{params, Connection};
use vm_format::{EpubIllustration, EpubReader};

use crate::Result;

const MAX_ILLUSTRATIONS_PER_CHAPTER: u32 = 512;
const MAX_ALT_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Illustration {
    pub ordinal: u32,
    /// Paragraph index, or the paragraph count for an end-of-chapter image.
    pub before_para: u32,
    pub alt: String,
}

#[derive(Debug)]
pub struct IllustrationImage {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

pub(super) fn import<R: Read + Seek>(
    db: &Connection,
    book_id: &str,
    reader: &mut EpubReader<R>,
    illustrations: &[EpubIllustration],
) -> Result<()> {
    if illustrations.is_empty()
        || db.query_row(
            "SELECT EXISTS(SELECT 1 FROM epub_illustration WHERE book_id = ?1)",
            [book_id],
            |row| row.get(0),
        )?
    {
        return Ok(());
    }

    let mut images = HashMap::<&str, Option<String>>::new();
    let mut ordinals = HashMap::<usize, u32>::new();
    let mut insert_image = db.prepare_cached(
        "INSERT INTO epub_image(book_id, hash, media_type, bytes)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(book_id, hash) DO NOTHING",
    )?;
    let mut insert_anchor = db.prepare_cached(
        "INSERT INTO epub_illustration(book_id, chapter, ordinal, before_para, alt, image_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for occurrence in illustrations {
        let ordinal = ordinals.entry(occurrence.chapter).or_default();
        if *ordinal >= MAX_ILLUSTRATIONS_PER_CHAPTER {
            continue;
        }
        let path = occurrence.image.path.as_str();
        if let std::collections::hash_map::Entry::Vacant(entry) = images.entry(path) {
            let hash = match reader.read_image(&occurrence.image) {
                Ok(bytes) => {
                    let hash = blake3::hash(&bytes).to_hex().to_string();
                    insert_image.execute(params![
                        book_id,
                        hash,
                        occurrence.image.media_type,
                        bytes
                    ])?;
                    Some(hash)
                }
                Err(error) => {
                    eprintln!("Cannot read EPUB illustration: {error}");
                    None
                }
            };
            entry.insert(hash);
        }
        if let Some(hash) = &images[path] {
            let alt = occurrence
                .alt
                .chars()
                .take(MAX_ALT_CHARS)
                .collect::<String>();
            insert_anchor.execute(params![
                book_id,
                occurrence.chapter,
                *ordinal,
                occurrence.before_para,
                alt,
                hash
            ])?;
            *ordinal += 1;
        }
    }
    Ok(())
}

pub(super) fn list(db: &Connection, book_id: &str, chapter: u32) -> Result<Vec<Illustration>> {
    let mut stmt = db.prepare_cached(
        "SELECT ordinal, before_para, alt FROM epub_illustration
         WHERE book_id = ?1 AND chapter = ?2 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map(params![book_id, chapter], |row| {
        Ok(Illustration {
            ordinal: row.get(0)?,
            before_para: row.get(1)?,
            alt: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn read(
    db: &Connection,
    book_id: &str,
    chapter: u32,
    ordinal: u32,
) -> Result<IllustrationImage> {
    Ok(db
        .prepare_cached(
            "SELECT i.media_type, i.bytes
         FROM epub_illustration AS a
         JOIN epub_image AS i ON i.book_id = a.book_id AND i.hash = a.image_hash
         WHERE a.book_id = ?1 AND a.chapter = ?2 AND a.ordinal = ?3",
        )?
        .query_row(params![book_id, chapter, ordinal], |row| {
            Ok(IllustrationImage {
                media_type: row.get(0)?,
                bytes: row.get(1)?,
            })
        })?)
}
