CREATE TABLE epub_image (
    book_id TEXT NOT NULL REFERENCES book(id) ON DELETE CASCADE,
    hash TEXT NOT NULL CHECK (length(hash) = 64),
    media_type TEXT NOT NULL CHECK (media_type IN ('image/jpeg', 'image/png', 'image/webp', 'image/gif')),
    bytes BLOB NOT NULL CHECK (length(bytes) BETWEEN 1 AND 33554432),
    PRIMARY KEY (book_id, hash)
) STRICT;

CREATE TABLE epub_illustration (
    book_id TEXT NOT NULL,
    chapter INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 511),
    before_para INTEGER NOT NULL CHECK (before_para >= 0),
    alt TEXT NOT NULL CHECK (length(alt) <= 256),
    image_hash TEXT NOT NULL,
    PRIMARY KEY (book_id, chapter, ordinal),
    FOREIGN KEY (book_id, chapter) REFERENCES chapter(book_id, idx) ON DELETE CASCADE,
    FOREIGN KEY (book_id, image_hash) REFERENCES epub_image(book_id, hash) ON DELETE CASCADE
) STRICT;
