CREATE TABLE comic_archive (
    book_id TEXT PRIMARY KEY REFERENCES book(id) ON DELETE CASCADE,
    blob_hash TEXT NOT NULL UNIQUE CHECK (length(blob_hash) = 64)
) STRICT;
