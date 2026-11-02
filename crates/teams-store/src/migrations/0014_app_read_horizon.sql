-- Schema 14: where the account last marked everything read in Teams, for its chats and for its channels (the chat
-- service's own property `appConsumptionHorizon`, `docs/protocol/chat.md`): whatever arrived up to then is read. No row
-- until a read or a delivery brought one.
CREATE TABLE app_read_horizon (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    chat INTEGER NOT NULL,
    channel INTEGER NOT NULL
) STRICT;
