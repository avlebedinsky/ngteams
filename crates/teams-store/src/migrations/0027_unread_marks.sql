-- Schema 27: a conversation the account marked unread ("Mark as unread"). The bookmark's arrival (ms) is kept beside
-- its time: while it isn't 0, the conversation is unread from the message after it on. A bookmark of `0;<ms>;0` (the
-- activity feed's "Mark all as read", or the end of a mark) keeps 0.
ALTER TABLE horizon_bookmarks ADD COLUMN arrival INTEGER NOT NULL DEFAULT 0;
