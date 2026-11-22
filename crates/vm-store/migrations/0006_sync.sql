CREATE TABLE sync_op (
    device BLOB NOT NULL,
    seq INTEGER NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY(device, seq)
) STRICT;

CREATE TABLE sync_shadow (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT;

ALTER TABLE bookmark RENAME TO bookmark_previous;
CREATE TABLE bookmark (
    book_id TEXT NOT NULL,
    chapter INTEGER NOT NULL,
    para INTEGER NOT NULL,
    off INTEGER NOT NULL CHECK (off >= 0),
    label TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (book_id, chapter, para, off),
    FOREIGN KEY (book_id, chapter, para)
        REFERENCES paragraph(book_id, chapter, idx) ON DELETE CASCADE
) STRICT;
INSERT INTO bookmark SELECT * FROM bookmark_previous;
DROP TABLE bookmark_previous;
