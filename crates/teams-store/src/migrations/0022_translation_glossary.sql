-- Schema 22: a translated chat's glossary (the app's chat translation, by the user's own AI tool). Each row is a term as
-- the chat writes it and the rendering the translator chose for it in the language the chat is read in (the app's
-- code), so a later translation, by a new tool process or in a later run, renders it the same way. `used_at` (ms since
-- the epoch) is when the translator last named it; the store keeps a chat's newest few per language. Rows go with
-- their conversation.
CREATE TABLE translation_glossary (
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    term TEXT NOT NULL,
    rendering TEXT NOT NULL,
    used_at INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, language, term)
) STRICT;
