# The own server (`ngteams-server`)

`crates/ngteams-server` is a Teams-compatible server ngteams can sign in to instead of Microsoft's. It speaks the
wire the client already speaks to Microsoft (the shapes in `docs/protocol/`), with every service under one HTTPS
origin. The client sends a request to it only for an account signed in to that server.

What it serves today: sign-in (the token endpoint) and authz. Other services are added one at a time. Authz names
only the services the server serves, so the client keeps every other service off.

## Stack

| Need | Library |
|---|---|
| HTTP | axum 0.8 on hyper 1 and tower; tower-http for request timeouts, body limits and traces |
| TLS | rustls (aws-lc-rs) through axum-server, or plain HTTP behind a reverse proxy |
| Database | Diesel 2.4: Postgres through diesel-async (tokio-postgres, no libpq), SQLite through Diesel's own connection on the blocking pool; bb8 pools; embedded migrations per backend |
| Passwords | argon2 (Argon2id, PHC strings) |
| Tokens | jsonwebtoken (EdDSA, Ed25519 keys from aws-lc-rs, `kid` per key); opaque refresh tokens kept as SHA-256 |
| Fan-out, counters | redis-rs 1.8: a connection manager with RESP3 pushes and resubscription after a reconnect (Redis or Valkey) |
| Files | object_store: a local directory, memory or S3 |
| Configuration, CLI | figment (TOML plus environment), clap |
| Logs | tracing, tracing-subscriber |

SQLite comes from the `libsqlite3-sys` the workspace already links (rusqlite pins it); sqlx can't share that version,
so Diesel serves both backends. fred (the planned Redis client) can't resolve beside PipeWire's `libspa`: its
`redis-protocol` pins `cookie-factory =0.3.2`.

## Running it

```sh
ngteams-server --config server.toml migrate
printf '%s\n' "$PASSWORD" | ngteams-server --config server.toml user add ada@example.com --name "Ada Lovelace"
ngteams-server --config server.toml serve
```

Commands: `serve`, `migrate`, `user add|list|disable|enable|reset-password`, `keys rotate`. A password is read from
the first line of standard input, never from the command line. Disabling an account or resetting its password
revokes its refresh tokens: its sessions end when their access tokens run out (one hour by default).

`GET /health/live` answers 204 while the process runs. `GET /health/ready` answers 204 while the database and the
fan-out answer, else 503.

## Configuration

Defaults, then the TOML file (`--config` or `NGTEAMS_SERVER_CONFIG`), then `NGTEAMS_SERVER_*` variables. A nested key
takes a double underscore: `NGTEAMS_SERVER_TLS__CERT`, `NGTEAMS_SERVER_TOKENS__ACCESS_LIFETIME`. Logs follow
`NGTEAMS_SERVER_LOG` (an `EnvFilter`, default `info`).

| Key | Default | Meaning |
|---|---|---|
| `listen` | `127.0.0.1:8080` | address to listen on |
| `public_origin` | `http://127.0.0.1:8080` | the origin clients use; every base authz hands out is under it. `https` (plain `http` only on loopback) |
| `database_url` | `sqlite://ngteams-server.sqlite` | `postgres://…` or `sqlite://<path>` |
| `database_pool` | `16` | Postgres connections per node (SQLite: one) |
| `redis_url` | none | `redis://…` or `rediss://…`; without it, fan-out and login counters stay in the node |
| `storage_url`, `storage_options` | `memory:///`, none | `file:///<dir>`, `s3://<bucket>[/<prefix>]`, with `object_store`'s option names (`aws_region`, `aws_endpoint`, …) |
| `tls.cert`, `tls.key` | none | PEM chain and key; without them the server speaks plain HTTP for a proxy in front |
| `region`, `partition` | `own`, `own01` | the names in authz's answer and the regional bases |
| `audiences` | the client's resources | the resources a token may be asked for |
| `tokens.access_lifetime` | `3600` | seconds an access token (and the skypetoken exchanged for it) lives |
| `tokens.refresh_lifetime` | `7776000` | seconds a refresh token lives (90 days) |
| `tokens.key_rotation` | `2592000` | seconds a signing key signs before a new one replaces it (30 days) |
| `tokens.reuse_grace` | `60` | seconds a spent refresh token may still be redeemed; after that it revokes its sign-in |
| `login.max_failures`, `login.window` | `10`, `900` | failed password sign-ins an account may have in the window before it is held off |

## Sign-in and tokens

- `POST /oauth2/v2.0/token` takes Entra's form: `grant_type=password` (`username`, `password`) for a sign-in,
  `grant_type=refresh_token` for every later token, each with `client_id` and one resource's `scope`
  (`<resource>/.default`, beside `openid profile offline_access`). The token's `aud` is that resource. The answer
  and the errors have Entra's shapes (`invalid_grant` with `AADSTS50126` for a wrong password, `50057` for a
  disabled account, `70000` for a refresh token that is not valid), so the client's `TokenClient` and `Session` run
  unchanged with the server as authority.
- Refresh tokens are single use. Each grant returns the next one of the same sign-in. One presented again after
  `tokens.reuse_grace` revokes that whole sign-in.
- Failed password sign-ins are counted per account (in Redis when configured). After `login.max_failures` the
  account's password sign-ins get 429 with `Retry-After` until the window ends.
- `POST /api/authsvc/v1.0/authz` takes an access token for `https://api.spaces.skype.com` and answers with a
  skypetoken, `region`, `partition` and `regionGtms`, as Teams' authz does. The skypetoken runs out when the
  exchanged token does. A disabled account gets 403 `UserNotFoundInSkypeTeam`.
- Access tokens, ID tokens and skypetokens are EdDSA JWTs with the issuer `<origin>/<tenant>/v2.0`. Each node checks
  them against the shared keys and reloads the keys when it meets a `kid` it doesn't hold. The tenant is one GUID per
  server, made by the first node and kept in the database. Accounts are `8:orgid:<oid>`.

## Prefix table

Every service lives under the public origin with a fixed prefix. `Served` means the server answers it now and authz
hands out its GTM.

| Service | GTM | Path under the origin | Served |
|---|---|---|---|
| Token endpoint | (the authority) | `/oauth2/v2.0/token` | yes |
| authz | `authService` (base `/api/authsvc`) | `/api/authsvc/v1.0/authz` | yes |
| Chat service | `chatServiceAfd` | `/api/chatsvc/<region>/` | no |
| Middle tier | `middleTier` | `/api/mt/part/<partition>/` | no |
| Chat service aggregator | `chatSvcAggAfd` | `/api/csa/<region>/` | no |
| Presence | `presenceUPS` | `/ups/<geo>/` | no |
| Registrar | (the origin) | `/registrar/prod/V2/` | no |
| Trouter | (the origin) | `wss://<origin>/trouter/v4/c` | no |
| AMS | `amsV2` | `/ams/` | no |
| Graph | (the origin) | `/graph/v1.0/` | no |
| Files | (the origin) | `/sp/` | no |
| Loki | (the origin) | `/loki/` | no |
| Search | (the origin) | `/search/` | no |
| Flight proxy | (the origin) | `/flightproxy/api/v2/` | no |
| TURN credentials | (the origin) | `/trap/tokens` | no |

## Scaling

- HTTP nodes keep no state of their own: Postgres is the source of truth, so any node answers any request. SQLite
  means a single node; it needs no Redis.
- With Redis, a write's events go to `Fanout::publish` on a topic such as `push:<mri>`, and every node that holds a
  subscriber for it delivers them. Each node keeps one Redis subscription per topic and feeds its local subscribers
  from it. Without Redis the same `Fanout` trait is one process's broadcast.
- Delivery through the fan-out is at most once. A subscriber that falls behind learns how many payloads it missed.
- Signing keys are rows in the database. A node rotates the key when it is due (checked hourly) and drops keys that
  can no longer verify a live token. `ngteams-server keys rotate` rotates at once.
- Migrations take a Postgres advisory lock, so nodes starting together take turns.

## Tests

`cargo test -p ngteams-server` runs the e2e tests: the real client crates (`teams-auth`'s session and clients,
`teams-core`'s account bring-up) against an in-process server on loopback, on SQLite. With `NGTEAMS_SERVER_PG_URL`
(a Postgres the tests may create databases on) the Postgres cases run too, and with `NGTEAMS_SERVER_REDIS_URL` the
Redis cases. CI's Coverage job sets both from its service containers. Locally:

```sh
podman run -d --rm --name ngts-pg -e POSTGRES_PASSWORD=ngts -p 55432:5432 docker.io/library/postgres:17
podman run -d --rm --name ngts-redis -p 56379:6379 docker.io/library/redis:7
NGTEAMS_SERVER_PG_URL=postgres://postgres:ngts@127.0.0.1:55432/postgres \
NGTEAMS_SERVER_REDIS_URL=redis://127.0.0.1:56379 cargo test -p ngteams-server
podman stop ngts-pg ngts-redis
```
