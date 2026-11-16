-- The search compares words with their case and their Latin accents folded away, the server's own folding on both
-- backends. `search_names` holds the folded words of the names the search finds people, teams, channels and files by
-- (`kind`: person, team, channel, file), with the `source` text they were folded from: a name that changed since
-- (its source differs) is a candidate of every search until the server folds it again. The messages' words are
-- folded again: their rows go, and the server indexes the messages again at its start.
CREATE TABLE search_names (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    source TEXT NOT NULL,
    words TEXT NOT NULL,
    PRIMARY KEY (kind, id)
);
DELETE FROM message_search;
