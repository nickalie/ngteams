CREATE TABLE user_chat_properties (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    body TEXT NOT NULL
);
