CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE book (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    author TEXT,
    format TEXT NOT NULL CHECK (format IN ('txt', 'epub', 'cbz')),
    encoding TEXT,
    chapter_count INTEGER NOT NULL CHECK (chapter_count > 0),
    added_at INTEGER NOT NULL DEFAULT (unixepoch()),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'removed'))
) STRICT;

CREATE INDEX book_shelf ON book(status, added_at DESC, id);

CREATE TABLE chapter (
    book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL CHECK (idx >= 0),
    title TEXT NOT NULL,
    byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
    body BLOB NOT NULL,
    PRIMARY KEY (book_id, idx)
) STRICT;

CREATE TABLE paragraph (
    book_id TEXT NOT NULL,
    chapter INTEGER NOT NULL,
    idx INTEGER NOT NULL CHECK (idx >= 0),
    byte_off INTEGER NOT NULL CHECK (byte_off >= 0),
    byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
    char_len INTEGER NOT NULL CHECK (char_len >= 0),
    PRIMARY KEY (book_id, chapter, idx),
    FOREIGN KEY (book_id, chapter) REFERENCES chapter(book_id, idx) ON DELETE CASCADE
) STRICT;

CREATE VIRTUAL TABLE paragraph_fts USING fts5 (
    body, book_id UNINDEXED, chapter UNINDEXED, para UNINDEXED,
    tokenize = 'trigram',
    detail = 'full'
);

CREATE TRIGGER chapter_delete_search AFTER DELETE ON chapter BEGIN
    DELETE FROM paragraph_fts WHERE book_id = OLD.book_id AND chapter = OLD.idx;
END;

CREATE TABLE progress (
    book_id TEXT PRIMARY KEY REFERENCES book(id) ON DELETE CASCADE,
    chapter INTEGER NOT NULL CHECK (chapter >= 0),
    para INTEGER NOT NULL CHECK (para >= 0),
    off INTEGER NOT NULL CHECK (off >= 0),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (book_id, chapter) REFERENCES chapter(book_id, idx) ON DELETE CASCADE
) STRICT;

CREATE TABLE bookmark (
    book_id TEXT NOT NULL,
    chapter INTEGER NOT NULL,
    para INTEGER NOT NULL,
    off INTEGER NOT NULL CHECK (off >= 0),
    label TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (book_id, chapter, para),
    FOREIGN KEY (book_id, chapter, para)
        REFERENCES paragraph(book_id, chapter, idx) ON DELETE CASCADE
) STRICT;

CREATE TABLE annotation (
    id TEXT PRIMARY KEY,
    book_id TEXT NOT NULL,
    chapter INTEGER NOT NULL,
    para INTEGER NOT NULL,
    start_off INTEGER NOT NULL CHECK (start_off >= 0),
    end_off INTEGER NOT NULL CHECK (end_off > start_off),
    text TEXT NOT NULL,
    note TEXT,
    color TEXT NOT NULL CHECK (color IN ('yellow', 'green', 'blue', 'pink')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (book_id, chapter, para)
        REFERENCES paragraph(book_id, chapter, idx) ON DELETE CASCADE
) STRICT;

CREATE TABLE legacy_book (
    source TEXT NOT NULL,
    legacy_id TEXT NOT NULL,
    book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
    PRIMARY KEY (source, legacy_id)
) STRICT;
