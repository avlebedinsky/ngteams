-- Schema 20: chats translated on this computer (the app's chat translation, by the user's own AI tool). A message's
-- translation is kept by its id, its version and the language it was translated into, so an edit (a newer version)
-- or another language is translated again and a later run shows the rest at once. `kind` is `translated`, `same` (the
-- message was in that language already: shown as it is, `content` empty) or `original` (the account's own message as
-- it was written, before it was sent translated). `content` is the rich text as teams-proto writes it (JSON); the
-- store only keeps it. Rows go with their message.
CREATE TABLE message_translations (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    language TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('translated', 'same', 'original')),
    content TEXT NOT NULL,
    -- the language the message was written in, as the translator named it; none when it didn't say
    source_language TEXT,
    PRIMARY KEY (conversation_id, message_id, version, language),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE
) STRICT;

-- The account's own messages as written, kept when one was sent (or edited) translated, so the account keeps seeing
-- what it wrote. `key` is the post's `clientmessageid`, or `edit:<message id>` for an edit; `sent_text` is the plain
-- text that was sent, matched against the message so a later change made elsewhere isn't shown as this original.
CREATE TABLE sent_originals (
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    language TEXT NOT NULL,
    content TEXT NOT NULL,
    sent_text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, key)
) STRICT;
