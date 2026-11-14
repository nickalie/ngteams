# Separate chat domain outcomes from database failures

The chat repositories currently use `DbError::Conflict` for rejected edits and reactions on a deleted message,
creation with inactive or missing roster members, and an unreachable reaction index. HTTP handling maps these to
400, so the delivered response is correct, but the database error's unique-value meaning no longer identifies
its cause. JSON corruption reporting also lives in the property repository while other chat repositories use it.

Introduce typed creation and message-transform outcomes for invalid domain actions, keeping unique violations
as `DbError::Conflict`. Keep the existing ownership and membership checks, rejection statuses, transaction
rollback and unchanged-message behavior. Put stored-JSON corruption reporting with the chat persistence helpers.
Cover deleted-message edits/reactions and invalid rosters through the real client against both SQLite and
PostgreSQL before replacing the error plumbing. This is a coordinated repository-error refactor, deferred from
task 177's first review fixes to keep the paging and delivery fixes separate.
