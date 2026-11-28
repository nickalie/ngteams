-- Schema 30: the search text of a card is now the text on the card, and a link's app cards add their text to the
-- message's. Remove the index rows of the card messages and of the messages whose properties name `cards`, so that
-- the backfill indexes them again. The match is wide: a message that it also takes only gets the same text again.
DELETE FROM search_entries
WHERE EXISTS (
    SELECT 1 FROM messages
    WHERE messages.conversation_id = search_entries.conversation_id
      AND messages.id = search_entries.message_id
      AND (messages.message_type = 'RichText/Media_Card' OR messages.properties LIKE '%cards%')
);
