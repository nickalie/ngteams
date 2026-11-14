CREATE TABLE chat_messages (
    chat_id TEXT NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    id BIGINT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    client_id TEXT,
    version BIGINT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (chat_id, id),
    UNIQUE (chat_id, owner_id, client_id)
);
CREATE INDEX chat_messages_version ON chat_messages (chat_id, version);
