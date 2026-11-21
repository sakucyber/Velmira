use std::{fs, io::Write, time::Duration};

use rusqlite::{params, Connection};
use vm_core::{BookFormat, ImportedChapter, ImportedText, Loc, TxtEncoding};

use crate::{database, legacy, reading_marks, AnnotationColor, Command, Error, Event, Store};
use crate::{CoverSize, ShelfFilter, ShelfSort, ShelfStatus};

fn sample() -> ImportedText {
    ImportedText {
        title: "测试小说".into(),
        encoding: TxtEncoding::Utf8,
        chapters: vec![
            ImportedChapter {
                title: "第一章".into(),
                paragraphs: vec!["月🌕照在山河上".into(), "Literal * and \"quotes\".".into()],
            },
            ImportedChapter {
                title: "第二章".into(),
                paragraphs: Vec::new(),
            },
        ],
    }
}

#[test]
fn worker_persists_book_and_unicode_reading_position() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile").join("velmira.db");
    let store = Store::spawn(path.clone(), || {}).unwrap();
    let next = || store.events().recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(next(), Event::Ready));
    for duplicate in [false, true] {
        store.send(Command::InsertBook { book: sample() }).unwrap();
        assert!(
            matches!(next(), Event::BookInserted { duplicate: value, .. } if value == duplicate)
        );
    }
    store.send(Command::ListBooks { offset: 0 }).unwrap();
    let Event::Books {
        mut books,
        has_more: false,
        offset: 0,
    } = next()
    else {
        panic!("expected one shelf page");
    };
    assert_eq!(books.len(), 1);
    let book = books.pop().unwrap();
    let loc = Loc {
        chapter: 0,
        para: 0,
        off: 3,
    };
    store
        .send(Command::SaveProgress {
            book_id: book.id.clone(),
            loc,
        })
        .unwrap();
    assert!(matches!(next(), Event::ProgressSaved { loc: value, .. } if value == loc));
    drop(store);

    let reopened = Store::spawn(path, || {}).unwrap();
    let next = || {
        reopened
            .events()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    };
    assert!(matches!(next(), Event::Ready));
    reopened
        .send(Command::OpenBook { book_id: book.id })
        .unwrap();
    let Event::BookOpened {
        book,
        toc,
        chapter_index,
        chapter,
    } = next()
    else {
        panic!("expected a chapter");
    };
    assert_eq!(book.progress, Some(loc));
    assert_eq!(chapter_index, 0);
    assert_eq!(toc, ["第一章", "第二章"]);
    assert_eq!(chapter.paragraphs, sample().chapters[0].paragraphs);
}

#[test]
fn wide_and_short_searches_use_scalar_positions_and_literal_quotes() {
    let mut db = database::open(std::path::Path::new(":memory:")).unwrap();
    let (book, _) = database::insert_text(&mut db, sample(), BookFormat::Txt, None).unwrap();
    assert_eq!(
        database::chapter(&db, &book.id, 1)
            .unwrap()
            .paragraphs
            .len(),
        0
    );
    assert_eq!(
        database::search(&db, &book.id, None, "在山河", 20).unwrap()[0]
            .loc
            .off,
        3
    );
    assert_eq!(
        database::search(&db, &book.id, None, "山河", 20).unwrap()[0]
            .loc
            .off,
        4
    );
    assert_eq!(
        database::search(&db, &book.id, Some(0), "🌕", 20).unwrap()[0]
            .loc
            .off,
        1
    );
    assert_eq!(
        database::search(&db, &book.id, None, r#""quotes""#, 20).unwrap()[0]
            .loc
            .para,
        1
    );
    assert_eq!(
        database::search(&db, &book.id, None, "LITERAL", 20).unwrap()[0]
            .loc
            .off,
        0
    );
    assert!(database::search(&db, &book.id, Some(1), "在山河", 20)
        .unwrap()
        .is_empty());
    assert!(database::search(&db, &book.id, None, "河", 20).is_err());
    db.execute("DELETE FROM book WHERE id = ?1", [&book.id])
        .unwrap();
    assert!(database::search(&db, &book.id, None, "山河", 20)
        .unwrap()
        .is_empty());
}

#[test]
fn imports_legacy_text_positions_once_without_writing_source() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("app.db");
    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE books(id, title, author, format, sha256, encoding, chapter_count, created_at);
             CREATE TABLE chapters(book_id, idx, title, paras_json);
             CREATE TABLE progress(book_id, chapter, para, offset, updated_at);
             CREATE TABLE bookmarks(id, book_id, chapter, para, offset, label, created_at);
             CREATE TABLE annotations(id, book_id, chapter, para, start_offset, end_offset, text, note, color, created_at, updated_at);
             INSERT INTO books VALUES ('a', '旧书', NULL, 'epub', 'hash-a', NULL, 1, 8);
             INSERT INTO books VALUES ('b', '旧漫画', NULL, 'cbz', 'hash-b', NULL, 3, 9);
             INSERT INTO progress VALUES ('a', 0, 0, 2, 10);
             INSERT INTO bookmarks VALUES ('mark', 'a', 0, 0, 1, '旧书签', 11);
             INSERT INTO bookmarks VALUES ('broken', 'a', 0, 5, 1, NULL, 12);
             INSERT INTO annotations VALUES ('note', 'a', 0, 0, 1, 2, '🌕', '笔记', 'yellow', 13, 14);"
        ).unwrap();
        old.execute(
            "INSERT INTO chapters VALUES ('a', 0, '第一章', ?1)",
            params![r#"["月🌕亮"]"#],
        )
        .unwrap();
    }
    let original = fs::read(&path).unwrap();
    let mut db = database::open(&temp.path().join("velmira.db")).unwrap();
    let report = legacy::migrate(&mut db, temp.path()).unwrap();
    assert_eq!(
        report,
        legacy::MigrationReport {
            books: 1,
            bookmarks: 1,
            annotations: 1,
            progress: 1,
            skipped_books: 1,
            skipped_positions: 1,
            already_migrated: false,
        }
    );
    let (books, _) = database::list_books(&db, 0).unwrap();
    assert_eq!(books[0].format, BookFormat::Epub);
    assert_eq!(books[0].progress.unwrap().off, 2);
    assert_eq!(
        database::chapter(&db, &books[0].id, 0).unwrap().paragraphs,
        ["月🌕亮"]
    );
    assert!(legacy::migrate(&mut db, &path).unwrap().already_migrated);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn invalid_legacy_chapter_rolls_back_completed_inserts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("app.db");
    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE books(id, title, author, format, sha256, encoding, chapter_count, created_at);
             CREATE TABLE chapters(book_id, idx, title, paras_json);
             INSERT INTO books VALUES ('a', 'first', NULL, 'txt', 'hash-a', 'utf-8', 1, 8);
             INSERT INTO books VALUES ('b', 'second', NULL, 'txt', 'hash-b', 'utf-8', 2, 8);"
        ).unwrap();
        old.execute(
            "INSERT INTO chapters VALUES ('a', 0, 'first', ?1)",
            params![r#"["paragraph"]"#],
        )
        .unwrap();
    }
    let mut db = database::open(&temp.path().join("velmira.db")).unwrap();
    assert!(legacy::migrate(&mut db, &path).is_err());
    assert!(database::list_books(&db, 0).unwrap().0.is_empty());
}

#[test]
fn native_marks_use_unicode_locations_and_revision_checks() {
    let mut db = database::open(std::path::Path::new(":memory:")).unwrap();
    let (book, _) = database::insert_text(&mut db, sample(), BookFormat::Txt, None).unwrap();
    let loc = Loc {
        chapter: 0,
        para: 0,
        off: 1,
    };
    let original = reading_marks::add_bookmark(&mut db, &book.id, loc, Some("bookmark")).unwrap();
    let repeated =
        reading_marks::add_bookmark(&mut db, &book.id, Loc { off: 2, ..loc }, None).unwrap();
    assert_eq!(repeated, original);
    assert_eq!(
        reading_marks::list_bookmarks(&db, &book.id, 0).unwrap().0,
        [original]
    );
    reading_marks::delete_bookmark(&db, &book.id, loc).unwrap();
    assert!(reading_marks::list_bookmarks(&db, &book.id, 0)
        .unwrap()
        .0
        .is_empty());

    let original = reading_marks::add_annotation(
        &mut db,
        &book.id,
        loc,
        2,
        Some("original note"),
        AnnotationColor::Yellow,
    )
    .unwrap();
    assert_eq!(original.text, "🌕");
    assert_eq!(original.revision, 1);
    assert!(reading_marks::add_annotation(
        &mut db,
        &book.id,
        loc,
        32,
        None,
        AnnotationColor::Yellow
    )
    .is_err());
    let updated = reading_marks::update_annotation(
        &db,
        &book.id,
        &original.id,
        original.revision,
        None,
        AnnotationColor::Blue,
    )
    .unwrap();
    assert!(matches!(
        reading_marks::update_annotation(
            &db,
            &book.id,
            &original.id,
            original.revision,
            Some("stale"),
            AnnotationColor::Pink
        ),
        Err(Error::AnnotationConflict)
    ));
    assert!(matches!(
        reading_marks::delete_annotation(&db, &book.id, &original.id, original.revision),
        Err(Error::AnnotationConflict)
    ));
    assert_eq!(
        reading_marks::list_annotations(&db, &book.id, 0).unwrap(),
        std::slice::from_ref(&updated)
    );
    assert_eq!(
        reading_marks::delete_annotation(&db, &book.id, &updated.id, updated.revision).unwrap(),
        0
    );
    assert!(reading_marks::list_annotations(&db, &book.id, 0)
        .unwrap()
        .is_empty());
}

#[test]
fn import_formats_have_separate_book_identity() {
    let mut db = database::open(std::path::Path::new(":memory:")).unwrap();
    let (txt, _) = database::insert_text(&mut db, sample(), BookFormat::Txt, None).unwrap();
    let (epub, duplicate) = database::insert_text(
        &mut db,
        sample(),
        BookFormat::Epub,
        Some("EPUB author".into()),
    )
    .unwrap();
    assert_ne!(txt.id, epub.id);
    assert_eq!(epub.format, BookFormat::Epub);
    assert!(!duplicate);
    assert!(
        database::insert_text(&mut db, sample(), BookFormat::Epub, None)
            .unwrap()
            .1
    );
}

#[test]
fn comic_reopens_from_library_after_source_is_removed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("velmira.db");
    let source = temp.path().join("comic.cbz");
    fs::write(&source, include_bytes!("../../../samples/demo-comic.cbz")).unwrap();
    let store = Store::spawn(path.clone(), || {}).unwrap();
    let next = || store.events().recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(next(), Event::Ready));
    store
        .send(Command::ImportCbz {
            path: source.clone(),
        })
        .unwrap();
    let Event::BookInserted {
        book,
        duplicate: false,
    } = next()
    else {
        panic!("the sample comic must import");
    };
    assert_eq!(book.format, BookFormat::Cbz);
    assert!(book.chapter_count > 1);
    store
        .send(Command::ImportCbz {
            path: source.clone(),
        })
        .unwrap();
    assert!(matches!(
        next(),
        Event::BookInserted {
            duplicate: true,
            ..
        }
    ));
    fs::remove_file(source).unwrap();
    store
        .send(Command::ReadComicPage {
            book_id: book.id.clone(),
            page_index: 1,
        })
        .unwrap();
    let Event::ComicPageRead { page, .. } = next() else {
        panic!("expected comic page");
    };
    assert_eq!(page.index, 1);
    assert!(page.media_type.starts_with("image/") && !page.bytes.is_empty());
    store
        .send(Command::ReadComicPage {
            book_id: book.id.clone(),
            page_index: book.chapter_count,
        })
        .unwrap();
    assert!(matches!(
        next(),
        Event::Error {
            operation: "read_comic_page",
            ..
        }
    ));
    let last = Loc {
        chapter: 1,
        para: 0,
        off: 0,
    };
    store
        .send(Command::SaveProgress {
            book_id: book.id.clone(),
            loc: last,
        })
        .unwrap();
    assert!(matches!(next(), Event::ProgressSaved { loc, .. } if loc == last));
    drop(store);

    let reopened = Store::spawn(path, || {}).unwrap();
    let next = || {
        reopened
            .events()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    };
    assert!(matches!(next(), Event::Ready));
    reopened
        .send(Command::OpenBook { book_id: book.id })
        .unwrap();
    let Event::ComicOpened {
        book,
        page: restored,
    } = next()
    else {
        panic!("expected opened comic");
    };
    assert_eq!(book.progress, Some(last));
    assert_eq!(restored.index, page.index);
    assert_eq!(restored.bytes, page.bytes);
    assert_cover_dimensions(&reopened, &book.id, CoverSize::Shelf, (400, 600));
}

fn assert_cover_dimensions(store: &Store, book_id: &str, size: CoverSize, dimensions: (u32, u32)) {
    store
        .send(Command::ReadCover {
            book_id: book_id.to_owned(),
            size,
        })
        .unwrap();
    let Event::CoverRead {
        book_id: id,
        size: found_size,
        bytes: Some(webp),
    } = store.events().recv_timeout(Duration::from_secs(5)).unwrap()
    else {
        panic!("expected persisted cover");
    };
    assert_eq!(id, book_id);
    assert_eq!(size, found_size);
    assert_eq!(&webp[8..12], b"WEBP");
    let decoded = vm_media::decode_page(&webp, vm_media::MAX_OUTPUT_EDGE).unwrap();
    assert_eq!((decoded.width, decoded.height), dimensions);
}

#[test]
fn epub_import_skips_invalid_cover_then_stores_two_webp_sizes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.db");
    let epub = temp.path().join("book.epub");
    let mut comic = vm_format::CbzReader::new(
        std::io::Cursor::new(include_bytes!("../../../samples/demo-comic.cbz")),
        "comic.cbz",
    )
    .unwrap();
    let image = comic.read_page(0).unwrap();
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&epub).unwrap());
        let members: [(&str, &[u8]); 6] = [
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#),
            ("book.opf", br#"<package>
              <metadata><title>Cover test</title><creator>Author</creator><meta name="cover" content="bad"/></metadata>
              <manifest>
                <item id="bad" href="bad.png" media-type="image/png"/>
                <item id="good" href="cover.png" media-type="image/png" properties="cover-image"/>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
              </manifest><spine><itemref idref="chapter"/></spine></package>"#),
            ("chapter.xhtml", b"<html><body><h1>Chapter</h1><p>Content for reading.</p></body></html>"),
            ("bad.png", b"not an image"),
            ("cover.png", &image),
        ];
        for (member, bytes) in members {
            zip.start_file(member, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    let store = Store::spawn(path.clone(), || {}).unwrap();
    let next = || store.events().recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(next(), Event::Ready));
    store
        .send(Command::ImportEpub { path: epub.clone() })
        .unwrap();
    let Event::BookInserted {
        book,
        duplicate: false,
    } = next()
    else {
        panic!("expected EPUB");
    };
    assert_eq!(book.format, BookFormat::Epub);
    let book_id = book.id;
    drop(store);
    fs::remove_file(epub).unwrap();

    let reopened = Store::spawn(path, || {}).unwrap();
    assert!(matches!(
        reopened
            .events()
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        Event::Ready
    ));
    assert_cover_dimensions(&reopened, &book_id, CoverSize::Shelf, (400, 600));
    assert_cover_dimensions(&reopened, &book_id, CoverSize::Detail, (800, 1200));
}

#[test]
fn rename_remove_restore_preserve_marks_and_original_import_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.db");
    let mut db = database::open(&path).unwrap();
    let (book, _) = database::insert_text(&mut db, sample(), BookFormat::Txt, None).unwrap();
    let loc = Loc {
        chapter: 0,
        para: 0,
        off: 1,
    };
    database::save_progress(&db, &book.id, &loc).unwrap();
    let bookmark = reading_marks::add_bookmark(&mut db, &book.id, loc, Some("旧记号")).unwrap();
    let annotation = reading_marks::add_annotation(
        &mut db,
        &book.id,
        loc,
        2,
        Some("旧批注"),
        AnnotationColor::Green,
    )
    .unwrap();
    assert!(database::rename_book(&mut db, &book.id, "   ").is_err());
    assert!(database::rename_book(&mut db, &book.id, "bad\0title").is_err());
    let (renamed, status) = database::rename_book(&mut db, &book.id, "  星空故事  ").unwrap();
    assert_eq!(status, ShelfStatus::Active);
    assert_eq!(renamed.id, book.id);
    assert_eq!(renamed.title, "星空故事");

    database::set_book_status(&mut db, &book.id, ShelfStatus::Removed).unwrap();
    drop(db);
    let mut db = database::open(&path).unwrap();
    assert!(database::list_books(&db, 0).unwrap().0.is_empty());
    let removed = ShelfFilter {
        status: ShelfStatus::Removed,
        query: "空故事".into(),
        ..ShelfFilter::default()
    };
    assert_eq!(
        database::list_shelf(&db, 0, &removed).unwrap().0,
        std::slice::from_ref(&renamed)
    );
    database::set_book_status(&mut db, &book.id, ShelfStatus::Active).unwrap();
    assert_eq!(
        database::list_books(&db, 0).unwrap().0,
        std::slice::from_ref(&renamed)
    );
    assert!(database::list_shelf(&db, 0, &removed).unwrap().0.is_empty());
    assert_eq!(
        reading_marks::list_bookmarks(&db, &book.id, 0).unwrap().0,
        [bookmark]
    );
    assert_eq!(
        reading_marks::list_annotations(&db, &book.id, 0).unwrap(),
        [annotation]
    );

    database::set_book_status(&mut db, &book.id, ShelfStatus::Removed).unwrap();
    let (returned, duplicate) =
        database::insert_text(&mut db, sample(), BookFormat::Txt, None).unwrap();
    assert!(duplicate);
    assert_eq!(returned, renamed);
    assert_eq!(database::list_books(&db, 0).unwrap().0, [returned]);
}

#[test]
fn shelf_paginates_after_filtering_and_treats_search_as_literal() {
    let mut db = database::open(std::path::Path::new(":memory:")).unwrap();
    for index in 0..62 {
        let mut input = sample();
        input.title = format!("Shelf%_{index:02}");
        database::insert_text(&mut db, input, BookFormat::Txt, None).unwrap();
    }
    database::insert_text(&mut db, sample(), BookFormat::Epub, Some("Author".into())).unwrap();
    let filter = ShelfFilter {
        format: Some(BookFormat::Txt),
        query: "shelf%_".into(),
        sort: ShelfSort::Title,
        ..ShelfFilter::default()
    };
    let (first, more) = database::list_shelf(&db, 0, &filter).unwrap();
    assert!(more);
    assert_eq!(first.len(), 60);
    assert_eq!(first.first().unwrap().title, "Shelf%_00");
    assert_eq!(first.last().unwrap().title, "Shelf%_59");
    let (last, more) = database::list_shelf(&db, 60, &filter).unwrap();
    assert!(!more);
    assert_eq!(
        last.iter()
            .map(|book| book.title.as_str())
            .collect::<Vec<_>>(),
        ["Shelf%_60", "Shelf%_61"]
    );
    let wrong_format = ShelfFilter {
        format: Some(BookFormat::Epub),
        ..filter
    };
    assert!(database::list_shelf(&db, 0, &wrong_format)
        .unwrap()
        .0
        .is_empty());
}
