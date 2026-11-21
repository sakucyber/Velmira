use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, TransactionBehavior};
use tempfile::NamedTempFile;
use vm_core::BookSummary;
use vm_format::CbzReader;

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
    pub(super) fn read_page(
        &mut self,
        db: &Connection,
        blobs: &Path,
        book_id: &str,
        index: u32,
    ) -> Result<ComicPage> {
        if self.active.as_ref().is_none_or(|(id, _)| id != book_id) {
            let hash: String = db
                .prepare_cached("SELECT blob_hash FROM comic_archive WHERE book_id = ?1")?
                .query_row([book_id], |row| row.get(0))?;
            let reader = CbzReader::open(&blob_path(blobs, &hash)?)?;
            self.active = Some((book_id.to_owned(), reader));
        }
        let (_, reader) = self.active.as_mut().expect("reader opened above");
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
    let input = File::open(path)?;
    let info = input.metadata()?;
    if !info.is_file() || info.len() > MAX_ARCHIVE_BYTES {
        return Err(Error::Invalid(
            "CBZ must be a file no larger than 512 MiB".into(),
        ));
    }
    fs::create_dir_all(blobs)?;
    let mut copy = NamedTempFile::new_in(blobs)?;
    let hash = hash_copy(input, &mut copy)?;
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
        if !existing.metadata()?.is_file() || hash_copy(existing, &mut io::sink())? != hash {
            return Err(Error::Invalid(
                "existing comic blob has an invalid content hash".into(),
            ));
        }
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inserted = tx.execute(
        "INSERT INTO book(id, title, author, format, chapter_count)
         VALUES (?1, ?2, ?3, 'cbz', ?4) ON CONFLICT(id) DO NOTHING",
        params![id, title, author, count],
    )? != 0;
    if inserted {
        tx.execute(
            "INSERT INTO comic_archive(book_id, blob_hash) VALUES (?1, ?2)",
            params![id, hash],
        )?;
        // Each page occupies a chapter locator so progress uses the same
        // foreign key and chapter/paragraph coordinate system as text.
        let body = zstd::bulk::compress(&[], 3)?;
        let mut stmt = tx.prepare_cached(
            "INSERT INTO chapter(book_id, idx, title, byte_len, body)
             VALUES (?1, ?2, ?3, 0, ?4)",
        )?;
        for index in 0..count {
            stmt.execute(params![id, index, (index + 1).to_string(), body])?;
        }
    } else {
        database::restore_reimport(&tx, &id)?;
    }
    covers::insert(&tx, &id, cover.as_ref())?;
    let book = database::summary(&tx, &id)?;
    tx.commit()?;
    Ok((book, !inserted))
}

fn hash_copy(input: impl Read, output: &mut impl Write) -> Result<String> {
    let mut input = input.take(MAX_ARCHIVE_BYTES + 1);
    let mut buffer = [0; 64 * 1024];
    let mut hasher = blake3::Hasher::new();
    while input.limit() != 0 {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    Err(Error::Invalid("CBZ is larger than 512 MiB".into()))
}

fn blob_path(blobs: &Path, hash: &str) -> Result<PathBuf> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Invalid("comic blob has an invalid hash".into()));
    }
    Ok(blobs.join(format!("{hash}.cbz")))
}
