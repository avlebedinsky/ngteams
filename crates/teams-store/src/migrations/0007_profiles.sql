-- Schema 7 (M2 Task 12): people's names and pictures as the account last read them, by MRI. The name is Graph's, else
-- the middle tier's profile lookup's (`name_source`); the picture is Graph's 96x96 one. `picture_state` says whether a
-- read settled it: `unread` (never), `none` (the account has none) or `some` (its bytes and media type here).
-- `read_at` is when the last read that settled everything finished (0 while none has): the core reads a profile again
-- once it is older than its age limit.
CREATE TABLE profiles (
    mri TEXT PRIMARY KEY NOT NULL,
    name TEXT,
    name_source TEXT,
    picture_state TEXT NOT NULL,
    picture_type TEXT,
    picture BLOB,
    read_at INTEGER NOT NULL
) STRICT;
