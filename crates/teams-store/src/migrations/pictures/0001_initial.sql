-- The picture cache's schema 1: the pictures messages show (a message's picture on AMS, a GIF on Giphy, a file's
-- thumbnail), as the engine last read them, so a later run shows them without reading them again. `key` is the SHA-256
-- of what names the picture (never a link or a token), `size` the bytes' length and `digest` their SHA-256 (a picture
-- whose bytes no longer match is dropped when read), `stored_at` when they were read and `used_at` when they were last
-- asked for (milliseconds since the Unix epoch): the least recently used go first once the cache holds more than its
-- limit. The bytes come last, so the columns before them are read without the bytes' overflow pages, and the index by
-- use covers `size`, so the eviction reads no row's bytes.
CREATE TABLE pictures (
    id INTEGER PRIMARY KEY,
    key BLOB NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    digest BLOB NOT NULL,
    stored_at INTEGER NOT NULL,
    used_at INTEGER NOT NULL,
    bytes BLOB NOT NULL
) STRICT;

CREATE INDEX pictures_by_use ON pictures (used_at, id, size);
