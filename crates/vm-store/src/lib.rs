//! SQLite-backed storage for the native reader. A dedicated thread owns the connection.

mod comics;
mod covers;
mod database;
mod illustrations;
mod legacy;
mod reading_marks;
mod replication;

use std::{path::PathBuf, thread};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use thiserror::Error;
use vm_core::{BookFormat, BookId, BookSummary, Chapter, ImportedText, Loc};

pub use comics::ComicPage;
pub use covers::CoverSize;
pub use illustrations::{Illustration, IllustrationImage};
pub use legacy::MigrationReport;
pub use reading_marks::{Annotation, AnnotationColor, Bookmark};
pub use replication::Replication;

const COMMAND_CAPACITY: usize = 16;
const EVENT_CAPACITY: usize = 16;

pub type Result<T> = std::result::Result<T, Error>;

/// Read a chapter from an existing library for a background job. The job
/// opens its own read-only connection and never changes the reading location.
pub fn read_chapter(path: &std::path::Path, book_id: &str, index: u32) -> Result<Chapter> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(3))?;
    database::chapter(&db, book_id, index)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("File or compression: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid legacy JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Cannot read the book format: {0}")]
    Format(#[from] vm_format::FormatError),
    #[error("The storage queue is full")]
    Busy,
    #[error("The storage worker is unavailable")]
    Unavailable,
    #[error("Invalid or unsupported library data: {0}")]
    Invalid(String),
    #[error("The annotation changed; reload it before saving or deleting")]
    AnnotationConflict,
    #[error("Synchronization: {0}")]
    Sync(#[from] vm_sync::SyncError),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ShelfStatus {
    #[default]
    Active,
    Removed,
}

impl ShelfStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Removed => "removed",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ShelfSort {
    #[default]
    Newest,
    Title,
    LastRead,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReadingStatus {
    #[default]
    All,
    Unread,
    Reading,
    LastChapter,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShelfFilter {
    pub status: ShelfStatus,
    pub format: Option<BookFormat>,
    pub query: String,
    pub sort: ShelfSort,
    pub reading: ReadingStatus,
}

#[derive(Debug)]
pub enum Command {
    ListBooks {
        offset: u32,
    },
    ListShelf {
        offset: u32,
        filter: ShelfFilter,
    },
    ListComics {
        offset: u32,
    },
    RenameBook {
        book_id: BookId,
        title: String,
    },
    SetBookStatus {
        book_id: BookId,
        status: ShelfStatus,
    },
    InsertBook {
        book: ImportedText,
    },
    InsertEpub {
        book: ImportedText,
        author: Option<String>,
    },
    ImportEpub {
        path: PathBuf,
    },
    ImportCbz {
        path: PathBuf,
    },
    ReadCover {
        book_id: BookId,
        size: CoverSize,
    },
    ListIllustrations {
        book_id: BookId,
        chapter: u32,
    },
    ReadIllustration {
        book_id: BookId,
        chapter: u32,
        ordinal: u32,
    },
    OpenBook {
        book_id: BookId,
    },
    OpenCompanion {
        novel_id: BookId,
        book_id: BookId,
    },
    ReadChapter {
        book_id: BookId,
        chapter: u32,
    },
    ReadComicPage {
        book_id: BookId,
        page_index: u32,
    },
    SaveProgress {
        book_id: BookId,
        loc: Loc,
    },
    Search {
        query: String,
        book_id: BookId,
        chapter: Option<u32>,
        limit: u32,
    },
    MigrateLegacy {
        data_dir: PathBuf,
    },
    ListBookmarks {
        book_id: BookId,
        offset: u32,
    },
    AddBookmark {
        book_id: BookId,
        loc: Loc,
        label: Option<String>,
    },
    DeleteBookmark {
        book_id: BookId,
        loc: Loc,
    },
    ListAnnotations {
        book_id: BookId,
        chapter: u32,
    },
    AddAnnotation {
        book_id: BookId,
        loc: Loc,
        end_off: u32,
        note: Option<String>,
        color: AnnotationColor,
    },
    AddAnnotations {
        book_id: BookId,
        ranges: Vec<(Loc, u32)>,
        note: Option<String>,
        color: AnnotationColor,
    },
    UpdateAnnotation {
        book_id: BookId,
        id: String,
        expected_revision: u32,
        note: Option<String>,
        color: AnnotationColor,
    },
    DeleteAnnotation {
        book_id: BookId,
        id: String,
        expected_revision: u32,
    },
}

impl Command {
    fn operation(&self) -> &'static str {
        match self {
            Self::ListBooks { .. } | Self::ListShelf { .. } => "list_books",
            Self::ListComics { .. } => "list_comics",
            Self::RenameBook { .. } => "rename_book",
            Self::SetBookStatus { .. } => "set_book_status",
            Self::InsertBook { .. }
            | Self::InsertEpub { .. }
            | Self::ImportEpub { .. }
            | Self::ImportCbz { .. } => "insert_book",
            Self::ReadCover { .. } => "read_cover",
            Self::ListIllustrations { .. } => "list_illustrations",
            Self::ReadIllustration { .. } => "read_illustration",
            Self::OpenBook { .. } => "open_book",
            Self::OpenCompanion { .. } => "open_companion",
            Self::ReadChapter { .. } => "read_chapter",
            Self::ReadComicPage { .. } => "read_comic_page",
            Self::SaveProgress { .. } => "save_progress",
            Self::Search { .. } => "search",
            Self::MigrateLegacy { .. } => "migrate_legacy",
            Self::ListBookmarks { .. } => "list_bookmarks",
            Self::AddBookmark { .. } => "add_bookmark",
            Self::DeleteBookmark { .. } => "delete_bookmark",
            Self::ListAnnotations { .. } => "list_annotations",
            Self::AddAnnotation { .. } | Self::AddAnnotations { .. } => "add_annotation",
            Self::UpdateAnnotation { .. } => "update_annotation",
            Self::DeleteAnnotation { .. } => "delete_annotation",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Ready,
    Books {
        offset: u32,
        books: Vec<BookSummary>,
        has_more: bool,
    },
    Shelf {
        offset: u32,
        filter: ShelfFilter,
        books: Vec<BookSummary>,
        has_more: bool,
    },
    Comics {
        offset: u32,
        books: Vec<BookSummary>,
        has_more: bool,
    },
    BookUpdated {
        book: BookSummary,
        status: ShelfStatus,
    },
    CoverRead {
        book_id: BookId,
        size: CoverSize,
        bytes: Option<Vec<u8>>,
    },
    Illustrations {
        book_id: BookId,
        chapter: u32,
        illustrations: Vec<Illustration>,
    },
    IllustrationRead {
        book_id: BookId,
        chapter: u32,
        ordinal: u32,
        image: IllustrationImage,
    },
    BookInserted {
        book: BookSummary,
        duplicate: bool,
    },
    BookOpened {
        book: BookSummary,
        toc: Vec<String>,
        chapter_index: u32,
        chapter: Chapter,
    },
    ComicOpened {
        book: BookSummary,
        page: ComicPage,
    },
    CompanionOpened {
        novel_id: BookId,
        book: BookSummary,
        page: ComicPage,
        source: Option<vm_core::ComicSource>,
    },
    ChapterRead {
        book_id: BookId,
        chapter_index: u32,
        chapter: Chapter,
    },
    ComicPageRead {
        book_id: BookId,
        page: ComicPage,
    },
    ProgressSaved {
        book_id: BookId,
        loc: Loc,
    },
    SearchResults {
        query: String,
        book_id: BookId,
        hits: Vec<SearchHit>,
    },
    MigrationFinished(MigrationReport),
    Bookmarks {
        book_id: BookId,
        offset: u32,
        bookmarks: Vec<Bookmark>,
        has_more: bool,
    },
    BookmarkAdded {
        book_id: BookId,
        bookmark: Bookmark,
    },
    BookmarkDeleted {
        book_id: BookId,
        loc: Loc,
    },
    Annotations {
        book_id: BookId,
        chapter: u32,
        annotations: Vec<Annotation>,
    },
    AnnotationSaved {
        book_id: BookId,
        annotation: Annotation,
    },
    AnnotationsAdded {
        book_id: BookId,
        chapter: u32,
    },
    AnnotationDeleted {
        book_id: BookId,
        id: String,
        chapter: u32,
    },
    Error {
        operation: &'static str,
        message: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub loc: Loc,
    pub excerpt: String,
}

/// The UI can submit commands and poll results without waiting for filesystem I/O.
pub struct Store {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

impl Store {
    /// Initialization happens on the storage thread. Check for `Ready` or an
    /// `Error` with operation `open_database` before using the library.
    pub fn spawn(path: PathBuf, wake: impl Fn() + Send + 'static) -> Result<Self> {
        let (commands, incoming) = bounded(COMMAND_CAPACITY);
        let (outgoing, events) = bounded(EVENT_CAPACITY);
        thread::Builder::new()
            .name("velmira-store".into())
            .spawn(move || run_worker(path, incoming, outgoing, wake))?;
        Ok(Self { commands, events })
    }

    pub fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::Unavailable,
            })
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }
}

fn run_worker(
    path: PathBuf,
    incoming: Receiver<Command>,
    outgoing: Sender<Event>,
    wake: impl Fn(),
) {
    let mut db = match database::open(&path) {
        Ok(db) => db,
        Err(error) => {
            let _ = outgoing.send(Event::Error {
                operation: "open_database",
                message: error.to_string(),
            });
            wake();
            return;
        }
    };
    if outgoing.send(Event::Ready).is_err() {
        return;
    }
    wake();

    let blobs = path.with_extension("comic-blobs");
    let mut comics = comics::ComicReader::default();
    for command in incoming {
        let operation = command.operation();
        let event =
            handle(&mut db, &blobs, &mut comics, command).unwrap_or_else(|error| Event::Error {
                operation,
                message: error.to_string(),
            });
        if outgoing.send(event).is_err() {
            break;
        }
        wake();
    }
}

fn handle(
    db: &mut rusqlite::Connection,
    blobs: &std::path::Path,
    comics: &mut comics::ComicReader,
    command: Command,
) -> Result<Event> {
    Ok(match command {
        Command::ListBooks { offset } => {
            let (books, has_more) = database::list_books(db, offset)?;
            Event::Books {
                offset,
                books,
                has_more,
            }
        }
        Command::ListShelf { offset, filter } => {
            let (books, has_more) = database::list_shelf(db, offset, &filter)?;
            Event::Shelf {
                offset,
                filter,
                books,
                has_more,
            }
        }
        Command::ListComics { offset } => {
            let filter = ShelfFilter {
                format: Some(BookFormat::Cbz),
                ..ShelfFilter::default()
            };
            let (books, has_more) = database::list_shelf(db, offset, &filter)?;
            Event::Comics {
                offset,
                books,
                has_more,
            }
        }
        Command::RenameBook { book_id, title } => {
            let (book, status) = database::rename_book(db, &book_id, &title)?;
            Event::BookUpdated { book, status }
        }
        Command::SetBookStatus { book_id, status } => {
            let book = database::set_book_status(db, &book_id, status)?;
            Event::BookUpdated { book, status }
        }
        Command::InsertBook { book } => {
            let (book, duplicate) = database::insert_text(db, book, BookFormat::Txt, None)?;
            Event::BookInserted { book, duplicate }
        }
        Command::InsertEpub { book, author } => {
            let (book, duplicate) = database::insert_text(db, book, BookFormat::Epub, author)?;
            Event::BookInserted { book, duplicate }
        }
        Command::ImportEpub { path } => {
            let (book, duplicate) = covers::import_epub(db, &path)?;
            Event::BookInserted { book, duplicate }
        }
        Command::ImportCbz { path } => {
            let (book, duplicate) = comics::import(db, blobs, &path)?;
            Event::BookInserted { book, duplicate }
        }
        Command::ReadCover { book_id, size } => {
            let bytes = covers::read(db, &book_id, size)?;
            Event::CoverRead {
                book_id,
                size,
                bytes,
            }
        }
        Command::ListIllustrations { book_id, chapter } => {
            let illustrations = illustrations::list(db, &book_id, chapter)?;
            Event::Illustrations {
                book_id,
                chapter,
                illustrations,
            }
        }
        Command::ReadIllustration {
            book_id,
            chapter,
            ordinal,
        } => {
            let image = illustrations::read(db, &book_id, chapter, ordinal)?;
            Event::IllustrationRead {
                book_id,
                chapter,
                ordinal,
                image,
            }
        }
        Command::OpenBook { book_id } => {
            let book = database::summary(db, &book_id)?;
            let chapter_index = book.progress.as_ref().map_or(0, |loc| loc.chapter);
            if book.format == BookFormat::Cbz {
                let page = comics.read_page(db, blobs, &book_id, chapter_index)?;
                Event::ComicOpened { book, page }
            } else {
                let toc = database::toc(db, &book_id, book.chapter_count)?;
                let chapter = database::chapter(db, &book_id, chapter_index)?;
                Event::BookOpened {
                    book,
                    toc,
                    chapter_index,
                    chapter,
                }
            }
        }
        Command::ReadChapter { book_id, chapter } => {
            let body = database::chapter(db, &book_id, chapter)?;
            Event::ChapterRead {
                book_id,
                chapter_index: chapter,
                chapter: body,
            }
        }
        Command::OpenCompanion { novel_id, book_id } => {
            let book = database::summary(db, &book_id)?;
            if book.format != BookFormat::Cbz {
                return Err(Error::Invalid(
                    "parallel reading requires a CBZ comic".into(),
                ));
            }
            let index = book.progress.as_ref().map_or(0, |loc| loc.chapter);
            let page = comics.read_page(db, blobs, &book_id, index)?;
            let source = comics
                .source(db, blobs, &book_id)?
                .filter(|source| source.novel_id == novel_id);
            Event::CompanionOpened {
                novel_id,
                book,
                page,
                source,
            }
        }
        Command::ReadComicPage {
            book_id,
            page_index,
        } => {
            let page = comics.read_page(db, blobs, &book_id, page_index)?;
            Event::ComicPageRead { book_id, page }
        }
        Command::SaveProgress { book_id, loc } => {
            database::save_progress(db, &book_id, &loc)?;
            Event::ProgressSaved { book_id, loc }
        }
        Command::Search {
            query,
            book_id,
            chapter,
            limit,
        } => {
            let hits = database::search(db, &book_id, chapter, &query, limit)?;
            Event::SearchResults {
                query,
                book_id,
                hits,
            }
        }
        Command::MigrateLegacy { data_dir } => {
            Event::MigrationFinished(legacy::migrate(db, blobs, &data_dir)?)
        }
        Command::ListBookmarks { book_id, offset } => {
            let (bookmarks, has_more) = reading_marks::list_bookmarks(db, &book_id, offset)?;
            Event::Bookmarks {
                book_id,
                offset,
                bookmarks,
                has_more,
            }
        }
        Command::AddBookmark {
            book_id,
            loc,
            label,
        } => {
            let bookmark = reading_marks::add_bookmark(db, &book_id, loc, label.as_deref())?;
            Event::BookmarkAdded { book_id, bookmark }
        }
        Command::DeleteBookmark { book_id, loc } => {
            reading_marks::delete_bookmark(db, &book_id, loc)?;
            Event::BookmarkDeleted { book_id, loc }
        }
        Command::ListAnnotations { book_id, chapter } => {
            let annotations = reading_marks::list_annotations(db, &book_id, chapter)?;
            Event::Annotations {
                book_id,
                chapter,
                annotations,
            }
        }
        Command::AddAnnotation {
            book_id,
            loc,
            end_off,
            note,
            color,
        } => {
            let annotation =
                reading_marks::add_annotation(db, &book_id, loc, end_off, note.as_deref(), color)?;
            Event::AnnotationSaved {
                book_id,
                annotation,
            }
        }
        Command::AddAnnotations {
            book_id,
            ranges,
            note,
            color,
        } => {
            let chapter =
                reading_marks::add_annotations(db, &book_id, &ranges, note.as_deref(), color)?;
            Event::AnnotationsAdded { book_id, chapter }
        }
        Command::UpdateAnnotation {
            book_id,
            id,
            expected_revision,
            note,
            color,
        } => {
            let annotation = reading_marks::update_annotation(
                db,
                &book_id,
                &id,
                expected_revision,
                note.as_deref(),
                color,
            )?;
            Event::AnnotationSaved {
                book_id,
                annotation,
            }
        }
        Command::DeleteAnnotation {
            book_id,
            id,
            expected_revision,
        } => {
            let chapter = reading_marks::delete_annotation(db, &book_id, &id, expected_revision)?;
            Event::AnnotationDeleted {
                book_id,
                id,
                chapter,
            }
        }
    })
}
