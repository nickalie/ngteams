CREATE TABLE chat_properties (
    chat_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (chat_id, user_id),
    FOREIGN KEY (chat_id, user_id) REFERENCES chat_members (chat_id, user_id) ON DELETE CASCADE
);
