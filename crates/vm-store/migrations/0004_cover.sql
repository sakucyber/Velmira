CREATE TABLE book_cover (
    book_id TEXT PRIMARY KEY REFERENCES book(id) ON DELETE CASCADE,
    shelf_webp BLOB NOT NULL CHECK (length(shelf_webp) BETWEEN 1 AND 1048576),
    detail_webp BLOB NOT NULL CHECK (length(detail_webp) BETWEEN 1 AND 4194304)
) STRICT;
