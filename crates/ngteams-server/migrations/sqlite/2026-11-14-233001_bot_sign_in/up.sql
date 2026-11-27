-- Sign-in cards: a bot's OAuth connections (an identity provider's endpoints and its client), the sign-ins under way
-- (a person's browser on its way through the provider, then the code it showed them), and each person's token for
-- a connection. Every secret and token is sealed with bots.token_key (AES-256-GCM); bot_connections times are Unix
-- seconds (as the registry's), the sign-ins' and tokens' Unix milliseconds.
CREATE TABLE bot_connections (
    bot_id TEXT NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authorize_url TEXT NOT NULL,
    token_url TEXT NOT NULL,
    client_id TEXT NOT NULL,
    client_secret BLOB NOT NULL,
    scopes TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (bot_id, name)
);

CREATE TABLE bot_sign_ins (
    id TEXT PRIMARY KEY NOT NULL,
    bot_id TEXT NOT NULL,
    connection TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    verifier BLOB NOT NULL,
    code_hash TEXT,
    token BLOB,
    attempts INTEGER NOT NULL,
    expires_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    FOREIGN KEY (bot_id, connection) REFERENCES bot_connections (bot_id, name) ON DELETE CASCADE
);
CREATE INDEX bot_sign_ins_person ON bot_sign_ins (bot_id, connection, user_id);

CREATE TABLE bot_user_tokens (
    bot_id TEXT NOT NULL,
    connection TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token BLOB NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (bot_id, connection, user_id),
    FOREIGN KEY (bot_id, connection) REFERENCES bot_connections (bot_id, name) ON DELETE CASCADE
);
