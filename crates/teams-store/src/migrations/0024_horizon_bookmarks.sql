-- Schema 24: when the account last marked a whole conversation read (the conversation's own property
-- `consumptionHorizonBookmark`, `0;<ms>;0`): the activity feed's "Mark all as read". An item of the feed whose version
-- is no later than `time_ms` (ms since the epoch) reads as read. The later of two times is kept. Rows go with their
-- conversation.
CREATE TABLE horizon_bookmarks (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
    time_ms INTEGER NOT NULL
) STRICT;
