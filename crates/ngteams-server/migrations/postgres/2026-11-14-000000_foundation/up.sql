-- The server's foundation: its tenant, accounts, refresh tokens and signing keys. Times are Unix seconds.
CREATE TABLE server_meta (
    name TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    upn TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    disabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE refresh_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    family_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    client_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    used_at BIGINT,
    revoked_at BIGINT
);
CREATE INDEX refresh_tokens_family ON refresh_tokens (family_id);
CREATE INDEX refresh_tokens_user ON refresh_tokens (user_id);

CREATE TABLE signing_keys (
    kid TEXT PRIMARY KEY NOT NULL,
    private_der BYTEA NOT NULL,
    public_der BYTEA NOT NULL,
    created_at BIGINT NOT NULL,
    retired_at BIGINT
);
