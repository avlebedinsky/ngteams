-- Schema 26: the messages the user translated one at a time (a message's "Translate", not a translated chat), so they
-- show translated again when the chat opens again or in a later run. Their translations are the chat translations'
-- rows (schema 20); a row here only says the message is shown translated. Rows go with their message.
CREATE TABLE translated_messages (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (conversation_id, message_id),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE
) STRICT;
