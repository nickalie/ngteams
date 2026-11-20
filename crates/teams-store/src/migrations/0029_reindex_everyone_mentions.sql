-- Schema 29: the search text of a mention of everyone is now the reader's label, not the label that the sender's
-- client wrote. Remove the index rows of the rich-text messages whose properties name `everyone`, so that the backfill
-- indexes them again. The match is wide (any `everyone` in the properties): a message that it also takes only gets
-- the same text again.
DELETE FROM search_entries
WHERE EXISTS (
    SELECT 1 FROM messages
    WHERE messages.conversation_id = search_entries.conversation_id
      AND messages.id = search_entries.message_id
      AND messages.message_type LIKE 'RichText%'
      AND messages.properties LIKE '%everyone%'
);
