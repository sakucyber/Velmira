use std::{
    io::{Seek, Write},
    path::Path,
    time::Duration,
};

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::write::SimpleFileOptions;

use crate::{image_extension, ComicPage, Storyboard, MAX_CHAPTER_CHARS};

pub mod automatic;

const MAX_PAGES: u32 = 128;
const MAX_CBZ_BYTES: i64 = 512 * 1024 * 1024;
// The reader applies its limit to the entire ZIP file, including its entry headers and directory.
const CBZ_HEADER_RESERVE: i64 = 16 * 1024;
const CBZ_SOURCE_RESERVE: i64 = 2048;
const MAX_PROJECT_IMAGE_BYTES: i64 = MAX_CBZ_BYTES - CBZ_HEADER_RESERVE - CBZ_SOURCE_RESERVE;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("漫画项目数据库错误：{0}")]
    Database(#[from] rusqlite::Error),
    #[error("漫画项目文件错误：{0}")]
    File(#[from] std::io::Error),
    #[error("漫画项目格式错误：{0}")]
    Format(#[from] serde_json::Error),
    #[error("漫画页不存在或已在其他窗口修改，请重新加载。")]
    Conflict,
    #[error("无效的原文、分镜或书籍标识。")]
    InvalidDraft,
    #[error("一个项目最多可包含 {MAX_PAGES} 页。")]
    TooManyPages,
    #[error("单页图像太大或格式不受支持。")]
    InvalidImage,
    #[error("请先生成项目中的所有漫画页再导出。")]
    Incomplete,
    #[error("生成的 CBZ 将超过 512 MiB 大小上限。")]
    TooLarge,
    #[error("写入 CBZ 失败：{0}")]
    Archive(#[from] zip::result::ZipError),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Automatic,
    Rows,
    Columns,
    LeadPanel,
}

impl Layout {
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Automatic => "",
            Self::Rows => " Use evenly spaced horizontal rows with consistent margins.",
            Self::Columns => {
                " Use a two-column grid with equal column widths and consistent margins."
            }
            Self::LeadPanel => {
                " Set the first panel across the full top width, with smaller panels below it."
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Draft {
    pub chapter: u32,
    pub source: String,
    pub layout: Layout,
    pub storyboard: Option<Storyboard>,
}

impl Draft {
    fn validate(&self) -> Result<(), ProjectError> {
        if self.source.trim().is_empty()
            || self.source.chars().take(MAX_CHAPTER_CHARS + 1).count() > MAX_CHAPTER_CHARS
            || self
                .storyboard
                .as_ref()
                .is_some_and(|board| board.validate(&self.source).is_err())
        {
            return Err(ProjectError::InvalidDraft);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub id: i64,
    pub revision: u32,
    pub draft: Draft,
    pub rendered: bool,
}

pub struct Project {
    db: Connection,
    book_id: String,
}

impl Project {
    pub fn open(path: &Path, book_id: &str) -> Result<Self, ProjectError> {
        if book_id.is_empty() || book_id.len() > 128 || book_id.chars().any(char::is_control) {
            return Err(ProjectError::InvalidDraft);
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS manga_page (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                book_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1,
                draft TEXT NOT NULL,
                image BLOB,
                UNIQUE(book_id, position)
            );",
        )?;
        Ok(Self {
            db,
            book_id: book_id.to_owned(),
        })
    }

    pub fn pages(&self) -> Result<Vec<Page>, ProjectError> {
        let mut statement = self.db.prepare(
            "SELECT id, revision, draft, image IS NOT NULL
             FROM manga_page WHERE book_id = ?1 ORDER BY position",
        )?;
        let rows = statement.query_map([&self.book_id], read_page)?;
        rows.map(|row| row.map_err(ProjectError::from)).collect()
    }

    pub fn add(&mut self, draft: Draft) -> Result<Page, ProjectError> {
        self.add_all(vec![draft]).map(|mut pages| pages.remove(0))
    }

    pub(super) fn add_all(&mut self, drafts: Vec<Draft>) -> Result<Vec<Page>, ProjectError> {
        for draft in &drafts {
            draft.validate()?;
        }
        let write = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (count, position): (u32, i64) = write.query_row(
            "SELECT COUNT(*), IFNULL(MAX(position), 0) + 1 FROM manga_page WHERE book_id = ?1",
            [&self.book_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if drafts.len() > (MAX_PAGES - count.min(MAX_PAGES)) as usize {
            return Err(ProjectError::TooManyPages);
        }
        let mut pages = Vec::with_capacity(drafts.len());
        for (index, draft) in drafts.into_iter().enumerate() {
            let raw = serde_json::to_string(&draft)?;
            let (id, revision) = write.query_row(
                "INSERT INTO manga_page(book_id, position, draft) VALUES (?1, ?2, ?3)
                 RETURNING id, revision",
                params![self.book_id, position + index as i64, raw],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            pages.push(Page {
                id,
                revision,
                draft,
                rendered: false,
            });
        }
        write.commit()?;
        Ok(pages)
    }

    pub fn save(&mut self, mut page: Page) -> Result<Page, ProjectError> {
        page.draft.validate()?;
        let draft = serde_json::to_string(&page.draft)?;
        let write = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        (page.revision, page.rendered) = write
            .query_row(
                "UPDATE manga_page SET revision = revision + 1, draft = ?1,
                 image = CASE WHEN ?1 = draft THEN image ELSE NULL END
                 WHERE book_id = ?2 AND id = ?3 AND revision = ?4
                 RETURNING revision, image IS NOT NULL",
                params![draft, self.book_id, page.id, page.revision],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(ProjectError::Conflict)?;
        write.commit()?;
        Ok(page)
    }

    pub fn store_render(&mut self, page: &Page, result: &ComicPage) -> Result<(), ProjectError> {
        let board = page.draft.storyboard.as_ref();
        if board != Some(&result.storyboard) {
            return Err(ProjectError::InvalidDraft);
        }
        if result.image.len() > vm_media::MAX_IMAGE_BYTES || image_extension(&result.image).is_err()
        {
            return Err(ProjectError::InvalidImage);
        }
        let write = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let other_images: i64 = write.query_row(
            "SELECT COALESCE(SUM(LENGTH(image)), 0) FROM manga_page WHERE book_id = ?1 AND id != ?2",
            params![self.book_id, page.id],
            |row| row.get(0),
        )?;
        if other_images + result.image.len() as i64 > MAX_PROJECT_IMAGE_BYTES {
            return Err(ProjectError::TooLarge);
        }
        expect_change(write.execute(
            "UPDATE manga_page SET revision = revision + 1, image = ?1
             WHERE book_id = ?2 AND id = ?3 AND revision = ?4 AND draft = ?5",
            params![
                result.image,
                self.book_id,
                page.id,
                page.revision,
                serde_json::to_string(&page.draft)?
            ],
        )?)?;
        write.commit()?;
        Ok(())
    }

    pub fn remove(&self, page: &Page) -> Result<(), ProjectError> {
        expect_change(self.db.execute(
            "DELETE FROM manga_page WHERE book_id = ?1 AND id = ?2 AND revision = ?3",
            params![self.book_id, page.id, page.revision],
        )?)
    }

    pub(super) fn clear(&self) -> Result<(), ProjectError> {
        self.db
            .execute("DELETE FROM manga_page WHERE book_id = ?1", [&self.book_id])?;
        Ok(())
    }

    pub fn preview(&self, page: &Page) -> Result<vm_media::RgbaImage, ProjectError> {
        let image: Vec<u8> = self
            .db
            .query_row(
                "SELECT image FROM manga_page WHERE book_id = ?1 AND id = ?2 AND revision = ?3
                 AND image IS NOT NULL",
                params![self.book_id, page.id, page.revision],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ProjectError::Conflict)?;
        vm_media::decode_page(&image, 1024).map_err(|_| ProjectError::InvalidImage)
    }

    pub fn move_page(&mut self, page: &Page, later: bool) -> Result<(), ProjectError> {
        let write = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let position: i64 = write
            .query_row(
                "SELECT position FROM manga_page WHERE book_id = ?1 AND id = ?2 AND revision = ?3",
                params![self.book_id, page.id, page.revision],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ProjectError::Conflict)?;
        let neighbor = if later {
            "SELECT id, position FROM manga_page WHERE book_id = ?1 AND position > ?2 ORDER BY position LIMIT 1"
        } else {
            "SELECT id, position FROM manga_page WHERE book_id = ?1 AND position < ?2 ORDER BY position DESC LIMIT 1"
        };
        if let Some((other, other_position)) = write
            .query_row(neighbor, params![self.book_id, position], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?
        {
            write.execute(
                "UPDATE manga_page SET position = 0 WHERE id = ?1",
                [page.id],
            )?;
            write.execute(
                "UPDATE manga_page SET position = ?1, revision = revision + 1 WHERE id = ?2",
                params![position, other],
            )?;
            write.execute(
                "UPDATE manga_page SET position = ?1, revision = revision + 1 WHERE id = ?2",
                params![other_position, page.id],
            )?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn export_cbz(&mut self, output: impl Write + Seek) -> Result<u32, ProjectError> {
        let novel_id = self.book_id.clone();
        self.export_for_novel(output, &novel_id)
    }

    fn export_for_novel(
        &mut self,
        output: impl Write + Seek,
        novel_id: &str,
    ) -> Result<u32, ProjectError> {
        let read = self.db.transaction()?;
        let (total, rendered, bytes): (u32, u32, i64) = read.query_row(
            "SELECT COUNT(*), COUNT(image), COALESCE(SUM(LENGTH(image)), 0)
             FROM manga_page WHERE book_id = ?1",
            [&self.book_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if total == 0 || total != rendered {
            return Err(ProjectError::Incomplete);
        }
        if bytes > MAX_PROJECT_IMAGE_BYTES {
            return Err(ProjectError::TooLarge);
        }
        let mut archive = zip::ZipWriter::new(output);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let chapter_list = {
            let mut statement =
                read.prepare("SELECT draft FROM manga_page WHERE book_id = ?1 ORDER BY position")?;
            let rows = statement.query_map([&self.book_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let draft: Draft = serde_json::from_str(&row?)?;
                Ok(draft.chapter)
            })
            .collect::<Result<Vec<_>, ProjectError>>()?
        };
        let source = vm_core::ComicSource {
            version: 1,
            novel_id: novel_id.to_owned(),
            chapters: chapter_list,
        };
        if source.valid_for(total as usize) {
            archive.start_file(vm_core::COMIC_SOURCE_ENTRY, options)?;
            serde_json::to_writer(&mut archive, &source)?;
        }
        let mut statement =
            read.prepare("SELECT image FROM manga_page WHERE book_id = ?1 ORDER BY position")?;
        let images = statement.query_map([&self.book_id], |row| row.get::<_, Vec<u8>>(0))?;
        for (index, image) in images.enumerate() {
            let image = image?;
            let extension = image_extension(&image).map_err(|_| ProjectError::InvalidImage)?;
            archive.start_file(format!("{:03}.{extension}", index + 1), options)?;
            archive.write_all(&image)?;
        }
        archive.finish()?.flush()?;
        Ok(total)
    }
}

fn read_page(row: &Row<'_>) -> rusqlite::Result<Page> {
    let raw: String = row.get(2)?;
    Ok(Page {
        id: row.get(0)?,
        revision: row.get(1)?,
        draft: serde_json::from_str(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        rendered: row.get(3)?,
    })
}

fn expect_change(rows: usize) -> Result<(), ProjectError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(ProjectError::Conflict)
    }
}
