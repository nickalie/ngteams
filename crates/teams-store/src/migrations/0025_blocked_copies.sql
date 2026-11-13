-- Schema 25: the copies the chat service keeps the account of its own messages a tenant's policy blocked (the stream
-- `48:generic`): the blocked message comes back without its text and files, and names its copy by `id`. A copy may come
-- before or after the message naming it; it is kept so either order, and a restart between them, puts the text back.
-- `properties` is the copy's `properties` object as JSON text (its `files`).
CREATE TABLE blocked_copies (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    properties TEXT,
    arrival_time INTEGER NOT NULL
) STRICT;
