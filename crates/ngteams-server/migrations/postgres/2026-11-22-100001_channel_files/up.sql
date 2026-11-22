-- Files in a channel's folder on its team's site (`/sp/sites/<site>/`). A member of the channel reads one; its
-- bytes count against the account that added it (`owner_id`), as a chat file's do. `folder` is the channel's folder
-- name when the file was added, so the file keeps its address when the channel is renamed. `stage` is `stub` (empty,
-- as `addStub` made it), `uploading` (one `StartUpload` streams its bytes) or `filled`. Times are Unix seconds.
CREATE TABLE channel_files (
    id TEXT PRIMARY KEY NOT NULL,
    team_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    folder TEXT NOT NULL,
    name TEXT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    stage TEXT NOT NULL,
    upload_id TEXT,
    size BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (team_id, folder, name)
);
CREATE INDEX channel_files_owner ON channel_files (owner_id);
