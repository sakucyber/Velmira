use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, TransactionBehavior};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use vm_core::BookSummary;
use vm_format::CbzReader;
use vm_media::CoverVariants;

use crate::{covers, database, Error, Result};

const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
pub struct ComicPage {
    pub index: u32,
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub(super) struct ComicReader {
    active: Option<(String, CbzReader<File>)>,
}

impl ComicReader {
    fn reader<'a>(
        &'a mut self,
        db: &Connection,
        blobs: &Path,
        book_id: &str,
    ) -> Result<&'a mut CbzReader<File>> {
        if self.active.as_ref().is_none_or(|(id, _)| id != book_id) {
            let hash: String = db
                .prepare_cached("SELECT blob_hash FROM comic_archive WHERE book_id = ?1")?
                .query_row([book_id], |row| row.get(0))?;
            let reader = CbzReader::open(&blob_path(blobs, &hash)?)?;
            self.active = Some((book_id.to_owned(), reader));
        }
        Ok(&mut self.active.as_mut().expect("reader opened above").1)
    }

    pub(super) fn source(
        &mut self,
        db: &Connection,
        blobs: &Path,
        book_id: &str,
    ) -> Result<Option<vm_core::ComicSource>> {
        Ok(self.reader(db, blobs, book_id)?.source())
    }

    pub(super) fn read_page(
        &mut self,
        db: &Connection,
        blobs: &Path,
        book_id: &str,
        index: u32,
    ) -> Result<ComicPage> {
        let reader = self.reader(db, blobs, book_id)?;
        let index_usize = index as usize;
        let media_type = reader
            .pages()
            .get(index_usize)
            .ok_or_else(|| Error::Invalid("comic page outside the book".into()))?
            .media_type;
        let bytes = reader.read_page(index_usize)?;
        Ok(ComicPage {
            index,
            media_type,
            bytes,
        })
    }
}

pub(super) fn import(
    db: &mut Connection,
    blobs: &Path,
    path: &Path,
) -> Result<(BookSummary, bool)> {
    let prepared = prepare(db, blobs, File::open(path)?, path, None)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inserted = prepared.insert(&tx, None, "active")?;
    if !inserted {
        database::restore_reimport(&tx, &prepared.id)?;
    }
    let book = database::summary(&tx, &prepared.id)?;
    tx.commit()?;
    Ok((book, !inserted))
}

pub(super) struct PreparedComic {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    hash: String,
    cover: Option<CoverVariants>,
    pub count: usize,
}

pub(super) fn prepare(
    db: &Connection,
    blobs: &Path,
    input: File,
    path: &Path,
    legacy_sha256: Option<&str>,
) -> Result<PreparedComic> {
    let info = input.metadata()?;
    if !info.is_file() || info.len() > MAX_ARCHIVE_BYTES {
        return Err(Error::Invalid(
            "CBZ must be a file no larger than 512 MiB".into(),
        ));
    }
    fs::create_dir_all(blobs)?;
    let mut copy = NamedTempFile::new_in(blobs)?;
    let hash = hash_copy(input, &mut copy, legacy_sha256)?;
    let mut id_hash = blake3::Hasher::new_derive_key("Velmira v1 CBZ book identity");
    id_hash.update(hash.as_bytes());
    let id = id_hash.finalize().to_hex().to_string();
    let file_name = path.file_name().map(|name| name.to_string_lossy());
    let mut reader = CbzReader::new(copy.reopen()?, file_name.as_deref().unwrap_or_default())?;
    // Reject an unusable first page before adding the comic to the bookshelf.
    let first_page = reader.read_page(0)?;
    let cover = if covers::exists(db, &id)? {
        None
    } else {
        covers::thumbnail(&first_page)
    };
    drop(first_page);
    let count = reader.pages().len();
    let title = reader.title.clone();
    let author = reader.author.clone();
    drop(reader);

    let destination = blob_path(blobs, &hash)?;
    // Persist the blob before the transaction references it. A crash or SQL
    // failure can leave an unreferenced complete blob, never a partial book.
    copy.as_file().sync_all()?;
    if let Err(error) = copy.persist_noclobber(&destination) {
        if error.error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error.error.into());
        }
        let existing = File::open(&destination)?;
        if !existing.metadata()?.is_file() || hash_copy(existing, &mut io::sink(), None)? != hash {
            return Err(Error::Invalid(
                "existing comic blob has an invalid content hash".into(),
            ));
        }
    }
    Ok(PreparedComic {
        id,
        title,
        author,
        count,
        hash,
        cover,
    })
}

impl PreparedComic {
    pub(super) fn insert(
        &self,
        db: &Connection,
        added_at: Option<i64>,
        status: &str,
    ) -> Result<bool> {
        let inserted = db.execute(
            "INSERT INTO book(id, title, author, format, chapter_count, added_at, status)
             VALUES (?1, ?2, ?3, 'cbz', ?4, COALESCE(?5, unixepoch()), ?6)
             ON CONFLICT(id) DO NOTHING",
            params![
                self.id,
                self.title,
                self.author,
                self.count,
                added_at,
                status
            ],
        )? != 0;
        if inserted {
            db.execute(
                "INSERT INTO comic_archive(book_id, blob_hash) VALUES (?1, ?2)",
                params![self.id, self.hash],
            )?;
            // Each page occupies a chapter locator so progress uses the same
            // foreign key and chapter/paragraph coordinate system as text.
            let body = zstd::bulk::compress(&[], 3)?;
            let mut stmt = db.prepare_cached(
                "INSERT INTO chapter(book_id, idx, title, byte_len, body)
                 VALUES (?1, ?2, ?3, 0, ?4)",
            )?;
            for index in 0..self.count {
                stmt.execute(params![self.id, index, (index + 1).to_string(), body])?;
            }
        }
        covers::insert(db, &self.id, self.cover.as_ref())?;
        Ok(inserted)
    }
}

fn hash_copy(
    input: impl Read,
    output: &mut impl Write,
    expected_sha256: Option<&str>,
) -> Result<String> {
    let mut input = input.take(MAX_ARCHIVE_BYTES + 1);
    let mut buffer = [0; 64 * 1024];
    let mut hasher = blake3::Hasher::new();
    let mut sha256 = expected_sha256.map(|_| Sha256::new());
    while input.limit() != 0 {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            if let Some((sha256, expected)) = sha256.zip(expected_sha256) {
                if !format!("{:x}", sha256.finalize()).eq_ignore_ascii_case(expected) {
                    return Err(Error::Invalid("legacy comic source SHA-256 differs".into()));
                }
            }
            return Ok(hasher.finalize().to_hex().to_string());
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        if let Some(sha256) = &mut sha256 {
            sha256.update(&buffer[..read]);
        }
    }
    Err(Error::Invalid("CBZ is larger than 512 MiB".into()))
}

fn blob_path(blobs: &Path, hash: &str) -> Result<PathBuf> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Invalid("comic blob has an invalid hash".into()));
    }
    Ok(blobs.join(format!("{hash}.cbz")))
}
