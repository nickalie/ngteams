CREATE TABLE chat_clock (
    id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    version BIGINT NOT NULL
);
INSERT INTO chat_clock (id, version) VALUES (1, 0);

CREATE TABLE chats (
    id TEXT PRIMARY KEY NOT NULL,
    creator TEXT NOT NULL REFERENCES users (id),
    topic TEXT,
    unique_roster BOOLEAN NOT NULL,
    created_at BIGINT NOT NULL,
    version BIGINT NOT NULL
);
CREATE INDEX chats_version ON chats (version);

CREATE TABLE chat_members (
    chat_id TEXT NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    position INTEGER NOT NULL,
    joined_at BIGINT NOT NULL,
    PRIMARY KEY (chat_id, user_id)
);
CREATE INDEX chat_members_user ON chat_members (user_id, chat_id);
