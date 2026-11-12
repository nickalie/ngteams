CREATE TABLE conversation_previews (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    version INTEGER NOT NULL,
    client_message_id TEXT,
    sender TEXT,
    sender_name TEXT,
    message_type TEXT NOT NULL,
    content TEXT NOT NULL,
    properties TEXT,
    arrival_time INTEGER NOT NULL,
    compose_time INTEGER,
    edit_time INTEGER,
    delete_time INTEGER
) STRICT;

CREATE VIEW pending_conversation_previews AS
SELECT * FROM conversation_previews AS preview
WHERE NOT EXISTS (SELECT 1 FROM conversation_sync WHERE conversation_id = preview.conversation_id)
  AND NOT EXISTS (SELECT 1 FROM messages WHERE conversation_id = preview.conversation_id);
