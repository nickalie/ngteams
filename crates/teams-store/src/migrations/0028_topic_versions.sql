-- Schema 28: the thread version (ms) of the name a conversation has: the `version` of the `ThreadUpdate` or the
-- `threadProperties.version` of the listing that brought it. A name from an older or the same version does not
-- replace it. NULL when no delivery said it.
ALTER TABLE conversations ADD COLUMN topic_version INTEGER;
