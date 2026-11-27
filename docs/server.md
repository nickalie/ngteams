# The own server (`ngteams-server`)

`crates/ngteams-server` is a Teams-compatible server ngteams can sign in to instead of Microsoft's. It speaks the
wire the client already speaks to Microsoft (the shapes in `docs/protocol/`), with every service under one HTTPS
origin. The client sends a request to it only for an account signed in to that server.

What it serves today: sign-in (the token endpoint), authz, the chat service, teams and channels (CSA, the middle
tier's and the provisioning service's team routes), pictures in messages, files sent in chats, link previews, the
calendar and meetings' scheduling (the middle tier's calendar routes), presence, people (short profiles, Graph's users
and pictures, the person card), account administration (the admin API and the first-admin setup), the registrar and
the Trouter push socket, and calls (the call controller behind `/flightproxy/api/v2/`: 1:1 calls, a group chat's
calls with their audio through the server's own media node, and the TURN credentials). The registrar, Trouter and the calling surface have no GTM (the client
finds them on the origin). Other services are added one at a time. Authz names only the services the server serves,
so the client keeps every other service off.

The chat service serves chat creation, conversation sync, thread/member reads, message posting, edits, soft deletion,
reactions, transient typing, account preferences and message pages (newest, backward, incremental and around a
requested time) under `/api/chatsvc/<region>/v1/`. These routes require an IC3 bearer token and current membership;
disabled users are refused even while their token is live. A repeated 1:1 roster returns the existing chat, and
group rosters and names persist in SQLite or PostgreSQL. Requests are limited to 64 KiB. Authz advertises
`chatServiceAfd`. It also serves the account's own streams (notes, the activity feed, the mentions stream, the call
log), a group chat's people (below) and its name (`PUT v1/threads/<chat>/properties?name=topic`, from any member, at
most 250 characters, `""` takes it off; each member gets a `ThreadActivity/TopicUpdate` and the `ThreadUpdate`).
Mentions and quotes are stored as Teams gives them back ([Rich messages](#rich-messages)).

## Stack

| Need | Library |
|---|---|
| HTTP | axum 0.8 on hyper 1 and tower; tower-http for request timeouts, body limits and traces |
| Push socket | tokio-tungstenite on hyper's upgrade (the tungstenite the client links; axum's `ws` would add a second one) |
| TLS | rustls (aws-lc-rs) through axum-server, or plain HTTP behind a reverse proxy |
| Database | Diesel 2.4: Postgres through diesel-async (tokio-postgres, no libpq), SQLite through Diesel's own connection on the blocking pool; bb8 pools; embedded migrations per backend |
| Passwords | argon2 (Argon2id, PHC strings) |
| Tokens | jsonwebtoken (EdDSA, Ed25519 keys from aws-lc-rs, `kid` per key); opaque refresh tokens kept as SHA-256 |
| Fan-out, replay, counters | redis-rs 1.8: a connection manager with RESP3 pushes (a broken subscription ends its subscribers); Redis Streams for the replay journal (Redis 6.2 or later, or Valkey) |
| Files | object_store: any S3-compatible service, a local directory, or memory |
| Pictures, thumbnails | image (PNG, JPEG, GIF, WebP), decoded within size and memory limits |
| Link previews | reqwest behind an address guard, scraper for the page's metadata |
| Rich messages | scraper (html5ever, attributes in source order) writes a mention's message out again; indexmap keeps a mention entry's keys in order |
| Download ranges, media types | http-range-header, mime_guess |
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

`serve --role all|api|media` (or `NGTEAMS_SERVER_ROLE`) says what the node runs: `all` (the default) the public API
and a media node for group calls in one process, a single node's deployment; `api` the public API only, its group
calls' media on the media nodes the registry lists (needs Redis, else a group start is `503`); `media` a media node
only, its internal API and its UDP socket ([Group calls](#group-calls)). A `media` node opens the database and the
fan-out as any node does.

Commands: `serve`, `migrate`, `user add|list|disable|enable|reset-password|promote|demote|delete`,
`profile show|set|picture|remove-picture`, `media list|delete`, `app add|list|disable|enable|rotate-secret|remove`
([Apps and bots](#apps-and-bots)), `setup-code`, `keys rotate`. A password is read from the first line of standard input, never from the command line. Disabling or
deleting an account, or resetting its password, revokes its refresh tokens and closes its push sockets at once: a
disabled or deleted account is refused everywhere from then on, and after a new password the sessions end when
their access tokens run out (one hour by default). `user add --admin` and `user promote` make an admin; `user list`
ends each line with `admin` or `user`. The command line keeps the same guards as the admin API (the last enabled
admin stays) and its actions are audited as `cli`.

`media list <upn>` prints what an account keeps in the store, a line each: `file` or `picture`, id, bytes, when
it was stored, name (tab-separated). `media delete <upn> <id>…` deletes them by id: the bytes leave the store and
the account's quota, a file's shares and links end, and a message that showed a deleted picture shows none. It is
the operator's way to free an account that is at its quota ([Freeing storage](#freeing-storage)).

With `redis_url` set, `user disable`, `user delete` and `user reset-password` make the change in the database first
and then tell the account's open push sockets through Redis. When Redis is down the change stays: the command prints
a warning on standard error that says the account is disabled or deleted (the password is replaced) and exits 0.
Nothing more is pushed to the account, as each of its open sockets, on every node, ends at its next delivery check;
the command need not be run again. `user delete` also warns and exits 0 when the file store can't be opened: the
account is deleted and its picture stays in the store, never served. A command that changed nothing (no such account, a short password, no database)
exits 1 with the error. The admin API's same changes (and an account's own password change) end the sockets the
same way; when Redis is down they answer as made and the node logs a warning. Errors and warnings carry colour codes
only when standard error is a terminal.

`NGTEAMS_SERVER_LOG` controls logging (default `info`); dependency debug and trace events are always suppressed
because database drivers can include secret bind values.

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
| `redis_namespace` | the tenant id in the database | the deployment's part of that Redis: every key and channel is `ngts:<namespace>:…`. 1 to 64 letters, digits, `.`, `_` or `-`. Set it only where two deployments on one Redis hold the same tenant id (a database restored from another's); every node of a deployment takes the same value |
| `redis_timeout_ms` | `1000` | the longest a Redis command or connection attempt takes. Each use of Redis goes on without it (the rate limits let a request through, push and the journal leave the rest to sync), so a request waits about this long for a Redis that is down or doesn't answer; the commands right after it fail at once for as long, or until Redis answers again |
| `storage_url`, `storage_options` | `memory:///`, none | where pictures and files live: `s3://<bucket>[/<prefix>]` on any S3-compatible service, or `file:///<dir>` on one node ([Storage](#storage)). `memory:///` forgets everything at exit |
| `storage.connect_timeout`, `storage.request_timeout` | `3`, `30` | seconds a node waits for a connection to an S3 store, and for its answer or for a download that stands still ([Storage](#storage)) |
| `storage.max_retries`, `storage.retry_timeout` | `3`, `8` | how often a request the store didn't answer is sent again, and the seconds after its first attempt past which it isn't |
| `media.max_picture_bytes` | `25165824` | the largest picture pasted into a message (24 MiB, the most the client reads back) |
| `media.max_file_bytes` | `262144000` | the largest file sent in a chat (250 MiB, the client's own limit) |
| `media.user_quota_bytes` | `5368709120` | what one account may store, pictures and files together (5 GiB); `0` is no limit |
| `media.link_lifetime` | `3600` | seconds a file's download or thumbnail link works |
| `media.link_previews` | `true` | whether the server fetches pages and pictures for link previews |
| `media.preview_timeout` | `5` | seconds one preview fetch may take |
| `media.preview_max_page_bytes`, `media.preview_max_picture_bytes` | `1048576`, `5242880` | the most of a page read for its preview; the largest picture fetched for one |
| `media.preview_allow_private_addresses` | `false` | lets previews reach loopback and private addresses. For tests only |
| `tls.cert`, `tls.key` | none | PEM chain and key; without them the server speaks plain HTTP for a proxy in front |
| `region`, `partition` | `own`, `own01` | the names in authz's answer and the regional bases |
| `audiences` | the client's resources | the resources a token may be asked for |
| `tokens.access_lifetime` | `3600` | seconds an access token (and the skypetoken exchanged for it) lives |
| `tokens.refresh_lifetime` | `7776000` | seconds a refresh token lives (90 days) |
| `tokens.key_rotation` | `2592000` | seconds a signing key signs before a new one replaces it (30 days) |
| `tokens.reuse_grace` | `60` | seconds a spent refresh token may still be redeemed; after that it revokes its sign-in |
| `login.max_failures`, `login.window` | `10`, `900` | password attempts allowed in the window; success clears the count |
| `setup.require_code` | `true` | whether creating the first admin takes the setup code; turn it off only for an install nobody else reaches before its admin exists |
| `setup.max_attempts`, `setup.window` | `5`, `300` | first-admin setup attempts a client address may make in the window (seconds) |
| `trouter.auth_timeout` | `10` | seconds a push socket has to authenticate after it opened |
| `presence.endpoint_ttl` | `320` | seconds an account's endpoint shows as it was last reported; the client reports again every two minutes |
| `presence.subscription_ttl` | `7200` | seconds a presence subscription holds; the client renews its own every 55 minutes |
| `presence.max_subscriptions`, `presence.max_endpoints` | `650`, `32` | the people one endpoint may watch; the endpoints one account may report at once |
| `calling.ring_timeout` | `30` | seconds a call rings before it ends unanswered |
| `calling.turn.urls` | none | the TURN relay's URLs (`turn:<host>[:<port>][?transport=udp\|tcp]`, `turns:<host>[:<port>]`), handed out with the credentials; set exactly when a credential is |
| `calling.turn.secret` | none | coturn's `static-auth-secret`: credentials are made from it per account and run out by themselves |
| `calling.turn.username`, `calling.turn.password` | none | one long-term user of the relay (coturn's `user=`), used when no secret is set |
| `calling.turn.realm`, `calling.turn.ttl` | `rtcmedia`, `86400` | coturn's `realm`; seconds a credential made from the secret holds |
| `workflows.enabled` | `true` | whether the server serves Workflows ([below](#workflows-with-incoming-webhooks)) and issues tokens for Power Automate's two audiences, even when `audiences` lists them |
| `workflows.max_body_bytes` | `262144` | the largest body a workflow's webhook takes: 256 KB, the limit the client's Overview names |
| `workflows.rate_per_minute` | `60` | posts one workflow's webhook takes in a minute, across every node (counted in Redis when there is one) |
| `workflows.runs_kept` | `100` | runs kept per workflow for its run history |
| `mixer.udp_listen` | `0.0.0.0:0` | the media node's one UDP socket: every group call's media goes through it. A free port unless set: set a fixed one where a firewall must let the relay's datagrams in |
| `mixer.public_addrs` | the socket's address, else the host's outward one | the addresses clients' TURN relay reaches that socket at (its host candidates, each on the socket's port) |
| `mixer.internal_listen`, `mixer.internal_url` | `127.0.0.1:0`, the listener's address | the internal API's listener (signaling → media node), on a private network; the URL the other nodes reach it at. With Redis, the node logs a warning when this URL is a loopback address, because other hosts can't reach it |
| `mixer.node_id` | a new one each start | the node's id in the registry |
| `mixer.ice_lite` | `true` | on the legs whose offer the node answers, it only answers connectivity checks (it has a reachable address); where it offers (a callee's leg) it runs full ICE |
| `mixer.max_rooms`, `mixer.max_legs_per_room` | `1000`, `50` | over them a start, a join or an add is refused (`503`) |
| `mixer.heartbeat`, `mixer.dead_after` | `5`, `15` | seconds between the node's heartbeats in Redis; seconds without one after which it is dead and its calls end |
| `mixer.media_timeout`, `mixer.connect_timeout` | `20`, `30` | seconds a party's media may stay down, or take to come up after its answer, before it is taken out of the call |
| `mixer.reconcile`, `mixer.drain_timeout` | `30`, `300` | seconds between the node's looks at its rooms' calls; seconds a stopping node serves its rooms before it ends them |
| `apps.enabled` | `true` | whether the server serves apps and bots ([below](#apps-and-bots)): the registry, a bot's credentials and the keys it checks with, the catalogue and installs, and authz's `appsService` |
| `bots.scope` | `https://api.botframework.com` | the resource a bot asks its token for (`<scope>/.default`) and that token's audience; never an audience a person's token is for (`audiences`, the public origin, Workflows') |
| `bots.issuer`, `bots.service_url` | `<public_origin>/bot`, `<public_origin>/bot/` | the issuer of the tokens activities to bots carry; the `serviceUrl` activities name |
| `bots.token_lifetime`, `bots.activity_token_lifetime` | `3600`, `3600` | seconds a bot's own token and an activity's token are valid |
| `bots.key_rotation` | `2592000` | seconds a bot signing key signs before a new one replaces it (30 days) |
| `bots.allow_unsigned` | `false` | whether a bot may be registered with `auth_mode: "unsigned"` (no authentication either way: development or a private network) |
| `bots.allow_http_endpoints` | `false` | whether a bot's endpoint may be plain `http` (development only) |
| `bots.allowed_networks` | `[]` | networks (CIDR) a bot's endpoint may resolve into besides public addresses: private, loopback or link-local ranges of self-hosted bots; a cloud metadata address is never reached |
| `bots.retry_delays` | `[15, 45]` | seconds before each new attempt of an activity a bot didn't take; one attempt more than there are entries |
| `bots.delivery_lease` | `30` | seconds a node holds an activity it sends before another node may take it over; longer than `bots.send_timeout` |
| `bots.send_timeout` | `10` | seconds a send to a bot may take, from connecting to the end of its answer |
| `bots.send_rate`, `bots.send_burst` | `50`, `50` | activities a second the server sends one bot, and how many at once, on each node; a send over it waits its turn, which is no failed attempt |
| `bots.connector_rate`, `bots.connector_burst` | `50`, `50` | requests a second one app's bot may make to the connector, and how many at once, on each node; more get 429 with `Retry-After` |
| `bots.token_key` | none | 32 bytes in base64 (`openssl rand -base64 32`): the key the people's tokens from a bot's OAuth connections and those connections' client secrets are sealed with (AES-256-GCM). Without it no connection is made and no one signs in to a bot ([Sign-in cards](#sign-in-cards)); every node needs the same key |
| `outgoing_webhooks.enabled` | `true` | whether the server serves a team's outgoing webhooks ([below](#outgoing-webhooks)): their routes, their mentions and authz's `outgoingWebhookService` |
| `outgoing_webhooks.timeout_ms` | `5000` | milliseconds a webhook has for its answer, from connecting to its end: Teams' 5 seconds |
| `outgoing_webhooks.max_picture_bytes` | `1048576` | the largest picture (PNG or JPEG) a webhook may have |
| `[[rooms]]` | none | the meeting rooms the room finder lists, each `name`, `address` (its mailbox) and optionally `list` (its building), `capacity`, `floor`, `street`, `audio_device`, `video_device`, `display_device`, `wheelchair` |

## Operations

`packaging/server/` holds a deployment for one host with Docker Compose (Podman works through `docker-compose` on
its socket): the image's `Dockerfile`, `compose.yaml`, `Caddyfile`, `server.toml` and `.env.example`.

### The image

The `Dockerfile` builds only `ngteams-server`, with the workspace's pinned toolchain, onto Debian slim. The context
is the repository root:

```sh
docker build -f packaging/server/Dockerfile -t ngteams-server .
```

`--build-arg CARGO_BUILD_JOBS=4` limits the compile on a busy host. Podman keeps the health check only with
`podman build --format docker`. The image runs as the user `ngteams` (uid 10001) in `/var/lib/ngteams-server`, its
entrypoint is `ngteams-server` and its command `serve`. It listens on `0.0.0.0:8080` (`NGTEAMS_SERVER_LISTEN`), and
its health check asks that port's `/health/ready`.

### Deploying with Compose

| Service | Runs |
|---|---|
| `caddy` | TLS on ports 80 and 443; balances `api-1` and `api-2` by their `/health/ready`, WebSockets included |
| `api-1`, `api-2` | `serve --role api` |
| `media` | `serve --role media`: its UDP socket on `MEDIA_UDP_PORT`, published; its internal API at `http://media:9090` on the Compose network |
| `migrate` | `migrate`, once, before the nodes start |
| `postgres` | Postgres 17, volume `postgres` |
| `redis` | Redis 7, nothing kept on disk |
| `rustfs` | RustFS, the S3 store, volume `rustfs`; `bucket` makes the bucket at each start |
| `coturn` | the TURN relay, on the host's network |

```sh
cd packaging/server
cp .env.example .env        # fill it in, then: chmod 600 .env
docker compose build
docker compose up -d
docker compose ps
curl -i https://teams.example.com/health/ready     # 204
```

`.env` holds the name (`NGTEAMS_DOMAIN`), an address of the host for the relay (`HOST_IP`) and the secrets: the
database password, the S3 keys and the TURN secret. Write each secret as hex (`openssl rand -hex 32`): they go into
URLs unescaped. Nothing else needs setting.

The firewall lets in:

| Port | For |
|---|---|
| TCP 80, TCP and UDP 443 | Caddy (80 for the certificate and the redirect) |
| UDP `MEDIA_UDP_PORT` (50000) | the media node, from the relay |
| TCP and UDP `TURN_PORT` (3478) | coturn, from clients |
| UDP `TURN_MIN_PORT`–`TURN_MAX_PORT` (49160–49359) | coturn's relay ports: one per party in a call |

coturn runs on the host's network: its relay ports and the addresses it sees must be the host's own. It sends a group
call's media to the media node at `HOST_IP`, the only private address it may reach: it relays UDP only, and it refuses
the private, shared (CGNAT, Tailscale), link-local and benchmark ranges, IPv6's too. On a host behind 1:1 NAT (a cloud
instance's public address), set `TURN_EXTERNAL_IP=<public>/<private>`. The relay speaks plain TURN: a call's media is
DTLS-SRTP inside it, and its credentials expire.

Compose sets the connections, the TURN relay and the media node's keys in the environment. Other keys go in
`server.toml` (read through `NGTEAMS_SERVER_CONFIG`) or as more `NGTEAMS_SERVER_*` variables, which win. After a
change to `server.toml`, `docker compose restart api-1 api-2 media`; `docker compose up -d` recreates what a change to
`.env` or `compose.yaml` touched.

To upgrade, update the checkout, then `docker compose build` and `docker compose up -d`: `migrate` runs first and the
nodes start after it.

### TLS

Caddy gets a certificate for `NGTEAMS_DOMAIN` from Let's Encrypt and renews it by itself. The name's DNS must point at
the host and ports 80 and 443 must be open. The nodes speak plain HTTP inside the Compose network.

A server without a public name uses Caddy's own certificate authority: `CADDY_GLOBAL_OPTIONS=local_certs` in `.env`.
Each account then trusts that authority's root:

```sh
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ngteams-ca.crt
```

The sign-in window's "Certificate authority file" takes it, the account's `ca_file`
([personal-build.md](personal-build.md#own-server-accounts)). The authority lives in the volume `caddy-data`: keep it,
or every client needs the new root.

Behind Caddy the nodes see its address as every client's, so the first-admin setup's attempts
(`setup.max_attempts`) count for all clients together.

### Backups

- **Postgres** holds everything that lasts: accounts, chats and messages, teams, the calendar, calls, the signing
  keys, apps, the audit. Dump it while it runs:

  ```sh
  docker compose exec -T postgres pg_dump -U ngteams -d ngteams --format=custom > ngteams.dump
  ```

  Restore it with the nodes stopped:

  ```sh
  docker compose stop api-1 api-2 media
  docker compose exec -T postgres pg_restore -U ngteams -d ngteams --clean --if-exists --no-owner < ngteams.dump
  docker compose up -d
  ```

  A database restored beside the deployment it came from, on the same Redis, needs its own `redis_namespace`
  ([Configuration](#configuration)).
- **The bucket** holds the pictures, files, thumbnails, profile pictures and app icons. Copy it with any S3 tool after
  the database dump: the copy then holds each blob the dump names, except one deleted between the two steps (its file
  is restored without its bytes). For an exact pair, stop `api-1`, `api-2` and `media` during both steps. For example,
  with rclone on the Compose network:

  ```sh
  docker run --rm --network ngteams_default --env-file .env -v "$PWD/blobs:/backup" \
    -e RCLONE_CONFIG_S3_TYPE=s3 -e RCLONE_CONFIG_S3_PROVIDER=Other \
    -e RCLONE_CONFIG_S3_ENDPOINT=http://rustfs:9000 \
    --entrypoint sh docker.io/rclone/rclone -c \
    'RCLONE_CONFIG_S3_ACCESS_KEY_ID=$S3_ACCESS_KEY RCLONE_CONFIG_S3_SECRET_ACCESS_KEY=$S3_SECRET_KEY \
     rclone copy s3:${S3_BUCKET:-ngteams} /backup'
  ```

  `copy` keeps what was deleted since, so an older dump still finds its blobs. To restore, copy the other way.
- **Redis** needs no backup. It holds the fan-out, the 15-minute push journal, presence, the sign-in counters and the
  media node registry. After it loses them, clients sync and report their presence again, and media nodes register at
  their next heartbeat.
- **`.env`** and the volume **`caddy-data`** (certificates, and with `local_certs` the authority clients trust).

### Adding nodes

Every API node serves every request ([Scaling](#scaling)). What all nodes share: the database, Redis (the same
namespace), the bucket with its keys, `public_origin`, and the TURN relay's URLs and secret. The signing keys are in
the database.

- **An API node**: copy `api-2` in `compose.yaml` as `api-3` and add `api-3:8080` to the `Caddyfile`'s
  `reverse_proxy`; then `docker compose up -d` and `docker compose exec caddy caddy reload --config
  /etc/caddy/Caddyfile`.
- **A media node** on the same host: copy `media` as `media-2` with another UDP port (published, and in its
  `NGTEAMS_SERVER_MIXER__UDP_LISTEN`) and `NGTEAMS_SERVER_MIXER__INTERNAL_URL: http://media-2:9090`. A new call goes
  to the live node with the fewest legs.
- **Nodes on other hosts** run the same image with the same shared settings. A media node's `mixer.internal_url` must
  be reachable from the API nodes on a private network, and its `mixer.public_addrs` from the relay. More relays take
  the same secret, each listed in `calling.turn.urls`.

### Accounts

The command line runs in a node, which has the configuration. A password comes on standard input (`-T` keeps it
apart from a terminal):

```sh
printf '%s\n' "$PASSWORD" | docker compose exec -T api-1 ngteams-server user add ada@example.com --name "Ada Lovelace"
docker compose exec api-1 ngteams-server user list
docker compose exec api-1 ngteams-server user disable bert@example.com
```

With the nodes down, `docker compose run --rm migrate user list` runs the same commands.

The first admin: `user add --admin`, or from ngteams. A node that starts without an admin logs a setup code
(`docker compose logs api-1 api-2 | grep 'setup code'`); `docker compose exec api-1 ngteams-server setup-code` makes a
new one. In ngteams' sign-in window choose "Own server" and type the server's address: it offers to create the
administrator with that code ([The first admin](#the-first-admin)). Admins then manage accounts in Settings, "Manage
users".

## Sign-in and tokens

- `POST /oauth2/v2.0/token` takes Entra's form: `grant_type=password` (`username`, `password`) for a sign-in,
  `grant_type=refresh_token` for every later token, each with `client_id` and one resource's `scope`
  (`<resource>/.default`, beside `openid profile offline_access`). The token's `aud` is that resource. The answer
  and the errors have Entra's shapes (`invalid_grant` with `AADSTS50126` for a wrong password, `50057` for a
  disabled account's password or refresh token, `70000` for a refresh token that is not valid), so the client's
  `TokenClient` and `Session` run unchanged with the server as authority.
- Refresh tokens are single use. Each grant returns the next one of the same sign-in. One presented again after
  `tokens.reuse_grace` revokes that whole sign-in.
- Password attempts are reserved per account before verification (in Redis when configured). Success clears the
  count. After `login.max_failures` the
  account's password sign-ins get 429 with `Retry-After` until the window ends.
- An account whose password must be changed first (an admin's reset with `must_change_password`) gets
  `invalid_grant` with `AADSTS50055` and `suberror: user_password_expired` for its right password, as Entra answers
  an expired one. It gets tokens again after `POST /api/account/v1/password`.
- An admin's access and ID tokens carry `roles: ["admin"]`; other accounts' tokens have no `roles` claim. The claim
  says what the account was when the token was minted (up to `tokens.access_lifetime` ago).
  `GET /api/account/v1/me` says what it is now.
- `POST /api/authsvc/v1.0/authz` takes an access token for `https://api.spaces.skype.com` and answers with a
  skypetoken, `region`, `partition` and `regionGtms`, as Teams' authz does. The skypetoken runs out when the
  exchanged token does. A disabled account gets 403 `UserNotFoundInSkypeTeam`.
- Access tokens, ID tokens and skypetokens are EdDSA JWTs with the issuer `<origin>/<tenant>/v2.0`. Each node checks
  them against the shared keys and reloads the keys when it meets a `kid` it doesn't hold. The tenant is one GUID per
  server, made by the first node and kept in the database. Accounts are `8:orgid:<oid>`.

## Accounts and administration

An account is an admin or not (`users.is_admin`). Admins manage accounts from ngteams through the admin API; the
operator has the same actions on the command line. All bodies are JSON.

### The first admin

- `GET /api/admin/v1/setup` (no credentials) answers `{"needs_admin": bool, "code_required": bool}`. `needs_admin`
  is true while the server has no enabled admin. The client asks this before sign-in and offers to create the admin.
- `POST /api/admin/v1/setup` with `{"code", "upn", "display_name", "password"}` creates the first admin (201, the
  account). It answers 403 `SetupCode` for a missing or wrong code, 409 `AdminExists` once an admin exists, and
  429 `TooManyAttempts` with `Retry-After` after `setup.max_attempts` attempts from one client address.
- The setup code: a node that starts without an admin issues a code and logs it once, at `warn`
  (`… setup code: XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`). `ngteams-server setup-code` issues a new code, which replaces
  the pending one, and prints it. The database keeps only the code's SHA-256, so any node checks it. Creating the
  first admin spends it. The code is read in any case, with or without the dashes.
- `setup.require_code = false` drops the code: whoever reaches the server first creates the admin.
- An admin made on the command line (`user add --admin`, `user promote`) also ends the setup.

### Admin API

Every request carries `Authorization: Bearer <access token for https://api.spaces.skype.com>`. Without a valid
token the answer is 401. The server looks the account up on every request: an account that is not an enabled admin
now gets 403 `NotAdmin` (or `AccountDisabled`), whatever its token's `roles` say.

| Request | Does | Answers |
|---|---|---|
| `GET /api/admin/v1/users?q=&after=&limit=` | lists accounts by UPN. `q` matches a part of the UPN or name, any case (on Postgres a name's case folds as the database's locale does). `limit` is 1–200 (default 50). `after` is the previous page's `next` | 200 `{"users": [account], "next": upn or null}` |
| `POST /api/admin/v1/users` | adds an account: `upn`, `password`, optional `display_name` (default: the UPN), `admin`, `must_change_password` | 201 account |
| `GET /api/admin/v1/users/{id}` | reads an account | 200 account |
| `PATCH /api/admin/v1/users/{id}` | changes any of `display_name`, `upn`, `enabled`, `admin` (assign or remove an admin). `enabled: false` signs the account out everywhere | 200 account |
| `DELETE /api/admin/v1/users/{id}` | deletes an account | 204 |
| `POST /api/admin/v1/users/{id}/password` | sets `password`, signs the account out everywhere; with `must_change_password: true` the account must replace it before it signs in | 204 |
| `GET /api/admin/v1/audit?before=&limit=` | lists admin actions, newest first. `before` is the previous page's `next` | 200 `{"entries": [entry], "next": id or null}` |

An account is `{"id", "mri", "upn", "display_name", "enabled", "admin", "must_change_password", "created_at",
"updated_at"}` (times in Unix seconds). A refusal is `{"error": {"code", "message"}}`:

| Status | `code` | When |
|---|---|---|
| 400 | `InvalidUpn`, `InvalidName`, `WeakPassword`, `SamePassword` | a UPN that isn't `name@domain`, a name over 256 characters, a password under 8 characters, a new password equal to the current one |
| 403 | `NotAdmin`, `AccountDisabled`, `SetupCode`, `InvalidCredentials` | |
| 404 | `NotFound` | no such account (a deleted one too) |
| 409 | `UpnTaken`, `LastAdmin`, `AdminExists` | |
| 429 | `TooManyAttempts` | with `Retry-After` |

A body that isn't the expected JSON gets axum's 400, 415 or 422.

Guards: a change that would leave the server without an enabled admin is refused with 409 `LastAdmin`. That covers
removing the role from, disabling or deleting the last enabled admin, by itself or by the command line. Each guarded
change runs in one transaction under a database lock, so two nodes can't each remove "the other" admin.

Deleting an account removes its sign-in: the UPN, password, role and refresh tokens go, and the UPN is free for a
new account (a new id). The row stays as a disabled account named "Deleted user" with the same id, so messages it
wrote keep an author. A deleted account's access tokens open nothing: authz and the admin API refuse it.

Audit: every change (`setup.first_admin`, `user.add`, `user.update`, `user.delete`, `user.password_reset`,
`user.password_change`) writes a row to `admin_audit` in the same transaction, and a `tracing` event at `info`. An
entry is `{"id", "at", "actor_id", "actor", "action", "target_id", "target", "detail"}`. `actor` is the admin's
UPN, `cli` or `setup`. `detail` holds the changed values, never a password. A refused change writes nothing.

### The account's own endpoints

- `GET /api/account/v1/me` (the same bearer token, any enabled account) answers the account as the database has it
  now. The client reads `admin` here to show its admin features.
- `POST /api/account/v1/password` with `{"upn", "current_password", "new_password"}` replaces the account's own
  password and signs it out everywhere (204). It takes no token, because an account that must change its password
  can't get one. The current password is the credential: a wrong one, an unknown account and a disabled account all
  get 403 `InvalidCredentials`. Attempts count toward `login.max_failures`, as sign-ins do (429 when held).

## Teams and channels

The client's team pages and channel dialogs find their routes at four GTMs: `chatSvcAggAfd` (CSA), `middleTier`,
`teamsAndChannelsProvisioningService` and `userProfileService`. The shapes are those of `docs/protocol/channels.md` and `teams.md`; each
route takes the access token of its own service's audience and looks the account up, so a disabled account's live
token opens nothing.

- **A team** has a thread id (`19:<id>@thread.tacv2`, its general channel's) and a group id (a GUID). Its creator
  is its owner. **A channel is a thread of the chat service**: a standard channel's members are the team's people,
  a private channel's the people its owners put in it (people of the team only). So a channel's messages, edits,
  reactions and read state are the chat service's, behind its membership check. Of a team's channels only the
  general one is in the conversation list; the others are read by their id.
- **Posts and replies.** A message posted to a channel is a post: its `conversationLink` ends in
  `;messageid=<its own id>`. A reply is posted to `<channel>;messageid=<post>` and is kept with the channel's
  messages under that post; the chain's own page (`…/conversations/<channel>;messageid=<post>/messages`) holds the
  post and its replies. A reply needs a post of that channel that isn't deleted. An archived channel, or a channel
  of an archived team, takes no new message.
- **CSA**: `GET api/v3/teams/users/me` lists the account's teams with the channels it may see, its role, the
  team's settings as `teamSettings`, and the account's `conversationFolders` (its channel sections). The sections'
  writes (`POST …/conversationFolders`, `PATCH …/batchUpdateExpandedState`) name the hierarchy version they were made
  against; another version answers 409 and changes nothing. A team's tags: `tagCards`, `tagSettings`, `tags`
  (`PATCH` with `if-match: <version>`, 412 for another version).
- **Middle tier** (`beta/…`): `teams/create`, `PATCH teams/<team>`, `DELETE teams/<team>/delete`, `joinCode`,
  `bulkUpdateRoledMembers`, `members` (`Promoted`, `Demoted`, `Removed`, `Left`, `DemotedAndLeft`),
  `sponsorJoinRequest`, `findJoinRequests`, `joinRequestAdminAction`, `joinRequest`, `joinByCode`, `suggested`,
  `find`, standard channels (`teams/<team>/channels[/<channel>]`, `setDefaultChannel`), the team's picture (PNG or
  JPEG, at most 4 MiB, kept in the file store) and `analytics/…/summary-timeseries` (counted from the channels'
  messages: posts, replies, reactions, mentions and the people who wrote, by day).
- **Provisioning**: private channels (`teams/<group>/channels`, their `users`, archive and restore), a channel's
  moderators (`channels/<channel>/members`), `groups/<group>/archive|unarchive|channels|clone`, the account's own
  `team/<team>/hide` and `conversations/show`, and `users/me/invitations` and `teamtemplates/v1.0/en-us`, both
  empty.
- **Chat service**: `PUT v1/threads/<thread>/properties?name=spaceAdminSettings` (a team's settings, on its general
  channel) and `?name=channelSettings` (a standard channel's moderation), from a team's owner;
  `PUT v1/threads/<channel>/members/<own mri>` (the account's own interest); and the account's own channel
  properties (`favorite`, `isGeneralChannelFavorite`, `channelNotificationSettings`, `followAllRc`, `notifyAllRc`,
  `alerts`) on `v1/users/ME/conversations/<channel>/properties`.
- **People pickers** (reads by `POST`, the middle tier's token): `beta/users/searchV2` with the text as a JSON
  string lists up to 15 accounts that can sign in whose name or address holds it (Add member's box);
  `<userProfileService>/api/v1.0/teams/<team>/users/profilesearch` with `{"keyword"}` lists the team's people the
  same way, for the team's people (a private channel's box). Each person carries its `jobTitle` when its
  [profile](#people) has one.
- **Graph**: `GET /graph/v1.0/me` (the token's account), which the client reads before each account-own write.

Who may do what, checked against the database on every request:

| Who | May |
|---|---|
| a team's owner | edit, archive, restore and delete it; add, promote, demote and remove people; read, make and take away its code; answer join requests; change its settings, tag settings and picture; archive, restore, show by default and moderate standard channels; copy it |
| a member | what the team's settings allow (`createTopic`, `updateTopic`, `deleteTopic`, `createPrivateSpace`); add members to a public team; ask a private team's owners to let people in; leave; tags when the tag settings allow it |
| a private channel's owner | rename, archive, restore and delete it; add, change and remove its people |
| anyone signed in | make a team; find and join public teams; join by a code; ask to join a private team by its group id |

A team and a private channel always keep an owner (403 `LastOwnerCannotBeRemoved`). When a private channel's only
owner leaves the team or is removed from it, the channel's longest-standing other member becomes its owner; a
channel with nobody else in it is deleted. Someone outside a team gets 404 `NotFound` for every CSA, middle-tier
and provisioning route that names it, and someone outside a private channel for every such route that names the
channel; the chat service's thread routes answer their own 403 to anyone not in the thread. A refusal is `{"errorCode", "message"}`; the codes the client's dialogs word are Teams' own
(`ChannelNameAlreadyExist`, `OperationNotAllowedOnArchivedTeam`, `NoPermissionToCreateChannel`,
`CannotCreatePrivateChannelDueToUserPolicy`).

These changes are published to the team's or channel's people as a `ThreadUpdate` on `push:<mri>`: a team made, edited,
archived, restored or deleted (the thread as it last was, with `isDeleted` true), its people and their roles, its
settings, and a channel made, renamed, archived, restored, deleted, moderated or given other people. Someone taken off a
team or out of a private channel, or gone from it by themself, is told it too, by an update that names no member
(`members` empty). A running client reads its teams again once such updates pause ([trouter.md](protocol/trouter.md)),
so its team and channel lists follow live. A channel's show-by-default, the team's picture, codes and tags reach other
clients only with their next teams read.

Limits: 200 standard and 30 private channels a team (`MaxChannelLimitExceeded`,
`MaxPrivateChannelLimitExceeded`), 200 tags a team, 200 sections an account and 1000 channels a section; a team's
code is 16 characters.

Not served yet: a channel's email address, team apps and templates, the default team picture, channel moderation
enforced on posts (the settings are kept and read back), and system
messages (`ThreadActivity/…`) for a team's changes.

## The calendar and meetings

The client's calendar, scheduling form and channel meetings find their routes on the middle tier, with the
account's token for `https://api.spaces.skype.com`; the shapes are those of `docs/protocol/calendar.md` and of
"A channel's meeting" in `calling.md`. Scheduling is served here; a meeting's call is [Meetings](#meetings).

- **Events**: `GET v2.0/me/calendars/default/calendarView` (a window of at most 100 days; the client asks for 62
  at most), `POST v2.0/me/calendars/default/events`, and `GET`, `PUT`, `DELETE v2.0/me/calendars/events/<id>` with
  `…/cancel` and `…/respond`. Times are kept as instants and answered in UTC; an event keeps the zone it names
  (`eventTimeZone`, by the middle tier's names: `TurkeySt`) and `beta/me/calendarEvents/timeZoneSettingsWithOffset`
  names an IANA zone that way. What is kept of a sent event is a fixed list of the form's own fields; who
  organizes it, who answered what and the meeting's links are the server's. An invitee's copy carries the invitee's
  own reminder, Outlook's default of 15 minutes (`isReminderSet`, `reminderMinutesBeforeStart`), not the organizer's.
  A calendar request's body is at most
  1 MiB (an event's text can be long); a larger one is refused (400 `BadRequest`).
- **A series** (`eventRecurrencePattern`, `eventRecurrenceRange`) is one row, read as its occurrences: the rule
  runs on the clock of the series' own zone (the `rrule` crate), so an occurrence keeps its time of day across a
  daylight-time change. An occurrence's id is the series' id, a dot and the occurrence's own start. One occurrence
  changed, answered, cancelled or taken off a calendar is kept as a row of its own under the series; a change of the
  series' rule or times forgets those rows. An answer or a removal of an occurrence that meets a change or a
  cancellation of its series is checked again against the series as it then is; one that keeps meeting changes is
  refused (409 `Conflict`).
- **Whom an event invites** are accounts of this server, named by MRI or by address (the sign-in name); another
  address is refused (400 `UnknownAttendee`), since the server sends no mail. Each of them has the event on its
  own calendar with its own answer (`respond`), reads whom it invites, and may take it off (`DELETE`). The
  organizer's change reaches everyone it now invites (a new time, of the event or of one occurrence, asks each of
  them again; who is taken off no longer has it), and its cancellation leaves the event cancelled (`isCancelled`, `Canceled: …`) on their
  calendars until they take it off. An event nobody else has is deleted outright.
- **A Teams meeting** is a chat-service thread, `19:meeting_<id>@thread.v2`, made by
  `POST beta/me/calendarEvents/privateMeeting/schedulingService/create` (`Scheduled` for the event the form then
  saves with it, or `MeetNow`, which has no event). Its join link is
  `<origin>/l/meetup-join/<chat>/0?context={"Tid","Oid"}` and its short link `<origin>/meet/<id>?p=<passcode>`;
  the thread's `properties.meeting` names its organizer, tenant, title and link. An event takes only a meeting its
  own organizer made and no other event uses, checked as the event is written; an event changed to a Teams meeting
  gets one, and so does a series one of whose occurrences is. The people the event
  invites are put in the chat, those one occurrence of a series invites too; who no longer has an invitation to
  the event or to any occurrence of it is taken out. The chat holds the invitation: a message whose `properties.meeting` names the
  meeting (`ScheduledMeetingCreated`), changed in place when the meeting is, with a cancellation
  (`ScheduledMeetingCancelled`) after it.
- **A channel's meeting**: `POST v2.0/groups/smtp:<team mailbox>/calendars/default/events`, and for its `iCalUID`
  `GET …/calendars/events/iCalUId/<uid>`, `PUT …/calendars/events/<uid>` and `…/<uid>/cancel`. The team's mailbox
  is `<group id>@<the origin's host>`. The meeting is posted to its channel (its link names the channel and that
  post), its change edits the post in place and its cancellation replies under it. Who scheduled it has an
  accepted copy on its own calendar, as has anyone it names and, with `inviteAllMembers`, everyone in the channel.
  A change or cancellation of one occurrence names it by `fetchEventContract`: its kind and its start, which for
  an `Exception` is the start it was moved to. An `Exception` no live changed occurrence starts at
  answers 404: a cancelled one is not found again, and neither is the series' own occurrence at that time.
  A change that would put an occurrence at the start another live occurrence of its series is at, moved there or
  there by the series' rule, answers 409 `Conflict`: the start alone would not tell the two apart. An occurrence
  cancelled or moved away leaves its start free.
- **The Scheduler**: `PUT v2.0/me/calendars/schedules` (each person's busy times; what an event is about only
  for the caller's own) and `findMeetingTimes` (the half hours, 08:00 to 18:00 counted from the midnight the
  read starts at, at which everyone asked about is free).
- **Meeting options**: `GET` and `POST beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0`, kept per chat
  (a meeting's, or a group chat's for its calls): `AutoAdmittedUsers`, `PresenterOption`, `AllowMeetingChat`,
  `AttendeeRestrictions`. A meeting's call goes by them while it runs ([Meetings](#meetings)).
- **Rooms**: `GET v2.0/places/roomLists`, `places/rooms` and `places/<list address>/roomList/rooms` list the
  `[[rooms]]` of the configuration. A room is a place, not an account: an event that names one invites nobody by
  it.

Who may do what, checked against the database on every request:

| Who | May |
|---|---|
| an event's organizer | read, change, cancel and delete it |
| an account it invites | read it with whom it invites, answer it, take it off its own calendar |
| who scheduled a channel's meeting | change and cancel it on the team's calendar |
| anyone in a meeting's channel (for a standard channel, anyone on the team) | read the channel's meeting from the team's calendar; schedule one there, while neither the team nor the channel is archived |
| the people in a meeting's or a group chat | read its meeting options; the organizer the path names saves them (a meeting's own organizer) |
| anyone signed in | make an event or a meeting of its own; ask for any account's busy times; list the rooms |

Anyone else gets 404 `NotFound` for an event, a meeting or a team's calendar: it learns nothing of it. A private
channel's meeting is hidden that way from the team's people outside the channel. An invited account that tries to
change or cancel gets 403 `Forbidden`.

What reaches the invited accounts beside their calendars, on `push:<mri>`: the meeting chat's `ThreadUpdate` and
the invitation, change and cancellation as that chat's (or the channel's) messages, and for each a `NewMessage` of
the account's `48:notifications` stream: an activity item (`msGraph`, `privateMeetingCreated`,
`privateMeetingUpdated`, `privateMeetingCanceled`, or `channelMeeting…`) naming who did it, the meeting's time and
the event it opens. The item is kept in the account's activity feed as a mention's is (see "The account's own
streams"), so a client that was away syncs it, and the account marks it read or removes it. An invitation is
always written; a change or a cancellation is not written for an account that muted the meeting's chat or channel
(`alerts` `false`), and nothing is written for a disabled account.

Not served: mail of any kind (invitations, answers and their notes reach nobody by mail; an answer's note is not
kept), the form's delete of an unused online meeting, `masterCategories`, an event by its join link, forwarding,
reminders, and `AllowMeetingChat` (the meeting chat takes messages whatever it says).

## Workflows with incoming webhooks

The client's Workflows (`docs/protocol/workflows.md`) talks to Power Automate's Flow API and Power Apps' connection
routes; for an own-server account their bases are `/workflows/` and `/workflow-connections/` on the origin, with the
account's tokens for `https://service.flow.microsoft.com/` and `https://service.powerapps.com/`, which the token
endpoint issues while `workflows.enabled` is on. What is served is what "Send webhook alerts to a channel" and "… to
a chat" need:

- **The gallery**: the channel dialog's catalog (`galleries/public/teamsworkflowtemplates`, filtered by category as
  the client asks), the Workflows app's gallery (`galleries/public/templates` with `$filter`, `$search`, `$top`) and
  a template's read, holding only those two templates under Teams' ids. Every other template needs a Microsoft
  connector and isn't offered.
- **The environment** (`environments`): one, `Default-<tenant id>`.
- **Connections**: the account's Microsoft Teams connection, made when the card editor opens (`PUT
  …/apis/shared_teams/connections/<name>`) and connected at once, so the client signs nothing in.
- **Workflows**: create, read, list (the account's own; `flowState=active` for those turned on), rename and edit
  (`PATCH`), `start`, `stop`, delete; `owners`, `modifyowners`, `owners/<id>`; `triggers`;
  `triggers/<trigger>/listCallbackUrl` (the webhook link); `runs` and `runs/<run>/actions`. A save is refused (400
  `WorkflowNotSupported`) unless its definition is a Teams webhook trigger posting to one channel or chat.
- **The webhook**: `POST <origin>/workflows/automations/direct/workflows/<id>/triggers/manual/paths/invoke?api-version=1&sp=…&sv=1.0&sig=<secret>`,
  with no token. It answers 202 and posts what it got into the channel or chat as the Workflows bot (`28:358f0194-…`,
  "Workflows"), through the chat service, so sync and push deliver it as any message.

While `workflows.enabled`, authz names the GTM `workflowsService` (`<origin>/workflows`). With
`workflows.enabled = false` the server serves none of this, issues no token for those two audiences and names no
`workflowsService`, so the client offers no Workflows ([Optional services](#optional-services)).

| Who | May |
| --- | --- |
| a workflow's owner (its maker and the co-owners it adds) | read, rename, edit, turn off and on, delete it; read its link, owners, runs; add a co-owner who can see where it posts, take a co-owner off (the maker stays); edit it in place while it can see where it posts, move it only to a channel or chat that each enabled owner can see |
| the maker of a workflow | save it only to a channel of a team it is on (a private channel's own people only) or a chat it is in, neither archived |
| anyone with its link | post to it |

Anyone else gets 404 `FlowNotFound`: it learns nothing of the workflow.

**What a webhook takes**: the Teams Workflows payload, `{"type": "message", "attachments": [{"contentType":
"application/vnd.microsoft.card.adaptive", "content": <card>}]}` (each card becomes one message, at most 10; hero and
thumbnail cards too), a bare Adaptive Card, or `{"text": "…"}` (a text message). A card must be one the client's
renderer draws: `type` `AdaptiveCard`, every element and action of a named type, containers nested at most 16 deep.

| Answer | When |
| --- | --- |
| 202 | posted, or taken but posted nowhere because none of its owners may post there any more (a failed run, `Forbidden`) |
| 400 `InvalidRequestContent` | not one of the forms above, or a card the client can't draw (a failed run) |
| 404 `WorkflowNotFound` | a wrong or missing secret, another trigger, a deleted workflow |
| 409 `WorkflowTriggerIsNotEnabled` | the workflow is turned off (no run) |
| 413 | over `workflows.max_body_bytes` |
| 429 `WorkflowRequestsThrottled` | over `workflows.rate_per_minute`, with `Retry-After` |

The secret is 256 random bits, compared by digest; the webhook's route has its own trace span that names its path
only, so the secret reaches no log. A workflow's link never changes: to replace it, delete the workflow and save it
again. Workflows, owners and runs are rows of the database, so any node takes any webhook.

```sh
curl -sS -X POST "$WEBHOOK_LINK" -H 'Content-Type: application/json' -d '{
  "type": "message",
  "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {
    "type": "AdaptiveCard", "version": "1.4",
    "body": [{"type": "TextBlock", "text": "Build 1042 passed", "weight": "bolder"}]
  }}]
}'
```

Not served: any other template or trigger (schedules, Planner, SharePoint, email, AI), a workflow run from a
message, a run by hand, the input-run runtime (`shared_logicflows`), connector operations and their choices, and
paging of the lists (a list is whole, without `nextLink`).

## Apps and bots

The server becomes a Bot Connector for bots built with the Bot Framework, Teams or Microsoft 365 Agents SDKs. It
registers apps and bots, gives each bot its credentials and the keys to check the server with, delivers chat events
to bots as activities, serves the connector routes a bot answers on and passes a person's invokes to the bot
([below](#invokes)), and serves the app catalogue the client reads and the adds and removals it sends ([below](#the-app-catalogue-and-installs)). Invokes, reactions and replies work in any chat the bot is in. A bot's own start of a
group chat or channel conversation is not served yet.

**A bot is a principal in chats**: a `users` row of kind `app` with the bot's id, MRI `28:<id>`, no password and no
sign-in. No people read finds it: not the admin's account list or account changes, not a sign-in, not a profile
lookup by MRI or address, not a people search (`users.kind`, read through one people scope). An app has at most one
bot, as a Teams manifest does; its bot's id is the manifest's `botId`, so a bot keeps the `MicrosoftAppId` it has.

### Registering an app

Admins only, with the same bearer token and refusals as the [account routes](#admin-api); every change is audited
(`app.add`, `app.package`, `app.update`, `app.secret`, `app.delete`, the app's name as `target`). A refused body is
400 `InvalidRequest`.

| Request | Does | Answers |
|---|---|---|
| `POST /api/admin/v1/apps` | registers a bot from the form: `name` (1–30 characters), optional `description`, optional `endpoint` (an absolute `https` URL without credentials; `http` with `bots.allow_http_endpoints`; an address written in it only on a network the server sends to, as the address guard below says; without one the bot has none until a `PATCH` sets it), `auth_mode` (`signed`, the default, or `unsigned` with `bots.allow_unsigned`). The app and its bot share a new id | 201 app with `secret` |
| `POST /api/admin/v1/apps/package` | registers the app a Teams app package holds (`Content-Type: application/zip`, at most 5 MiB): `manifest.json` (a GUID `id`, `version`, `name.short` up to 30 characters, `description`, `developer.name`, `accentColor` `#RRGGBB`, at most one bot with a GUID `botId` and scopes of `personal`, `team`, `groupChat`) and its two icons (PNG, color 192×192, outline 32×32, kept in the file store as `apps/<id>/color.png` and `outline.png`). The bot has no endpoint until a `PATCH` sets it | 201 app, with `secret` when it has a bot |
| `PUT /api/admin/v1/apps/{id}/package` | a new package of the same app and bot: version, names, manifest, icons. The bot keeps its endpoint, mode and secret | 200 app |
| `GET /api/admin/v1/apps` | every app, by name in any case, then by id | 200 `{"apps": [app]}` |
| `GET /api/admin/v1/apps/{id}` | one app | 200 app |
| `PATCH /api/admin/v1/apps/{id}` | any of `enabled`, `everyone` (every account may use it) and `users` (the account ids that may when not), the bot's `endpoint` and `auth_mode` | 200 app |
| `POST /api/admin/v1/apps/{id}/secret` | a new secret for its bot; the old one stops working at once | 200 `{"secret"}` |
| `POST /api/admin/v1/apps/{id}/test` | sends its bot a test activity (below) | 200 `{"delivered", "status", "error", "elapsed_ms"}` |
| `DELETE /api/admin/v1/apps/{id}` | deletes the app; its bot gets no token any more, and its row stays (deleted, with its name) for what it posted. The same bot id may be registered again | 204 |

An app is `{"id", "version", "name", "full_name", "short_description", "full_description", "developer",
"accent_color", "source" ("form" or "package"), "enabled", "everyone", "users", "manifest", "bot", "created_at",
"updated_at", "health"}`; `bot` is null or `{"id", "mri", "endpoint", "auth_mode", "scopes"}`; `health` is null
without a bot, else its delivery health: `{"state", "failures", "paused_until", "last_error", "last_success",
"outbox": {"pending", "sent", "failed"}}`. `state` is `paused` (until `paused_until`, ms), `failing` (its last send
failed; `failures` in a row since the last pause, which starts a new count, `last_error` why), `healthy` (its last send went; `last_success`, ms) or `new`; the
outbox counts are the rows of the last day. The secret (256 random bits,
base64url) is in the answer that made it and nowhere else: the database keeps its SHA-256, and no read, list, log or
audit entry holds it.

| Status | `code` | When |
|---|---|---|
| 400 | `EndpointNotAllowed` | an endpoint whose written address is a private, loopback or link-local one that `bots.allowed_networks` doesn't name; a name is checked at each send |
| 400 | `EndpointMetadata` | an endpoint whose written address is a cloud metadata address, which no setting opens |
| 400 | `InvalidName`, `InvalidEndpoint`, `UnsignedNotAllowed`, `UnknownUser`, `NoBot`, `InvalidPackage`, `BotChanged`, `InvalidRequest` | `message` says what is wrong; `UnknownUser` also for a bot's id |
| 404 | `NotFound` | no such app |
| 409 | `AppExists`, `BotExists` | the app id is registered; the bot id is another app's bot, a person's or the Workflows bot's |
| 400, 409 | `NoEndpoint`, `AppDisabled` | a test activity to a bot without an endpoint, or of an app that is off |
| 429 | `TooManyRequests` | a test activity over the bot's send rate; `Retry-After` says when |
| 413 | | a package over 5 MiB |

The operator has the same on the command line: `app add --name <name> [--endpoint <url>] [--description <text>]
[--unsigned]` or `app add --package <file.zip>` print `added <id> (<name>)` and `secret <secret>`; `app list` prints
`<id>\t<enabled|disabled>\t<name>\t<28:bot or ->`, in the same order; `app disable|enable|rotate-secret|remove <id>`.

### A bot's credentials

A bot gets the server's token with MSAL's client-credentials request, unchanged:

- `POST /<tenant>/oauth2/v2.0/token` (the tenant: the server's id, or `botframework.com`) or `POST /adfs/oauth2/token`,
  form `grant_type=client_credentials`, `client_id` (the bot's id), `client_secret` (or both as HTTP Basic),
  `scope=<bots.scope>/.default`; anything else MSAL adds is ignored. The OpenID metadata MSAL reads first is at
  `/<tenant>/v2.0/.well-known/openid-configuration` and `/adfs/.well-known/openid-configuration`; msal-python takes an
  `/adfs` authority without asking Microsoft, so a Python bot uses `https://<origin>/adfs` as its authority.
- The answer is Entra's: `{"token_type": "Bearer", "expires_in", "ext_expires_in", "access_token"}`. The token is the
  server's EdDSA JWT with `aud` `bots.scope`, `appid`, `azp` and `sub` the bot's id, `tid`, `idtyp: "app"`; it has no
  `oid`, so it never passes for a person's token, and no person's token is ever issued for `bots.scope`.
- Refusals take Entra's shape and codes: `invalid_client` `AADSTS7000215` (401) for a wrong secret or an unknown bot
  alike, `7000216` without a secret; `unauthorized_client` `7000112` (400) for a disabled app or an `unsigned` bot;
  `invalid_scope` `70011`; `unsupported_grant_type` `70003` for any other grant; `invalid_request` `90002` for another
  tenant. Wrong secrets count per bot as password attempts do (`login.max_failures` in `login.window`, across nodes
  with Redis); after that a wrong secret gets 429 `temporarily_unavailable` with `Retry-After` until the window ends.
  The right secret is never held off (a bot's id is public), and it clears the count.

### The app catalogue and installs

While `apps.enabled`, authz names `appsService` (the middle tier's base), so the client offers the Store, an app's
details, a chat's and a team's apps and the bot buttons ([Optional services](#optional-services)). The middle tier
serves the catalogue in Teams' shapes ([apps.md](protocol/apps.md)). An account sees only the apps it may use: enabled,
and for everyone or for it.

| Request | Answers |
|---|---|
| `POST beta/users/apps/store` | the apps, by name, in `appCategoriesDictionary.adminPreInstalledApps` ("Added by your org") and `popularApps` (which a team's "Add an app" reads); no ratings |
| `POST beta/users/apps/batchedDefinitions` | the definitions of the asked ids, in their order |
| `POST beta/users/apps/aggregatedEntitlements` | the account's apps (`Installed`; the others `Featured`), every definition, and with team threads in the body each team's apps (`teamsEntitlements`) |
| `POST beta/teams/apps/aggregatedEntitlements` | each asked team's apps the account is on, `Installed`, with the definitions |
| `GET beta/chats/<chat>/apps/chatentitlements` | a chat's apps for one of its people (else 403), with their definitions as a list |
| `POST beta/users/apps/entitlements`, `DELETE …/<app>` | the app for the account itself; its bot's 1:1 chat with the account is made, or the bot put back in it. On removal the bot leaves that chat: the account keeps it, and the bot gets nothing of it and posts nothing there (403) until an add |
| `POST beta/chats/<chat>/apps/entitlements`, `DELETE …/<app>` | the app on a group chat the account is in (a 1:1 chat or a channel is 400); its bot joins the chat as a person is added (`ThreadActivity/AddMember`), and leaves it on removal |
| `POST beta/teams/<team>/apps/entitlements`, `DELETE …/<app>` | the app on a team, by its owners, its members while the team's `installApp` setting (`uninstallApp` for a removal) lets them, and administrators; its bot joins the team's standard channels (and each one made later), and leaves every channel of the team on removal |
| `GET /teamsappdata/apps/<id>/color.png`, `outline.png` (no token) | an enabled app's icons from the file store, as Teams' CDN serves them |

An add takes the app's definition as its body (its `id` names the app) and answers 201 `{"value": {"id", "state":
"Installed", "installedVersion", "createdTime", "installedBy", "installerIdentity", "botMetadata"}}`; a removal answers 200.
An app whose bot doesn't serve the place (`personal` for the account, `groupChat` for a chat, `team` for a team) is 400; an app the account may
not use is 404. A definition is rendered from the app's manifest: `id`, `name`, `version`, `developerName`,
`developerUrl`, `privacyUrl`, `termsOfUseUrl`, `shortDescription`, `longDescription`, `accentColor`, `bots` (`id`,
`scopes`, `commandLists`, `isNotificationOnly`), `inputExtensions` (the manifest's `composeExtensions`),
`staticTabs`, `configurableTabs`, `permissions`, `validDomains`, and for a packaged app `largeImageUrl` and
`smallImageUrl` on the origin (`?v=` the app's last change).

**What the bot gets.** An add or a removal is a row of `app_installs`, written in one transaction with the bot's two
activities in the outbox, so it gets them as it gets any other: on an add `installationUpdate` (`action` `add`) then
`conversationUpdate` (`membersAdded` `[{"id": "28:<bot id>"}]`); on a removal `conversationUpdate`
(`membersRemoved`) then `installationUpdate` (`action` `remove`). For the account itself they are in its 1:1 chat
with the bot (`personal`); for a group chat in the chat (`groupChat`); for a team in its general channel
(`conversationType` `channel`, the team's thread as `conversation.id`, `channelData.team` `{"id", "name",
"aadGroupId"}`, `channelData.channel`, `teamsTeamId`, `teamsChannelId`, and `channelData.eventType`
`teamMemberAdded` or `teamMemberRemoved` on the `conversationUpdate`).

**People who come and go.** When people are added to a group chat or a team where the bot is, join the team, are
taken out or leave, the bot gets `conversationUpdate` with `membersAdded` or `membersRemoved` (each person as
`{"id": "29:<object id>", "aadObjectId"}`), `from` the person who made the change, in the chat or (for a team) in its
general channel as above. The row is written in the roster change's own transaction. A role change, a bot's own
seat and a private channel's people tell it nothing. A bot is not told yet when a channel is made (`channelCreated`).

**Groups and channels.** In a group chat or a channel the bot gets only a message that mentions it. In a channel the
activity's `conversation.id` is the post's reply chain, `<channel>;messageid=<post>`, with the channel and team in
`channelData` as above. Its `text` names each mention as `<at>name</at>`, and `entities` holds each mention
(`{"type": "mention", "mentioned": {"id", "name"}, "text": "<at>name</at>"}`) after the `clientInfo`. A bot replies
in the post's thread by posting to that reply chain. A removed bot is in none of the chat or the team's channels, so
it gets nothing more and posts nowhere there (403).

### Bot chats and delivery

**A bot chat** is the 1:1 chat `19:<a>_<b>@unq.gbl.spaces` of a person and a bot (the two ids in order), as Teams
names it. There is no create call: a person's first message to that id makes the chat when the bot's app is enabled
and the person may use it (`everyone`, or listed in `users`); otherwise the post is 404 as for any unknown chat. A
bot makes the same chat with `POST v3/conversations` under the same rule. In the chat's members and thread reads the
bot is `28:<id>`. A bot's message gets a numeric `clientmessageid` from the server, as a Microsoft bot's message has,
so a person's read receipt can name it.

**To a bot.** In a 1:1 chat the bot gets each message (`message`), edit (`messageUpdate`, `channelData.eventType`
`editMessage`), delete (`messageDelete`, `softDeleteMessage`) and typing (`typing`) of the person; in a group chat only
a message that mentions it. In any chat it gets a reaction put on or taken off one of its own messages
(`messageReaction`, `replyToId` the message, `reactionsAdded` or `reactionsRemoved` `[{"type": <key>}]`). An activity is the `msteams` channel's: `channelId: "msteams"`, `serviceUrl`, `from`
`{"id": "29:<account id>", "name", "aadObjectId"}`, `recipient` `{"id": "28:<bot id>", "name"}`, `conversation`
`{"id", "conversationType": "personal", "isGroup": false, "tenantId"}`, `channelData.tenant.id`, `timestamp`,
`localTimestamp`, `locale`. A message's `id` is the chat service's message id; its `text` is the message's plain text,
with its HTML beside it as a `text/html` attachment. No account's token is ever in an activity.

Each activity is a row of the outbox (`bot_deliveries`), written in the chat write's own transaction. Every API node
sends rows: it takes a row's lease (`bots.delivery_lease` seconds) with a compare-and-set update, so one node sends it
at a time, and a row waits while an older one of the same bot and chat is pending. A write wakes every node's sender
through the fan-out (`bots:wake`); a two-second poll covers a lost wake-up. A send that the bot doesn't answer with
2xx in `bots.send_timeout` seconds is tried again after each of `bots.retry_delays` (by default 15 s and 45 s: three
attempts in about a minute), then the row is `failed`; typing is tried once. A planned stop (SIGTERM) sends each
activity once: the node takes no more rows, and the sends under way end and are recorded before it exits (it waits
at most `bots.delivery_lease` seconds). Give a stopping API node 10 s for its HTTP requests, that wait and 10 s more
before a kill: the Compose file gives 60 s. After a crash or a kill, a bot can get an activity twice when its lease
runs out, as with Bot Framework. Its id is the same each time, so a bot can drop the repeat. After 20 failed sends in
a row a bot gets nothing for a minute (`bot_health`). Sent and failed rows go after a day.

**Send limits.** The server sends one bot at most `bots.send_rate` activities a second (a burst of `bots.send_burst`),
on each node; an activity over it goes back to the outbox, due when the bot's rate has room, and the wait is no
attempt. A bot's calls to the connector keep to `bots.connector_rate` a second per app (a burst of
`bots.connector_burst`), on each node, checked after its credential; more get 429 with the Bot Framework's
`{"error": {"code": "TooManyRequests"}}` and `Retry-After` in seconds. Both are in each node, so a deployment of
several API nodes lets a bot have that much from each.

**The test activity** (`POST /api/admin/v1/apps/{id}/test`, "Send a test activity" in Manage apps) sends the bot one
`event` activity, `name` `ngteams/testActivity`, `value` `{"sentAt"}`, from the administrator in their 1:1 chat with
the bot, as any activity goes: to the registered endpoint through the address guard, signed when the bot is, within
its send rate. The answer says whether the bot took it (2xx), its status and, when it didn't, why (the bot's status,
or the guard's or the connection's reason in a person's words: for example "the connection was refused", "the name did
not resolve", "the TLS handshake failed" or "the request timed out"). It is no chat event: no outbox row, and the bot's health stays as its
deliveries left it. No account's token is in it.

**The address guard.** A bot's endpoint is set by an administrator, and the server checks every send: `https` only
(`http` with `bots.allow_http_endpoints`), no user info, no redirects followed, no proxy, the answer read up to
256 KiB. The connection goes only to addresses the guard's own resolver passed, so a name can't answer one way for the
check and another for the connection. Public addresses pass; a private, loopback, link-local, shared or otherwise
reserved one passes only when it is in `bots.allowed_networks` (CIDR, for a self-hosted bot on a private network);
a cloud metadata address (`169.254.169.254`, `fd00:ec2::254`, `100.100.100.200`) never. An address written in the
endpoint is checked when an administrator saves it (400 `EndpointNotAllowed`, or `EndpointMetadata` for a metadata
address); a name, which may resolve elsewhere
later, at each send. A refused address fails the row at once.

**Signed and unsigned.** A signed bot's activity carries the server's RS256 token (above); the bot answers with its own
token at `serviceUrl` `<bots.service_url>` (`<origin>/bot/` by default). An unsigned bot (`auth_mode` `unsigned`,
with `bots.allow_unsigned`) gets no token and sends none: its own `serviceUrl` is
`<bots.service_url>u/<bot id>.<credential>/`, the credential derived from its secret, so a new secret makes a new
one. Its endpoint takes anyone's posts, so use it for development or on a private network.

### The connector routes

Under the `serviceUrl` (`/bot/…`, or an unsigned bot's `/bot/u/<bot id>.<credential>/…`). A doubled slash and any
letter case in the fixed segments are taken, as the SDKs join URLs differently. A signed bot sends
`Authorization: Bearer <its token>`; a person's token, an unsigned bot's or no token is 401. A bot of a disabled app
is 403.

| Request | Does | Answers |
|---|---|---|
| `POST v3/conversations/{id}/activities`, `…/activities/{replyToId}` | a `message` as the bot (in a channel, `{id}` `<channel>;messageid=<post>` puts it in the post's thread): `text` in `textFormat` `markdown` (the default; turned into HTML with pulldown-cmark), `plain` or `xml`, cleaned with ammonia (no scripts, styles, handlers or pictures; links `https`, `http`, `mailto`); card attachments (Adaptive, hero, thumbnail, OAuth and sign-in cards, up to 10, with `suggestedActions`) as one `RichText/Media_Card` message after the text. A `typing` activity is the bot typing | 200 `{"id"}` (the last message's) |
| `PUT v3/conversations/{id}/activities/{activityId}` | replaces the bot's own message. If the activity has cards with text or pictures, the server returns 400 because it makes two messages. | 200 `{"id"}` |
| `DELETE v3/conversations/{id}/activities/{activityId}` | the bot's own message, soft-deleted | 200 |
| `GET v3/conversations/{id}/members`, `…/activities/{activityId}/members` | the chat's people as `TeamsChannelAccount` (`id` `29:<id>`, `name`, `aadObjectId`, `objectId`, `givenName`, `surname`, `email`, `userPrincipalName`, `tenantId`, `userRole`) | 200 list |
| `GET v3/conversations/{id}/pagedmembers` | the same | 200 `{"members"}` |
| `GET v3/conversations/{id}/members/{member}` | one, by `29:<id>`, `8:orgid:<id>`, id or address | 200 account |
| `POST v3/conversations/{id}/attachments` | keeps a picture for the chat (`AttachmentData`: `type`, `name`, `originalBase64`; PNG, JPEG, GIF or WebP, limited by `media.max_picture_bytes` and the 4 MiB JSON request limit; base64 permits less than 3 MiB of picture bytes) as a media object of the bot's own, made for that chat | 200 `{"id"}` |
| `GET v3/attachments/{id}`, `…/views/original` | a picture the bot made or one of a chat it is in: its `AttachmentInfo` (`name`, `type`, `views` `[{"viewId": "original", "size"}]`), or its bytes | 200 |
| `POST v3/conversations` | the bot's 1:1 chat with one person (`members[0].id` `29:<id>` or `8:orgid:<id>`, or `aadObjectId`) who may use its app, with `activity` posted in it when given. 403 when the chat is there but the person removed the app | 201 `{"id", "activityId", "serviceUrl"}` |
| `GET v3/teams/{id}` | the team of a bot in it: `TeamDetails` (`id`, `name`, `aadGroupId`, `type`, `channelCount`, `memberCount`) | 200 |
| `GET v3/teams/{id}/conversations` | its standard channels, `{"conversations": [{"id", "name"}]}`, the general one without a name | 200 |
| `v3/batch/…`, other `v3/teams/…`, `v1/meetings/…`, `…/history` | not served | 501 |

A message's picture attachment (`contentType` `image/…`) names a picture the bot kept (its `contentUrl` the
`…/v3/attachments/{id}/views/original` above) or carries one as a `data:` URL (kept then, as an upload is). The
message shows it as a pasted picture does ([Pictures in messages](#pictures-in-messages-ams-ams)): an `<img>` of
the object's `imgo` view after the text, read by the chat's people alone. An uploaded picture can appear only in
its upload chat. A delete or edit releases a picture when no message uses it. A refused request releases each new
picture that no message uses. A request may be up to 4 MiB.

A bot acts only in a chat it is in (403) and changes only its own messages (403). Refusals take the Bot Framework's
`{"error": {"code", "message"}}`: 400 `BadArgument` (not an activity, no text or card, an attachment that isn't a card
the client draws or a picture the server keeps, a group conversation, an update that makes more than one message),
401 `Unauthorized`, 403 `Forbidden`, 404 `NotFound` (also when a picture reference disappears before a post or update
commits), 429 `TooManyRequests` with `Retry-After` (over `bots.connector_rate`), 507 `InsufficientStorage` (the
bot's pictures fill its `media.user_quota_bytes`).

### Invokes

`POST /api/chatsvc/<region>/v1/agents/28:<bot>/invoke` with a person's `ic3` bearer token: a card's button, a
message extension's command, search or dialog ([bots.md](protocol/bots.md)). The server checks that the person may
use the bot's app (`appId` the app's or the bot's id; else 404), and that both are in `conversation.id` now (a chat,
or a channel post's `<channel>;messageid=<post>`; else 403) and the person may read `messageId` there (else 404).
The channel's `;messageid=` suffix must also name a message that the person may read, even with a separate `messageId`.
Without a conversation (an app's commands asked with none open) the invoke names their 1:1 chat. Then it posts one
activity to the bot, signed as a delivery is and through the same address guard, and waits at most
`bots.send_timeout` seconds; nothing is kept and nothing is sent again, so any node serves it.

The activity is an `invoke` with the request's `name` and `value` (`replyToId` the `messageId`), addressed as a
delivery is; a card's plain submit (`name` `message`) is a `message` activity with the `value` and a `messageBack`'s
`text`. Nothing else of the request goes to the bot: a `userAadToken` a client sends is never read.

| The bot's answer | The person gets |
|---|---|
| 2xx, empty | the same status, empty |
| 2xx, `{"composeExtension": …}` or `{"task": …}` | the same status, with `responseType` added |
| 2xx, other JSON (an `Action.Execute`'s `{"statusCode", "type", "value"}`) | that JSON as it is |
| 400–599 but 401, 403, 407 | the bot's status and JSON body |
| 401, 403, 407, a redirect, a body that isn't JSON, no endpoint, a refused address | 502 |
| none within `bots.send_timeout` | 504 |


### Sign-in cards

The server is a bot's token service (the Bot Framework's `OAuthUrl`, the Teams SDK's `tokenServiceUrl`): a signed
bot calls it at the origin with its own token, an unsigned bot under its own `serviceUrl`
(`<origin>/bot/u/<bot id>.<credential>/api/…`). It needs `bots.token_key`.

**Connections.** An administrator gives a bot OAuth connections, by name, through the admin API: an identity
provider's authorize and token URLs (the token URL passes the address guard, as a bot's endpoint does), the client id
and secret the provider gave, and the scopes. The provider sends people back to `<origin>/api/oauth/callback`: register
that redirect with it.

| Request | Does | Answers |
|---|---|---|
| `GET /api/admin/v1/apps/{id}/connections` | the app's bot's connections, by name, never a secret | 200 `{"value": [{"name", "authorize_url", "token_url", "client_id", "scopes", "redirect_uri", "created_at", "updated_at"}]}` |
| `PUT /api/admin/v1/apps/{id}/connections/{name}` | `authorize_url`, `token_url`, `client_id`, `client_secret` (needed for a new one; a change without it keeps the old one), `scopes` (space-separated). A name has 1–64 letters, digits, `-`, `_` or `.`. A change keeps the connection's sign-ins and people's tokens | 201 new, 200 changed: the connection |
| `DELETE /api/admin/v1/apps/{id}/connections/{name}` | the connection, its sign-ins and every person's token for it | 204 |

Refusals: 400 `InvalidConnection`, `EndpointNotAllowed`, `EndpointMetadata`; 404 `NotFound`; 409
`SignInNotConfigured` (no `bots.token_key`). Each change is audited (`app.connection.add`, `.change`, `.delete`).

**The flow.** A bot's sign-in prompt (the Teams SDK's `signin`, botbuilder's `OAuthPrompt`) asks for the person's
token; with none, it asks for a link and posts an OAuth card with it. The person opens the link in a browser, signs in
at the provider (the authorization code grant with PKCE, through the address guard), and the server's page shows them
a six-digit code. Their client sends the code to the bot as `signin/verifyState`, and the bot redeems it for the
token. The link works for 15 minutes; the code works once, for 15 minutes from when the page showed it, only for
the person the link was made for; five wrong codes end the sign-in. A token within a minute of its end is renewed
with its refresh token when the bot asks for it; one the provider refuses to renew (`invalid_grant`, or no refresh
token) signs the person out of that connection; any other failure keeps it for the next request.

| Request (the bot's) | Answers |
|---|---|
| `GET api/usertoken/GetToken?userId&connectionName&channelId[&code]` | 200 `{"channelId", "connectionName", "token", "expiration"}`, or 404 with none |
| `GET api/botsignin/GetSignInResource?state` | 200 `{"signInLink"}`; `state` is the SDK's base64 JSON (`connectionName`, the conversation reference's `user.id`, `msAppId`, in either letter case). 400 when it names another app, no connection of the bot's, or no one who may use the bot |
| `GET api/botsignin/GetSignInUrl?state` | the same link as text |
| `GET api/usertoken/GetTokenStatus?userId&channelId[&include]` | 200 `[{"channelId", "connectionName", "hasToken", "serviceProviderDisplayName"}]` |
| `DELETE api/usertoken/SignOut?userId[&connectionName]&channelId` | 200: the person's tokens and sign-ins of that connection, or of all |
| `POST api/usertoken/GetAadTokens` | 200 `{}` |
| `POST api/usertoken/exchange` | 412: single sign-on isn't offered |

Single sign-on is not built: no token of a person's account ever reaches a bot. A client's `signin/tokenExchange`
invoke is answered 412 and never sent, which makes the SDKs fall back to their card. The tokens, the PKCE
verifiers and the client secrets are sealed in the database (`bot_connections`, `bot_sign_ins`,
`bot_user_tokens`); a code is kept only as its digest.

### The keys a bot checks activities with

Each activity the server sends to a bot (signed mode) carries an RS256 JWT with exactly `iss` (`bots.issuer`), `aud`
(the bot's id), `serviceurl` (the activity's `serviceUrl`, byte for byte), `iat`, `nbf` and `exp`: no `ver`, `appid`
or `azp`, which would make an SDK take it for an emulator's or a skill's token. The keys are RSA-2048 (the server's
EdDSA keys can't be used: no SDK accepts them), published where the SDKs look:

| Route | For |
|---|---|
| `/bot/v1/.well-known/openidconfiguration` (`issuer`, `jwks_uri`, `id_token_signing_alg_values_supported`) | botbuilder, the Agents and Teams .NET SDKs |
| `/bot/v1/.well-known/keys` (the JWKS) | its `jwks_uri`; the Teams SDK for TypeScript and Python, which rewrite `openidconfiguration` to `keys` |
| `/<tenant>/discovery/v2.0/keys`, `/adfs/discovery/keys` (the same JWKS) | the Agents SDK for JavaScript, which computes this path |

Each key carries `endorsements: ["msteams"]` (botbuilder takes no key without its channel's endorsement). The keys
are rows of `bot_signing_keys`, so every node signs with and publishes the same ones; the JWKS is read from the
database on each request, so a key one node makes is published by all at once. A new key is published every
`bots.key_rotation` and replaces the signer after it was published for 25 hours (or one `bots.key_rotation`, if that
is shorter), so each key signs for one `bots.key_rotation` (the first also for those 25 hours): an SDK keeps the JWKS
for a day and fetches it again for an unknown `kid` at most once an hour. The key it replaces
is then retired, and stays published until no token it signed can still be used (its lifetime, the SDKs' five
minutes of skew and three housekeeping rounds), then goes. A node that serves the API (`serve --role all` or `api`)
makes the first key and rotates these keys, at its start and at its housekeeping; a `media` node does neither.

With `apps.enabled = false` none of these routes is served.

### What each SDK needs

From the container tests (`binary::bots::containers`, `tests/bots/`: each SDK's echo-and-cards bot against the real
binary over TLS) and the QA test of task 229 (`binary::bots::sdk`). A signed bot needs the server's origin over
`https` (every MSAL refuses another authority).

| SDK | Checks the server's activity token | Gets its own token | Seen |
|---|---|---|---|
| Teams SDK v2, TypeScript (`@microsoft/teams.apps` 2.2.1) | `cloud: withOverrides(PUBLIC, {openIdMetadataUrl: "<origin>/bot/v1/.well-known/openidconfiguration", tokenIssuer: "<origin>/bot", loginEndpoint})`, with a `loginEndpoint` the issuer doesn't start with (the SDK takes an issuer under it for an Entra one), `clientId`, `tenantId` | code: the SDK's msal-node takes no known authorities, so a `token` callback with msal-node, authority `<origin>/<tenant>` and `knownAuthorities: ["<origin host>"]`; `serviceUrl: "<origin>/bot/"` for a proactive send | text, Markdown, a card, edit, delete, typing, a member, proactive, the test activity; its OAuth flow (`oauthFlows`, `tokenServiceUrl: "<origin>"`) signs a person in with the card and `signin/verifyState`, and out |
| Microsoft 365 Agents SDK, JavaScript (`@microsoft/agents-hosting` 1.10.1) | by configuration: `authorityEndpoint: "<origin>"`, `tenantId` (its keys at `<origin>/<tenant>/discovery/v2.0/keys`), `issuers: ["<origin>/bot"]`, `clientId` | code: its msal-node asks Microsoft about any authority it doesn't know, and the adapter takes no token provider, so one of a few lines (msal-node as above, scope `<bots.scope>/.default`) set as the adapter's `connectionManager.getTokenProvider` and `getTokenProviderFromActivity` | the same |
| Bot Framework SDK, Python (botbuilder 4) | `ToBotFromChannelTokenIssuer`, `ToBotFromChannelOpenIdMetadataUrl` (upper snake case) | by configuration: msal-python takes an `<origin>/adfs` authority without asking Microsoft | tokens, the SDK's own check of an activity's token, a wrong secret, rotation |
| msal-node alone | | `knownAuthorities: ["<origin host>"]`, authority `<origin>/<tenant>` or `<origin>/botframework.com` | tokens, refusals, the hold |
| botbuilder-dotnet, botbuilder-js, Teams SDK Python and .NET, Agents SDK Python and .NET | | | not run yet: the design's expectations stand (botbuilder-dotnet by configuration; Agents SDK Python only unsigned) |

Every SDK has its own unauthenticated mode (an empty app id, or its "allow unauthenticated" switch), which an
unsigned bot uses with no code; the container tests run the signed mode.

## Outgoing webhooks

A team's outgoing webhook works as in Teams ([`docs/protocol/bots.md`](protocol/bots.md#outgoing-webhooks)): a
message in one of the team's standard channels that @mentions it goes to its callback URL, and the answer is the
webhook's reply in the post's thread. It needs no app registration and no administrator.

**Registration** is on the middle tier, `beta/teams/<team>/outgoingWebhooks` (the routes are in the protocol note).
The team's owners make, change and remove its webhooks; so do its members while the team's settings let them add
custom apps (`uploadCustomApp`), and the server's administrators for any team. The team's people list them and read
their pictures, and only those who may change a webhook see its callback URL. A name is one per team in any letter
case. The callback URL must be one the server sends to (the address guard of [bot delivery](#bot-chats-and-delivery),
with `bots.allow_http_endpoints` and `bots.allowed_networks`); an address written in the URL is checked at once, a
name when the server sends. Each webhook is an app account (`users.kind` `app`, `28:<id>`, which signs in nowhere and
is in no chat). Its token is 32 random bytes, shown in base64 once, in the answer that made it. The server keeps it
in the database as it keeps its other signing keys, because it signs with it. A removed webhook's account stays,
deleted, for the replies it posted. While `outgoing_webhooks.enabled`, authz names `outgoingWebhookService` (the
middle tier's base), so the client shows webhooks for the account.

**A mention** in a standard channel of the webhook's team (a post, or a reply in a post's thread) by a person makes
a row of the outbox (`outgoing_webhook_deliveries`) in the message's own transaction. A chat, a private channel, a
mention of another team's webhook, an edit or an app's message makes none. The activity is Teams'
(`conversation.id` `<channel>;messageid=<post>`, `text` with `<at>name</at>`, `from` `29:<id>`, `recipient` null,
`channelData` with the team, the channel and the tenant); no token is in it, and it carries no credential to call the
server back.

**Delivery.** Every API node sends rows. A node takes a row from `pending` to `sending` with a compare-and-set update,
so one node sends it, and posts it once, signed with `Authorization: HMAC <base64 HMAC-SHA256 of the exact body>`,
through the address guard, with `outgoing_webhooks.timeout_ms` for the whole exchange. A post wakes every node's
sender through the fan-out (`webhooks:wake`); a one-second poll covers a lost wake-up. The node that still holds the
row commits its terminal state and replies in one transaction. Replies use the webhook's current name in the post's thread:

- a 2xx answer with an activity: its text (Markdown, plain or XML, as a bot's) and its cards (Adaptive, hero,
  thumbnail), each card's actions cut to those that open a link, as Teams allows a webhook;
- a 2xx answer with an empty body, or an activity with no text and no card: no reply;
- anything else (another status, no answer in time, a refused address, a body that is not an activity, an attachment
  the client does not draw): Teams' reply "Sorry, there was a problem encountered with your request".

If a reply insert fails, the transaction rolls back all replies and the terminal state. The sweep then posts the apology.
A row is never sent twice. When a node dies while it sends, its row stays `sending` until its lease
(`outgoing_webhooks.timeout_ms` + 10 s) runs out; then any node's sweep fails it with the apology and sends nothing.
A row that waited more than a minute fails unsent, with no reply. A planned stop waits for the requests under way and
their replies. Done and failed rows go after a day.

With `outgoing_webhooks.enabled = false` none of this is served: no routes, no sends, no `outgoingWebhookService`. A
mention of a webhook made before is kept but not sent: when the webhooks are on again, it fails unsent and silent.

## Prefix table

Every service lives under the public origin with a fixed prefix. `Served` means the server answers it now, and authz
hands out its GTM where it has one.

| Service | GTM | Path under the origin | Served |
|---|---|---|---|
| Token endpoint | (the authority) | `/oauth2/v2.0/token` | yes |
| authz | `authService` (base `/api/authsvc`) | `/api/authsvc/v1.0/authz` | yes |
| Admin API, first-admin setup | (the origin) | `/api/admin/v1/` | yes |
| The account's own endpoints | (the origin) | `/api/account/v1/` | yes |
| Chat service | `chatServiceAfd` | `/api/chatsvc/<region>/` | yes |
| Middle tier | `middleTier` | `/api/mt/part/<partition>/` (the partition as `<letters>-<digits>`: `own-01`) | the team and calendar routes, `users/fetchShortProfile` |
| Chat service aggregator | `chatSvcAggAfd` | `/api/csa/<region>/` | yes |
| Provisioning | `teamsAndChannelsProvisioningService` | `/fabric/<region>/templates/api/` (the client takes a region of letters only) | yes |
| Presence | `presenceUPS` | `/ups/<geo>/` (the region's lowercase letters) | yes |
| Registrar | (the origin) | `/registrar/prod/V2/` | yes |
| Trouter | (the origin) | `wss://<origin>/trouter/v4/c` | yes |
| AMS and link previews | `amsV2` | `/ams/` | yes |
| Graph | (the origin) | `/graph/v1.0/` | `me`, `me/drive`, `users/<id>`, `users/<id>/photos/96x96/$value`, a shared file's item and thumbnails |
| User profiles | `userProfileService` | `/api/userprofilesvc/<region>/` | a team's people search |
| Files | (the origin) | `/sp/` | yes |
| Loki | (the origin) | `/loki/` | yes |
| Search | (the origin) | `/search/` | the title bar's suggestions and results ([Search](#search)) |
| Flight proxy | (the origin) | `/flightproxy/api/v2/` | 1:1 calls |
| TURN credentials | (the origin) | `/trap/tokens` | yes |
| Power Automate (Flow API), webhooks | `workflowsService` announces it; requests go to the client's fixed base | `/workflows/` | the webhook workflows' subset, while `workflows.enabled` |
| Power Apps connections | `workflowsService` announces it; requests go to the client's fixed base | `/workflow-connections/` | the account's Teams connections, while `workflows.enabled` |
| A bot's token endpoint and metadata | (the bot's authority) | `/<tenant>/oauth2/v2.0/token`, `/<tenant>/v2.0/.well-known/`, `/adfs/` | while `apps.enabled` |
| The keys bots check activities with | (the bot's settings) | `/bot/v1/.well-known/`, `/<tenant>/discovery/v2.0/keys` | while `apps.enabled` |
| A bot's token service | (the bot's `OAuthUrl`) | `/api/usertoken/`, `/api/botsignin/`; the browser's `/api/oauth/signin`, `/api/oauth/callback` | while `apps.enabled` |
| A team's outgoing webhooks | `outgoingWebhookService` (the middle tier's base) | `/api/mt/part/<partition>/beta/teams/<team>/outgoingWebhooks` | while `outgoing_webhooks.enabled` |

## Optional services

The client hides what an own server doesn't serve, rather than offering it and failing (`teams_core::Capabilities`,
read from authz's GTMs when the account starts; the app asks the server's authz again at each start, so a change to
the server's services shows after a restart; a server that takes no connection in 5 seconds is not waited on, and the
account starts on the region it has). Each optional service has a GTM that announces it:

| Service | GTM | Served | What the client hides without it |
|---|---|---|---|
| Workflows | `workflowsService` | while `workflows.enabled` | the rail's More apps, a channel's and a message's Workflows entries, the Workflows app |
| Copilot | `copilotService` | no | the Workflows dialog's AI Workflows tab and its Copilot check |
| A meeting's recap | `mcpService` | no | a meeting chat's Recap tab |
| Live captions | `captionsService` | no | a call's Language and speech, the captions key and command, the saved captions |
| Recording and transcription | `recordingService` | no | a call's Record and transcribe |
| The app catalogue and bots | `appsService` | while `apps.enabled` | a team's Apps tab, Get more apps, a copied team's Apps, a bot card's buttons that send to the bot |
| A team's outgoing webhooks | `outgoingWebhookService` | while `outgoing_webhooks.enabled` | Manage team's "Create an outgoing webhook" and the team's webhooks (client task 412) |

`docs/protocol/endpoints.md` names for every request the client sends whether the server serves it, the client hides
it, it goes to a public host, or it is a known gap (`own server: …`); `cargo test -p ngteams-server endpoints` checks
that each row's note starts with one (`served`, `hidden (<capability>)`, `public host`, `not served (<why>)`), and
that each hidden one is a service that a server with its option off leaves off. It does not send the served rows to
the server: the tests of each service do.

## Push: the registrar and Trouter

The wire is Teams' (`docs/protocol/trouter.md`): socket.io 0.9 over a WebSocket at `/trouter/v4/c`, with the
client's query (`tc`, `timeout`, `epid`). The handshake carries no credential; one with another `Origin` than the
public origin is refused.

- **Authentication.** The server sends `1::`. The first frame must be `user.authenticate` with the account's `ic3`
  token within `trouter.auth_timeout`, else the socket gets `0:::{"reason":"timeout"}`. A token that isn't valid, or
  a disabled account, gets `0:::{"reason":"unauthorized"}` and no `trouter.connected`. A token is run out at its
  `exp`, with no clock skew, at the connect as on an open socket: when it runs out the socket gets `unauthorized`,
  and the client comes back with a new token (it normally reconnects before that).
- **An account's end.** `user disable`, `user delete` and `user reset-password`, the admin API's same changes and
  an account's own password change close the account's open sockets at once with `unauthorized`, on every node:
  each publishes the disconnect on Redis, after its database change, and warns when it can't (the change stays, and
  the read below ends the sockets). Each socket also reads its account
  from the database once it listens, before each delivery and, on a node without Redis (which the command can't
  reach), every second, also while a replay to a slow reader is under way; it ends when the account is gone,
  disabled or has another password than its token was issued under. An access token names its account's password
  version (`pwd_ver`, a count of the resets), so a token from before a reset opens no socket and no picture, file
  or preview route either, though it is good elsewhere until it runs out. So nothing posted after the command returned is pushed to the account. The
  client then asks for a new token, which it doesn't get, and is signed out.
- **Endpoint.** `trouter.connected` names the endpoint, its push URL (`<origin>/trouter/v4/f/<endpoint>/`), the
  `reconnectUrl` on the same origin and `connectparams` valid for seven days. The endpoint id is derived from the
  account, the client's `epid` and its stack, so the same client gets the same endpoint on every connect and on
  every node, and `connectparams` carry no secret. Of an endpoint's sockets the one that claimed it last stays: a
  socket announces its claim with its time once it listens, an older socket closes, and a newer one that hears an
  older claim announces its own again. An account holds at most 32 sockets on a node.
- **Registrar.** `POST /registrar/prod/V2/registrations` with the `ic3` token takes either stack's registration
  (`TeamsCDLWebWorker`, `SkypeSpacesWeb`) and answers `202`. The push URL must be the caller's own endpoint for that
  `registrationId` and stack (else `403`). The TTL is at most 3600 seconds. A registration is a row in
  `push_registrations`; an account holds at most 64 live ones (`429` beyond). The account's sockets are told at once.
  A registration ends with its TTL. Nothing removes it sooner: Teams' removal at a sign-out is known only from the
  bundle (no capture shows its request) and the client sends none, and the socket's goodbye (`0::`) is the same on
  a stop that Teams leaves the registration through. A registration without a socket gets no delivery.
- **Deliveries.** A messaging endpoint with a live registration gets the account's `push:<mri>` events as `3:::`
  deliveries at `/trouter/v4/f/<endpoint>/messaging`, each body an `EventMessage` around the fan-out's resource:
  `NewMessage`, `MessageUpdate`, `ConversationUpdate`, `CustomUserProperties` and `ThreadUpdate` (a new chat, a chat's
  people, a team's or a channel's change; the client reads its teams again for a team's or a channel's thread and a
  stored group chat's people for a chat, [trouter.md](protocol/trouter.md)). A channel's messages come as a chat's do,
  to the people in the channel's thread: a team's people for a standard channel, its own for a private one, and nobody
  once they are out of it. A reply's `to` and links name its post's chain (`<channel>;messageid=<post>`); a channel's
  message names no `threadtype`. `ReadReceipt` stays internal: Teams delivers no peer read receipt. The calling
  stack's socket gets its endpoint's call deliveries ([Calls](#calls)) and no chat events. The client's answers
  (`3:::` with a status) are accepted; a calling client's answer to an acceptance carries its later callbacks, which
  the call controller keeps.
- **Keep-alives.** `ping` is answered `["pong"]`, any other acknowledged event `[]`. A socket silent for two ping
  intervals and 20 seconds is closed.
- **Replay.** Every delivered event but typing (and other `Control/…` messages) is also kept in the account's
  journal for 15 minutes, at most 512 entries: a Redis Stream `journal:push:<mri>` (in the deployment's
  [keyspace](#scaling)), or memory on a single node. Each
  registered endpoint keeps a cursor, the last entry written to its socket, for 24 hours: a field of the account's
  hash `cursors:push:<mri>`, which holds the 64 cursors that moved last. A socket delivers from its endpoint's
  cursor, so after a reconnect, on any node, the endpoint gets what it missed as stored deliveries (`"mstore":true`,
  the same `id` as the live one would have had), in order, each written to a socket once. The cursor is kept after
  each batch and when the socket ends; only a node that dies mid-batch replays that batch. A new endpoint starts at
  the journal's end.
- **Message loss.** The socket gets `trouter.message_loss` with the `messaging` tag, and the client runs its
  conversation sync and acknowledges, when entries after the cursor may be gone:
  - the journal overflowed past the cursor;
  - the endpoint was away, or unregistered while an event came, and its cursor is older than 15 minutes with
    nothing left to prove otherwise (a socket that followed its account is not told after a quiet spell);
  - a reconnecting endpoint has no cursor;
  - the journal couldn't keep an event (Redis refused it): the account's sockets are told on the control topic;
  - the socket missed control words.
- **A broken Redis subscription.** What is published while a node's Redis subscription is down reaches none of its
  sockets, and nothing says what it was. So the break closes every socket of the node with `0::`. Each client
  reconnects after its backoff, is replayed what the journal kept from its cursor, and syncs its conversations as
  on any connect, which covers an event that reached neither the journal nor the control topic.
- **A slow reader.** A replay ends, like the socket, when the token runs out, the node stops or the control topic
  says so, however slowly the client reads.
- **Shutdown.** A node closes its sockets with `0::` when it stops; clients reconnect to another node after their
  backoff. `ngteams_server::trouter::disconnect` closes an account's sockets on every node.

## Storage

Every stored blob (a pasted picture, a sent file, a file's thumbnails) goes through `object_store`. Only its own
S3 calls are used, so any S3-compatible service works by configuration alone. Several nodes share one bucket. The
database says who may read a blob; the bucket needs no public access and clients never reach it.

AWS S3:

```toml
storage_url = "s3://my-bucket/ngteams"

[storage_options]
aws_region = "eu-central-1"
aws_access_key_id = "…"
aws_secret_access_key = "…"
# aws_session_token = "…"        # temporary credentials
```

Any other S3-compatible service (RustFS, MinIO, Ceph, Garage, R2):

```toml
storage_url = "s3://my-bucket/ngteams"

[storage_options]
aws_endpoint = "https://s3.example.com"
aws_region = "us-east-1"                      # the name the service signs requests with
aws_access_key_id = "…"
aws_secret_access_key = "…"
aws_virtual_hosted_style_request = "false"    # path-style addressing; "true" for <bucket>.<host>
# aws_allow_http = "true"                     # only for a plain-HTTP endpoint on a trusted network
```

The option names are `object_store`'s. In the environment they are `NGTEAMS_SERVER_STORAGE_OPTIONS__AWS_REGION`
and so on. Without keys, `object_store` takes the instance's or the container's role credentials. The bucket must
exist. Uploads go as multipart uploads, so the store's rule for unfinished multipart uploads decides how long an
aborted one stays.

A store out of reach answers the client 503 within seconds: a node waits `storage.connect_timeout` for a
connection and sends a request again at most `storage.max_retries` times, none later than `storage.retry_timeout`
after the first attempt. `storage.request_timeout` is the wait for the store's answer to one request (one picture,
or one 5 MiB part of a file, sent whole first) and the longest a download may then stand still, the store sending
nothing or the client taking nothing. A file of any size is sent in parts and a download has no total time, so a
slow client doesn't count against it; raise it only when the link between the server and its store is slow.
`read_timeout` and `connect_timeout` in `storage_options` take the place of the two timeouts, and `timeout` there
limits the whole of each request, a download too: none is set by default. These keys are for S3 stores only.

Keys in the bucket: `ams/<object>/<version>` (pictures: each upload of an object's bytes gets a key of its own, and
the database names the one the object reads from), `drive/<item>` (files), `drive-thumbnails/<item>/…` (thumbnails),
`profile-pictures/<account>/<upload>` (an account's picture), and `outgoing-webhooks/<webhook>/pictures/<upload>`
(a webhook's picture, with its etag as the upload name). Each upload of a webhook's picture gets its own key.
Old webhook pictures use `outgoing-webhooks/<webhook>/picture` until a manager replaces or removes them.

## Pictures, files and link previews

### Pictures in messages (AMS, `/ams/`)

Requests carry the chat service's token (`https://ic3.teams.office.com`).

| Request | Does | Answers |
|---|---|---|
| `POST /ams/v1/objects/` | makes an object for a pasted picture: `{"type": "pish/image", "permissions": {"<chat>": ["read"]}, "filename"}`. The caller must be a member of every chat named | 201 `{"id"}`; 403 for a chat the caller isn't in |
| `PUT /ams/v1/objects/<id>/content/imgpsh` | stores the picture's bytes. Only the object's maker; only PNG, JPEG, GIF or WebP, by their signature | 201; 403; 413 over `media.max_picture_bytes`; 415; 507 over the quota |
| `GET /ams/v1/objects/<id>/views/<view>` | the picture (`imgo`, `imgpsh_fullsize`, `imgpsh_fullsize_anim`: all the bytes as sent) | 200; 403; 404 while the object is empty |
| `GET /ams/v1/objects/<id>` | the object's description, in Teams' eight keys | 200; 403 |
| `DELETE /ams/v1/objects/<id>` | deletes the picture: its bytes leave the store and its maker's quota. Only the object's maker. Not seen in a capture of Teams: the route is this server's own | 204; 403; 404 |

An object is read by its maker and by whoever is a member of one of its chats when the read arrives: someone who
left the chat no longer reads it. `forward` (sharing an object with another chat) and other object types are not
served.

### Files sent in chats (`/sp/`, `/graph/v1.0/`)

The client sends a file the way it sends one to OneDrive, and the server keeps one drive per account:

1. `GET /graph/v1.0/me/drive` names the drive at `<origin>/sp/personal/<account id without dashes>/`.
2. Under `/sp/personal/<account>/_api/v2.0/`, with a token whose audience is the server's own origin (the token
   endpoint mints it beside the configured audiences), and only for the drive's owner:
   - `GET drive`: the drive and its owner.
   - `GET drive/root:/Microsoft Teams Chat Files` (404 until the first upload) and its `:/permissions`, or
     `drive/root/permissions`: the owner's grant alone.
   - `PUT drive/root:/Microsoft Teams Chat Files/<name>:/content`: stores the body, streamed to the store, never
     held whole. 201 with the item (`id`, `name`, `size`, `webUrl` and `webDavUrl` (both the file's path),
     `parentReference.driveId`, `sharepointIds.listItemUniqueId`). A name the drive already has gets a number (`notes 1.txt`). 413 over
     `media.max_file_bytes`, 507 over the quota. A new file is readable by its owner alone.
   - `POST sites/root/items/<item>/driveItem/invite`: shares the file with named accounts (`recipients` by
     `email`, which is the UPN). Only the direct form the client sends is taken: `requireSignIn: true` and
     `sendInvitation: false`. There are no links anyone could open. 400 for an address that is no enabled account.
   - `GET sites/root/items/<item>/driveItem/permissions`: who the file is shared with: the owner
     (`roles: ["owner"]`) and each other account (`["write"]`).
   - `DELETE sites/root/items/<item>/driveItem/permissions/<account id>`: ends one account's access.
   - `GET drive/root:/Microsoft Teams Chat Files:/children`: the drive's files (`value`: the items).
   - `DELETE drive/items/<item>` (the item's `id` or its `sharepointIds.listItemUniqueId`): deletes the file, as
     Graph and SharePoint delete a drive item. 204; 403 on another account's drive; 404 for a file the drive
     doesn't have. Its bytes and thumbnails leave the store and the owner's quota, and its shares and links end.
3. A reader reaches the file through Graph, by the sharing link the message carries (the file's `webUrl`):
   `GET /graph/v1.0/shares/u!<link, base64url>/driveItem` (the item with `@microsoft.graph.downloadUrl`) and
   `…/driveItem/thumbnails` (Graph's `small`, `medium` and `large`, 96, 176 and 800 pixels). 403 for an account
   the file isn't shared with.
4. **A file shared on** ("Share this file", `docs/protocol/chat.md`, "Files: shared on"): the client posts the file's
   entry again in another chat or channel. When a post names a file of this server and its sender has access to
   the file, the server gives each reader of that chat or channel access too (the readers the post's push goes to),
   as an anyone link reaches them on Microsoft's service. A post by someone without access gives nobody access.
   The message and its file grants commit in one transaction. A grant error rolls both back. A post checks at most
   ten file entries. A link with userinfo or a different origin gives no access.
   Anyone with access reads what the link lets people do:
   `GET /sp/personal/<owner>/_api/v2.0/shares/u!<link, base64url>/permission` answers the link with `scope`
   `users`, `type` `edit` and the words "People you choose can edit". 403 for an account without access, 404 for a
   link to another drive.

Download and thumbnail links are `<origin>/sp/download/<item>?tempauth=…` and
`<origin>/sp/thumbnail/<item>/<size>?tempauth=…`. The `tempauth` is a JWT the server signs for that one item and
purpose, valid for `media.link_lifetime` seconds. Any node checks it with the shared keys. It names the account
it was handed to and that account's password version, and each use reads the account: a link of an account that
is disabled, deleted or has had its password reset since answers 401, and a link to a file no longer shared with
that account answers 403. The link takes no
token of the account, and the server streams the bytes from the store, so the store's credentials and the
account's tokens never meet. S3's own presigned URLs aren't handed out: the client takes a file link only on the
server's origin and follows no redirect.

A download serves `Range` (one range: 206 with `Content-Range`; an unsatisfiable one: 416; several ranges: the
whole file). Files always download as attachments (`Content-Disposition`, `nosniff`), so nothing uploaded is
rendered on the server's origin.

Thumbnails are made the first time they are asked for, from PNG, JPEG, GIF and WebP files up to 32 MiB, as JPEGs,
and kept in the store. Other files have none (`value: []`), and the client draws its placeholder.

The file's `webUrl` itself answers nothing: there is no web page to open a file in.

Sharing needs two people reads: `POST <middle tier>/beta/users/fetchShortProfile` (MRIs to names and addresses)
and `GET /graph/v1.0/users/<id>` ([People](#people)). Authz hands out `middleTier` and `amsV2`, so a client signed
in through authz pastes pictures and sends files.

### Files in a channel (`/sp/sites/`)

The client puts a file for a channel post or reply into the channel's folder on its team's site, as Teams on the
web does (`docs/protocol/channels.md`, "Files in a channel"). Each team has one site, named by the team's group id,
and each channel has one folder in the site's `Shared Documents`:

1. The channel's thread (`GET v1/threads/<channel>?view=msnp24Equivalent`) names the folder in its `properties`:
   `sharepointSiteUrl` (`<origin>/sp/sites/<group id>`), `sharepointRootLibrary` (`Shared Documents`),
   `sharepointChannelDocsFolder` and `channelDocsFolderRelativeUrl` (`/sp/sites/<group id>/Shared
   Documents/<folder>`). The general channel's folder is `General`. Another channel's folder has the channel's
   name, or `Channel <the channel id's letters and digits>` when the name has a character that a folder can't have.
   A file keeps the folder that it got: when a channel gets a new name, new files go to the new folder.
2. Under `/sp/sites/<group id>/_api/web/`, with a token whose audience is the server's own origin, only for an
   account on the team (else 403; a site that no team has: 404). A step on a file also needs the account in the
   file's channel, so the files of a private channel stay with its members:
   - `POST GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)?@v='<folder path>'&@fn='<name>'`: an empty file
     (`OData` strings in quotes, a quote doubled, percent-encoded). 200 with the file (`SP.File`: `UniqueId`,
     `Name`, `ServerRelativeUrl`, `Length` `"0"`). The folder must be a channel's folder of that site (else 404),
     the channel must not be archived (else 403), and the name must be one that SharePoint holds (else 400). When
     the folder has the name, 400 with SharePoint's `odata.error` (`-2130575257, Microsoft.SharePoint.SPException`,
     "A file with the name … already exists."). The client then tries `<name> 1.<extension>`.
   - `POST GetFileById('<id>')/StartUpload(uploadId=guid'<guid>')`: the file's bytes, streamed to the store, never
     held whole. 200 with `{"value":"<bytes taken>"}`. Only the account that added the empty file fills it (else
     403), and only once (else 400). 413 over `media.max_file_bytes`, 507 over the quota. The bytes count against
     that account, as the bytes of a chat file do.
   - `POST GetFileById('<id>')/FinishUpload(uploadId=guid'<guid>',fileOffset=<length>)`: 200 with the file and its
     `Length`. The upload id and the offset must be those of the `StartUpload` (else 400).
   - `GET GetFileById('<id>')`: the file. When `StartUpload` or `FinishUpload` names `@folder` or `@file`, they must
     be the file's (else 400).
   - `DELETE GetFileById('<id>')/recycle`: 200. Any account in the channel can do it, as a member of a team's site
     can. The file is gone at once: its bytes and thumbnails leave the store and the count of the account that
     added it. There is no recycle bin to restore it from.
3. The post names the file by its address, `<origin>/sp/sites/<group id>/Shared Documents/<folder>/<name>`
   (`objectUrl`, `fileInfo.fileUrl`). Each account in the channel reads the file through Graph by that address, as a
   chat file is read by its sharing link: `GET /graph/v1.0/shares/u!<address, base64url>/driveItem` and
   `…/driveItem/thumbnails`, and the signed download and thumbnail links. 403 for an account that is not in the
   channel. The links stop when the account leaves the channel.
4. Nothing is shared or granted. A post that names a channel file in another chat or channel gives nobody access
   to it: only the accounts in the file's channel read it.

The operator's `ngteams-server media list|delete` shows and deletes the channel files that an account added
(`channel-file`).

### Freeing storage

An account at `media.user_quota_bytes` gets 507 for each new picture or file until some of what it stored is
deleted:

- **A picture** is deleted with the last message that shows it: when its sender deletes a message, each picture
  of the sender's that the message showed (`amsreferences`), and that no other message in its chats still names,
  is deleted. `DELETE /ams/v1/objects/<id>` deletes one whatever shows it. A picture that a person removes by
  an edit, or pastes and never sends, stays until its object is deleted. A bot edit releases the pictures it
  removes, as a delete does ([Apps and bots](#apps-and-bots)).
- **A channel file** stays in its channel's folder when its post is deleted. `DELETE …/GetFileById('<id>')/recycle`
  deletes it.
- **A file** stays in its sender's drive when its message is deleted, as a file stays in the sender's OneDrive in
  Teams. Its owner deletes it with `DELETE drive/items/<item>`. ngteams has no command for that yet, so on a
  server of ngteams clients the operator frees files with `ngteams-server media list|delete`.

Each delete takes the bytes off the account's count in the transaction that removes the row, so uploads and
deletes at once leave the count right, on several nodes too. The bytes are cleared from the store after that; if
the store fails then, the node logs their key and the bytes stay in the bucket, read by nothing. A node that
stops waits up to 10 seconds for the clears still running, and logs the keys it leaves.

### Link previews (`/ams/urlp/v1/url/`)

- `GET info?url=<page>` answers `{"url", "status", "title", "favicon", "preview": {"title", "description",
  "previewurl"}}`, from the page's Open Graph tags, else its `<title>` and description. `preview` is the object a
  message's `properties.links` entry carries. Teams' own answer was never captured, so this shape is ours.
- `GET image/Thumbnail?url=<picture>` answers the picture as a JPEG at most 640 pixels on its longer side. This is
  the read the client makes for a message's `previewurl`, so a reader's client never reaches the page's site.

The account chooses the address, so the server guards every fetch against request forgery:

- only `http` and `https` on their default ports, no user info;
- every address of the host must be public: loopback, private, link-local (the cloud metadata address too),
  shared, reserved, multicast and documentation ranges are refused, in IPv4, IPv6 and the IPv6 forms that carry an
  IPv4 address. A refused address gets 403 before anything is sent;
- the connection is made through a resolver that hands out public addresses only, so a name can't change its
  answer between the check and the connection;
- at most three redirects, each checked the same way; no proxy;
- `media.preview_timeout` for the whole exchange, size caps for the page and the picture, and at most 16 fetches
  per node at once (503 with `Retry-After` beyond).

`media.link_previews = false` removes both routes.

## Calls

The server does a call's signaling and hands out the TURN relay's credentials. A 1:1 call's media goes between the
two clients through the relay (their ICE is relay-only): the caller's offer travels in the callee's invitation, the
callee's answer in the caller's acceptance; `ngteams` takes an own-server account's relay from the server
([below](#the-turn-relay-coturn)). The wire is Teams' (`docs/protocol/calling.md`), every link under
`<origin>/flightproxy/api/v2/ep/`; requests carry the account's skypetoken.

- **The start** (`POST /flightproxy/api/v2/epconv`) from one account to one other. The answer is the conversation
  with its links (`leave`, `updateEndpointState`, `updateEndpointMetadata`, `updateParticipantProperties`,
  `notificationLinks`). The callee's registered calling endpoints (the `SkypeSpaces` stack's Trouter sockets, on
  any node) get the invitation at their root: `{"evt": 107, "gp": <base64 JSON>}` with the caller, the leg's
  `attach`, `progress` and `reject` links and the caller's offer. An invitation an endpoint doesn't answer (its
  socket sends no response to the delivery) is delivered again, three times 2 s apart, as Teams' service does, by
  the node that holds that socket; any response stops it, and the client takes a redelivery as the invitation it
  has. A group chat's call is [below](#group-calls), a
  meeting's [below that](#meetings); the test call's start is answered `501`.
- **While it rings**: `attach` (answered with the leg's `progress`, `acceptance` and `callLeg` links and the
  conversation's), `progress`, `accept` (answered with the call leg's links and the keep-alive interval), `reject`.
  An answer delivers the acceptance, with the callee's answer, at the caller's `acceptance` callback; the callee's
  other endpoints get the call's end (`487`, "accepted by another fork"). A decline ends the call for the caller
  (`603`, the callee as its sender): there is no voicemail. A ring nobody answers in `calling.ring_timeout` ends at
  both ends (`487` "Call cancelled on timeout." at the callee, `480` at the caller); the caller's `leave` while it
  rings cancels it.
- **While it is up**: the call's keep-alive at the call leg's own link (`202`); `leave`, which ends the call for
  the other party; `updateEndpointState`, whose mute both rosters then list; a renegotiation (`renegotiate`),
  delivered at the far end's `mediaRenegotiation` callback with where its answer (`negotiations/…/answer`) or its
  rejection goes, the answer delivered back with where it is acknowledged (`mediaAnswers/…/acknowledge`), and the
  acknowledgement told to the answerer; a renegotiation while another waits for its answer is turned away with
  `491` at its `rejection` callback, and one unanswered for 30 seconds gives way to the next: what then comes for
  it at its own links is answered `410`. `updateMediaDescriptions` and `applyChannelParameters` are taken and change
  nothing. The caller's `mediaRenegotiation` callback is read from its answer to the acceptance's delivery (its
  `callAcceptanceAcknowledgement`), as Teams' call controller reads it.
- **Hold and resume**: `hold` is answered `202` with the `resume` link; both rosters gain Teams' music-on-hold
  app for as long as the hold lasts, which is how the held client knows, and the holder is told at its
  `holdCompletion` and `resumeCompletion` callbacks. No music plays and the media isn't moved: that takes the
  mixer.
- **Deliveries** go to the callbacks each party handed out (`…/trouter/v4/f/<endpoint>/callAgent/…`), gzip'd in
  base64 as Teams' call controller sends them. A request's callbacks are taken only when all are on one calling
  endpoint the account itself registered (`403` otherwise), so no request makes the server deliver to someone
  else's socket.
- **Who may do what**: the token's account must be one of the call's two parties; anyone else gets `404` for every
  link of the call. Only the callee attaches, answers and declines. A call controller operation on a call that is over gets
  `410`, except a `progress` (`200`, nothing changes): the client reads a refusal of it as a failed call, while the
  ring's end (`487`) is still on its way to it. The conversation's own (`leave`, the endpoint's state and metadata)
  are answered `200` and change nothing, as a write that was on its way when the far end hung up must not read as a
  failure.
- **The chat** of the two parties, when they have one, gets the call's `Event/Call` messages from the caller:
  its start, and its end listing who was in it and for how long. An end with the caller alone is a missed call
  (unanswered, declined or cancelled).

### Group calls

A start naming a group chat (`groupChat.threadId`) is a group call. Its media goes through a **media node**
(`src/mixer/`): every party's leg goes to the node, which sends each party the mix of everyone else. Signaling
reaches it through `calling::MediaNodes`: the node makes a leg per party (with the party's offer it answers;
without, it offers), takes a callee's answer and a renegotiation, and drops legs and rooms. Each operation asks the
node first (a leg's requests repeat safely by its id) and then makes the step that records what the node answered;
a join or an add first reserves its seat, so the source ids the node gets are the ones the roster names. A
reservation whose leg or step fails is undone and its leg dropped: the seat gets back the leg its ring was offered
on (also when several reservations of it overlap), and a seat made for it goes when no seat was made after it;
otherwise it stays, without a leg and in no roster, for that account's next try. With no media node to take a new
room (`--role api` without Redis or a live node, or every node full) the start is answered `503`.

**The media node** is str0m (ICE, DTLS, SRTP and the data channel, on one UDP socket, `mixer.udp_listen`)
with libopus: each party's audio is decoded into a buffer of its own (played once 60 ms are in, concealed for three
20 ms ticks when it runs dry, trimmed back past 200 ms), and every 20 ms each party gets the sum of every other
active party (above −50 dBFS within 200 ms), soft-clipped and encoded (32 kb/s, in-band FEC, DTX). A party is never
sent its own voice. The control plane on the data channel answers the client's `syn` with `ack` (it sends `dsh` and
`sr_res`, takes `sr`) and tells the dominant speakers (`dsh`, the audio source ids, the newest speaker first, at most
every 300 ms and to a party once its control plane is up). A party that stops speaking, or mutes, while another talks
on gives the first place to the newest party still speaking: the client shows the first one as speaking while it
hears anything. The node writes its SDP in Teams' dialect with its host
candidates (`mixer.public_addrs`); the client is relay-only, so its media reaches the node from the TURN relay's
address, and **coturn must be allowed to send to the node's address** (a `denied-peer-ip` covering it, or coturn's
default refusal of loopback peers, leaves group calls without media: add `allowed-peer-ip=<node address>`).

**Video** is forwarded, never transcoded. Every party's leg has the main video line (its camera), nine receiving
lines and the sharing line, each both ways and H.264 at `107` (retransmissions `99`); each line names its stream id
(`a=x-source-streamid`): the main line the party's video source, the sharing line its sharing source, the receiving
lines ids of their own from the party's block (a party's ids are 20 apart: audio, video, sharing, data, then its
receiving lines). The camera and the screen go on as the party describes its lines (`updateMediaDescriptions`: a line
other than the sharing line sending is the camera, the sharing line sending is the screen); the roster then says so
(`main-video` `sendrecv`, an `applicationsharing-video` stream `sendonly` with the sharing source; as in Teams, one
share at a time: a share that starts takes over the one going, whose stream the roster no longer lists, and that
sharer's client stops its share) and the others ask
the node for it with source requests on the control plane (`sr`: a receiving line's stream id and the source to show,
`-1` for none; each is answered `sr_res` `ok`, and one naming no receiving line of the party's changes nothing). The node writes each frame of a camera or a screen onto every line
whose last request named it and whose `max-fs` its pictures fit (the size read from the sequence parameter set of the
source's keyframes; nothing is scaled), from that source's next keyframe on (it asks the source for one by PLI, or FIR where
the line takes no PLI, at most every 300 ms per source), its RTP time going on from where the line was when it changes
source. A receiver's PLI or FIR on a line is passed to the source it shows. No video goes to a party before it
seeded its bandwidth (`applyChannelParameters`' `sendSideBWSeed`, in its start, answer, join or renegotiation, or on
its own), as Teams' mixer does.

**The internal API** (`mixer.internal_listen`; on a private network, never under the public origin) takes
`PUT|DELETE /internal/mixer/v1/rooms/<call>/legs/<leg>` (`{"sources", "offer"?}`, answered `{"answer"|"offer",
"epoch"}`), `POST …/legs/<leg>/answer` (`{"answer"}`), `POST …/legs/<leg>/offer` (`{"offer"}`, answered
`{"answer"}`), `POST …/legs/<leg>/parameters` (`{"seed"}`, the bandwidth seed in bits a second), `GET|DELETE
…/rooms/<call>`. Each request carries a bearer token the deployment's own keys signed for
`ngteams-media` (60 s; every node reads the same keys, so there is no secret to configure) and the node life its
call's row names (`ngteams-media-epoch`); another life's room is `410`. A renegotiation the same as the last is
answered the same; more than 20 in a minute are `429`.

**Failure**: a call's row names its node (`{node, url, epoch}`), so any signaling node reaches its room; with Redis,
every media node beats in the registry every `mixer.heartbeat` seconds. A call whose node is gone (no heartbeat for
`mixer.dead_after`, another life after a restart, or this process's node stopped) ends for everyone (`503` at each
party's `end`, the chat told); a running call is never moved to another node. A node's entry must stay missing from the
registry for two `mixer.heartbeat`s before its calls end, so a Redis that lost its data ends no call. A party whose leg
stays down for `mixer.media_timeout`, or doesn't connect within `mixer.connect_timeout` of its answer, is taken out of
the call (`408` at its `end`, the others get the roster); a leg still there 5 s after its report is reported again.
Every `mixer.reconcile` seconds the node looks at its rooms, and drops a room whose call is over or gone at two looks in
a row (a new call's room is made before its row is stored). A stopping `--role media` node takes no new room and serves
its rooms for `mixer.drain_timeout` at most; a stopping `--role all` node, its API already gone, stops its media node at
once.

- **The start**: the caller must be in the chat (`403`), and only the chat's members it names ring, once each; a
  chat that doesn't exist is `404`, and a meeting's chat or a channel is `400` (their call is a meeting's,
  [below](#meetings)). The caller is in the call at once: its acceptance, with the node's answer to its
  offer, comes at its `acceptance` callback, and its first roster at its `rosterUpdate`. Each callee's calling
  endpoints get `{"evt": 109, "gp": …}` with the node's offer to its own leg (`fromMixer`, `mediaLegId`,
  `conversationType: "groupCall"`, the chat in `groupChat`, `isMultiParty`). The chat gets `callStarted` naming the
  call's conversation (`<conversationUrl>`), which is how a member who wasn't rung finds it. A start in a chat whose
  call still runs is answered with a conversation that never runs and ended at once at its `conversationEnd`
  callback with `409` / `5704`, naming the running call (one call per chat is a unique index, so two starts can't
  both win).
- **Each party alone**: a callee's `accept` gives its answer to its leg and takes it into the call (its other
  endpoints stop ringing, `487`), unless a join of the same callee reserved its seat over that leg before the
  answer is stored (also before it reached the node): then the answer is `410` and the join takes it in; a decline
  or a ring that runs out (`calling.ring_timeout`, per callee) ends only that party's ring and drops its leg; a
  `leave` takes one party out (its roster entry `inactive`) and drops its leg, and the last one out ends the call:
  the rings still going end, the room is dropped, and the chat gets `callEnded` listing everyone who was in it. A
  leave from an endpoint whose answer lost changes nothing.
- **Joining and following**: a chat member's `POST conv/<id>` with a `callInvitation` and its offer joins the
  call: its acceptance, with the node's answer, comes at its own `acceptance` callback. Without one, a member follows
  the roster (the whole roster now, its changes after); a party that rings or is in the call hands out more
  callbacks. Anyone else (a former party no longer in the chat too) gets `404`.
- **Add** (`conv/<id>/add`, the conversation's `addParticipantAndModality` link): a party in the call rings a member
  of the chat with the node's offer to a new leg, invited by the adder (the caller added back after leaving rings
  and answers as anyone else); one who still rings rings on as it is. Each ring of a party has its own invitation
  links (`…/forked/<call>/<seat>/i1/<ring>/…`), as the client takes an invitation equal to an earlier one for its
  redelivery; an earlier ring's link is `410`. Someone with no calling endpoint isn't
  brought in, and the adder is told at its `addParticipantFailure` callback (`580` / `5205`); an added party that
  declines is told there too (`580` / `5208`, `callControllerTransactionEnd` `603`). An added party whose ring times
  out is not told there, because no capture shows what Teams sends then: the adder's client keeps "Calling…" until
  the party joins or the call ends. Someone not in the chat is `403`. In a meeting's call only a host adds (an attendee's add is `403`), and someone who waits in its
  lobby is let in with `admit`, not added (`403`).
- **Renegotiation** goes to the node, and its answer back to the party alone, at its `mediaAnswer` callback; the next
  one waits for that answer's acknowledgement (`491` at its `rejection` callback), for 30 s at most, as an
  acknowledgement may be lost and is never sent again. `updateMediaDescriptions` sets the party's camera and screen
  in the roster (one that describes nothing changes nothing); `applyChannelParameters` gives its bandwidth seed to its
  leg on the node. A group call has no hold (`403`, and no `hold` link).
- **The roster** lists each party in the call with its main audio, its main video (`sendrecv` while its camera is
  on, `recvonly` if not) and, while it shares its screen, its sharing stream, by the source ids signaling gave it
  (1000, 1020, … per party). Each change goes to every party in
  the call, every ringing party's ring subscription and every follower; a party that comes in gets the whole roster.
  Each calling endpoint of the others also gets `{"evt": 115 | 116, "cp": base64(gzip(JSON))}` when someone comes in
  or leaves; the client reads neither, and the shape is the protocol notes' minimum.
- **Raised hands and reactions**: `conv/<id>/publishState` of `raiseHands` puts the party's hand up (answered with
  its state id and its rank among the hands raised; the roster lists it under `publishedStates`), `removeState` of
  that id takes it down, and a party's hand goes when it leaves. `sendMessage`'s reactions go to everyone in the
  call, the sender too, at their `receiveMessage` callback, named by who sent them (`202`).
- **Seats**: a group call's controller links carry the party's seat where a 1:1 call's carry `1`; a link naming
  another party's seat is `404`.

### Meetings

A meeting's call is a group call in the meeting's chat that nobody rings (`src/calling/meeting.rs`). The meeting is
the calendar's ([above](#the-calendar-and-meetings)); its organizer is the meeting's, whoever started the call.

- **Joining**: the client's start with the meeting's chat and organizer (`meetingInfo`, `groupChat` the meeting's
  chat) starts the meeting's call when none runs and joins it when one does; a start naming the meeting by its id and
  passcode alone (`meetingData`, Join with an ID) does the same, and an id and passcode of no meeting are `404`. A
  plain group call start in a meeting's chat is `400`, and a running call in it that is no meeting's is not joined
  (`403`). Two
  first starts at once make one call (the group calls' unique index), the second joining it. The chat gets
  `callStarted` naming the call's conversation and, at the end, `callEnded`.
- **The start's push**: when a scheduled meeting's call starts, each calling endpoint of the meeting chat's other
  members gets `{"evt": 128, "gp": <base64 JSON>}` (`MeetingStart`, its call, chat, subject, starter and
  `meetingInfo`); a first joiner that waits in the lobby is pushed to the organizer only (`MeetingStartFromLobby`).
  A channel's meeting pushes none.
- **The lobby** is the meeting's `AutoAdmittedUsers`, read at each join: the organizer always comes in; under
  `InvitedUsers` the meeting chat's people do, under `OrganizerOnly` nobody else; any other value lets every account
  in (they are all of the company). Whom it holds waits: its roster entry has a `lobby` endpoint (`modalityJoined`
  `Lobby`), everyone sees it waiting, and it has no media. Its offer is kept with its seat; a host's `admit`
  (`conv/<id>/admit`) makes its leg on the media node with that offer, and its acceptance comes as a joiner's, so the
  client goes from the lobby straight into the call (Teams' lobby media session and retarget aren't used). A host's
  `removeParticipant` turns someone waiting away (`403` at its `end`) or takes someone out of the call (`0`, the
  others told). A guest who comes before anyone is in waits all the same; the call goes on while anyone is in it or
  waits.
- **Hosts** are the organizer and the presenters in the call: they let in, turn away, add and mute. Each roster entry
  names its `meetingRole`: `organizer`, `presenter`, or `attendee` while only the organizer presents.
- **The options in effect**: the call takes `AutoAdmittedUsers`, `PresenterOption` and `AttendeeRestrictions` when it
  starts and again each time the organizer saves them (Controls, or the options page). With `PresenterOption`
  `OrganizerOnly` (or `SpecificPeople`, whose list isn't kept) everyone else is an attendee; while attendees may not
  unmute (`DisableAudio`), their main audio is `serverMuted` at a new `serverMuteVersion`. Everyone in the call or its
  lobby is told the options at their `conversationUpdate` callback (`meetingDetails.meetingCapability`, both
  restrictions as `DisableAv`); a meeting's conversation names them too.
- **Mute others**: a host's `conv/<id>/mute` mutes everyone else (`scope` `all`, the client's Mute all) or those its
  `muteParticipants` name. Each muted party's main audio turns `serverMuted` at a new version, so its client mutes
  itself; the outcome goes to the request's own `muteUnmuteAsync` callback. The party's own unmute takes the mute
  back. Signaling only: the media node mixes whatever the party sends.
- **End for all**: the organizer's `DELETE` of the conversation ends the call for everyone in it and in its lobby
  (`ConversationEndForAllInitiated`, the organizer its sender) and drops the room; anyone else's is a leave.
- **A channel's meeting**: the channel's Meet now subscribes at `epconv` with the channel and
  `enableGroupCallMeetupGeneration` (no call invitation); that makes the channel's meeting, its caller the organizer
  and nobody in it yet (a meeting running in the channel is answered instead), and the organizer then enters it at its
  conversation; a Meet now in a thread that isn't a channel (a scheduled meeting's chat too) is `400`. The first
  entry posts the meeting to the channel as its organizer's: an `Event/Call` `callStarted`
  naming where it runs, whose `properties.meeting` names its organizer and tenant and, once the organizer titles it
  (`PUT …/messages/<post>/properties?name=meeting`, the post's author only), its title. The channel's people join from
  that post at the conversation; the call's end, under the same call id, is a reply in that post's chain, so the
  channel shows the post alone, ended (a meeting nobody entered has no post and posts no end). One meeting runs in a channel at a time.

Not served yet: the test call, transfer, voicemail; for meetings, CSA's `activeMeetups` (the client finds a channel's
meeting by its post), a scheduled channel meeting's call (only a Meet now's runs in a channel), co-organizers, a lobby
for a meeting locked by its organizer, Spotlight, the meeting's timer, notes and layout, and the meeting chat's
`AllowMeetingChat`.

### The call log

When a 1:1 or group call ends, each of its parties gets an entry in its own call log (`48:calllogs`, one of [the
account's own streams](#the-accounts-own-streams)), and each callee who never took it gets a missed call in its
activity feed. No capture shows the service writing either; both take the shapes a lab account's synced streams
hold (`docs/protocol/chat.md`, "The call log"), and the client's catch-up and push store them as any stream's. A
[meeting's](#meetings) call leaves neither, as in Teams.

- **An entry** is a `Text` message from the caller, "Call Logs for Call <call id>", whose `properties.call-log`
  (JSON as text) tells the call: its id and times (`connectTime` when the party came into it, else null), the
  party's `callDirection` (`outgoing` for the caller, `incoming` for everyone else, a joiner too), `callType`
  (`twoParty`, `multiParty`) and `callState`: `accepted` when the party came into the call, `declined` when it
  declined (and didn't come in later), else `missed`. The caller of a call nobody took has it as `missed`, as
  Teams' logs show it. The caller is the `originator`; a 1:1 call names the callee as `target`, a group call every
  party it rang in `participants` and its chat in `threadId`. `sharedCorrelationId` is the call's conversation and
  `userParticipantId` the party's own participant id; forwarding, transfer and attributes are null.
- **A missed call** (`activityType` `call`, `activitySubtype` `missedCall`, the caller as its source) goes to each
  party but the caller whose entry is `missed`: a cancelled ring, one that ran out, a group call that ended while it
  rang, someone with no calling endpoint. A decline is none. The client reads it as "Missed call from <caller>".
- Both are written after the call's end is delivered, one transaction per stream, and told on each account's push
  channel, so its clients on any node have them at once (each client listed both streams on its first sync).

### The TURN relay (coturn)

`GET /trap/tokens` (the account's `ic3` token) answers Teams' shape, `{"tokens": [{"realm", "username",
"password"}], "expires"}`, and beside it `urls`, the relay's URLs from `calling.turn.urls`. With
`calling.turn.secret` the credential is the TURN REST API's: the user name is `<expiry in Unix seconds>:<account
id>` and the password the base64 HMAC-SHA1 of the user name under the secret, so coturn checks it without asking
the server and it runs out after `calling.turn.ttl`. Without a secret, `calling.turn.username` and
`calling.turn.password` name one long-term user. With neither the answer is `503` and calls have no media.

The server refuses to start when a URL isn't a TURN URL (RFC 7065: `turn:<host>[:<port>][?transport=udp|tcp]`,
`turns:<host>[:<port>][?transport=tcp]`; ports 3478 and 5349 when none is given), when URLs and a credential
aren't set together (either alone leaves every call without media), or when the long-term user's name or password
is empty (clients refuse an empty one).

`ngteams` takes an own-server account's relay from `urls`, per account: its media allocates on those servers, in
order, with the credential beside them. It leaves out a URL that isn't a TURN URL or whose host is in a Microsoft
domain, so an own server's credential never reaches Microsoft; with none left the call fails at its setup and no
allocation is made. A named relay's own `300 Try Alternate` is followed, as Teams' relay's is: the relay names
its alternate, and it already holds the credential. A Microsoft account in the same window keeps Teams' relay host
and ignores any `urls`. Clients
need the relay's hosts and ports reachable themselves: the server isn't on the media path.

A coturn for it:

```
listening-port=3478
tls-listening-port=5349
realm=rtcmedia
use-auth-secret
static-auth-secret=<the same secret as calling.turn.secret>
cert=/etc/coturn/fullchain.pem
pkey=/etc/coturn/privkey.pem
no-multicast-peers
no-tcp-relay
denied-peer-ip=10.0.0.0-10.255.255.255
denied-peer-ip=100.64.0.0-100.127.255.255
denied-peer-ip=172.16.0.0-172.31.255.255
denied-peer-ip=192.0.0.0-192.0.0.255
denied-peer-ip=192.168.0.0-192.168.255.255
denied-peer-ip=169.254.0.0-169.254.255.255
denied-peer-ip=198.18.0.0-198.19.255.255
denied-peer-ip=fc00::-fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff
denied-peer-ip=fe80::-febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff
```

These are the limits of the coturn in `packaging/server/compose.yaml`: it relays UDP only, and never to a private,
CGNAT, benchmark or link-local address.

Clients relay everything, so the relay needs its UDP range (`min-port`, `max-port`) open and enough bandwidth for
every call's media twice. Several coturn instances with one secret serve any number of nodes.

## Presence

The wire is UPS's (`docs/protocol/trouter.md`, "Others' presence" and "Own presence"), under `/ups/<geo>/v1/`, with
the token of `https://presence.teams.microsoft.com/`. Every request is from an account that can sign in, with a
token from after its last password reset.

| Request | Does | Answers |
|---|---|---|
| `POST presence/getpresence/` | reads the presence of a list of `{"mri"}` (1 to 650) | 200, one item per MRI in order: `mri`, `presence` (`sourceNetwork` `Self` or `SameEnterprise`, `calendarData.isOutOfOffice`, `capabilities`, `availability`, `activity`, `lastActiveTime`, `deviceType` while an endpoint is live), `etagMatch`, `etag`, `status` `20000`, and `source` when asked with it. An MRI that is no account that can sign in gets `status` `40400` and no `presence` |
| `PUT me/endpoints/` | reports the account's endpoint (`x-ms-endpoint-id`, the body's `id`) with its `availability` and `activity` | 201; 400 for another form; 429 past `presence.max_endpoints` |
| `PUT me/forceavailability/` | sets the manual status (`Available`, `Busy`, `DoNotDisturb`, `Away`, `BeRightBack`, `Offline`); with no body, clears it | 200 |
| `POST pubsub/subscriptions/<epid>` | changes whom the caller's messaging endpoint watches: `subscriptionsToAdd`, `subscriptionsToRemove`, and with `shouldPurgePreviousSubscriptions` everything it watched before is dropped first | 200 `{"addStatuses", "removeStatuses"}`, an item per MRI: `20200` taken, `40020` no such account, `40040` past `presence.max_subscriptions`. 400 without `x-ms-correlation-id` or `x-ms-client-version` ("Missing mandatory header"), or when `trouterUri` isn't the caller's own endpoint's push URL followed by `unifiedPresenceService` |

- **What an account shows.** Without a live endpoint: `Offline`, whatever its manual status. With one: its manual
  status when it has one, else the busiest of its live endpoints (`DoNotDisturb`, `Busy`, `BusyIdle`, `Available`,
  `AvailableIdle`, `BeRightBack`, `Away`), with that endpoint's activity. `lastActiveTime` is the time of the
  account's last endpoint report (`0001-01-01T00:00:00Z` before the first). `isOutOfOffice` is the directory's
  ([People](#people)).
- **An endpoint lapses** `presence.endpoint_ttl` after its last report: the client sends none when it quits, so
  an account shows offline up to that long after its last client stopped. Each node looks for lapsed endpoints
  every second. A manual status stays until it is cleared or 30 days after it was set, over restarts of the
  nodes and of Redis; when it expires, its watchers are told what the account shows then. Deleting an account
  deletes its manual status.
- **Deliveries.** A subscription holds for `presence.subscription_ttl`. Right after it, the endpoint gets a
  snapshot of the people it newly watches (`"isSnapshot": true`); then each change of what one of them shows, as a
  `3:::` delivery at `/trouter/v4/f/<endpoint>/unifiedPresenceService` with UPS's body
  (`{"presence": [{"mri", "etag", "source", "presence"}]}`). A report that changes nothing a watcher sees is not
  delivered. Presence deliveries go only to the sockets open now and are not kept for replay: the client subscribes
  again after each connect and gets a snapshot. A change of `isOutOfOffice` alone is not pushed; the next read or
  change carries it.
- **State.** The manual status is in the database (`presence_manual`: the status and when it expires). The rest
  is in Redis when `redis_url` is set, else in the node. The keys, in the deployment's
  [keyspace](#scaling) (`ngts:<namespace>:…`), are `presence:account:<account>` (a hash: the last activity, each
  endpoint with its lapse time), `presence:due:accounts` (a sorted set: the accounts with an endpoint,
  by their next lapse, of an endpoint or of the manual status), `presence:watchers:<mri>` and `presence:watching:<account>|<endpoint>` (sorted sets by the
  subscription's end, each forgotten whole when its last subscription ends). The node's own state forgets its keys
  as Redis does. A change is published on the watcher's `push:<mri>` as
  `{"resourceType": "Presence", "endpoints": […], "resource": <the delivery's body>}`.

## People

The directory holds, beside each account's name and address, an optional profile (`user_profiles`): given name,
surname, job title, department, office, company, two phone numbers, the account it reports to, whether its
automatic replies are on, and its picture. The operator sets them:

```sh
ngteams-server --config server.toml profile set bert@example.com --job-title Engineer --office "Room 4" \
    --manager cora@example.com --out-of-office true
ngteams-server --config server.toml profile picture bert@example.com bert.png
ngteams-server --config server.toml profile show bert@example.com
```

`profile set` takes `--given-name`, `--surname`, `--job-title`, `--department`, `--office`, `--company`,
`--mobile-phone`, `--business-phone`, `--manager <upn>` and `--out-of-office true|false`. An empty value clears a
field; a field not named stays. A value has at most 256 characters. An account can't report to itself, directly or
through the accounts that report to it. `profile picture` takes a PNG, JPEG, GIF or WebP file of at most 4 MiB,
cuts its middle square and keeps it as a 96 by 96 JPEG in the file store, in place of the one before;
`profile remove-picture` takes it away. A picture doesn't count toward the account's storage quota: the operator
sets it, and it is a few kilobytes.

The reads, each for any account that can sign in:

- **Short profiles**: `POST <middle tier>/beta/users/fetchShortProfile` with a list of MRIs (at most 500), or with
  `?isMailAddress=true` a list of addresses. `{"type", "value": [profile]}`, a profile per account found: `mri`,
  `objectId`, `displayName`, `email`, `userPrincipalName`, `userType`, `accountEnabled`, and `givenName`,
  `surname`, `jobTitle`, `department`, `officeLocation`, `companyName`, `mobile`, `telephoneNumber` where the
  directory has them. A disabled account still answers, so what it wrote keeps its author's name; a deleted one
  doesn't.
- **Graph**: `GET /graph/v1.0/me` and `GET /graph/v1.0/users/<id>` answer the user's record (`id`, `displayName`,
  `userPrincipalName`, `mail`, `businessPhones`, `givenName`, `surname`, `jobTitle`, `mobilePhone`,
  `officeLocation`, `preferredLanguage`), or with `$select` only the properties it names. An account that was
  deleted answers 404 `Request_ResourceNotFound`. `GET /graph/v1.0/users/<id>/photos/96x96/$value` answers the
  picture (`image/jpeg`), or 404 `ImageNotFound` for an account without one, a deleted one and any other size.
  Deleting an account takes its picture out of the file store. When the file store can't be opened, `user delete`
  still deletes the account and leaves the picture (never served), warns on standard error and exits 0.
- **The person card** (Loki, under `/loki/`, with the token of `https://loki.delve.office.com` and an
  `X-ClientType` header, else 400 `ClientTypeMissing`; `docs/protocol/loki.md`). The persona is `aadObjectId`
  (and the same account in `teamsMri` when that is sent); one that can't sign in answers 404.
  - `api/v1/livepersonacard/configuration`: `LokiUrl` is `<origin>/loki/`.
  - `api/v2/person`: names, the address as mail and IM address, the UPN, `userType` `Member`.
  - `api/v1/organization`: `managers` (the reporting line upward, nearest first, at most 8) and `directs`.
  - `api/v1/workingwith` (`limit`, 12 by default, at most 50): the other people of the threads **the reader and
    the person are both in now**, those in the most of them first. So a card never tells a reader of a chat the
    reader isn't in.
  - `api/v1/schedule`: no calendar is behind it yet, so the status is `Unknown` (`Oof` while automatic replies are
    on) and no working hours are named.
  - `api/v1/oofstatus`: `outOfOfficeState` `Disabled`, or `AlwaysEnabled` while automatic replies are on.

The team pickers' people searches are in [Teams and channels](#teams-and-channels).

## Search

The title bar's search is Substrate's (`docs/protocol/search.md`), under `/search/` with the token of
`https://outlook.office.com/search`: `POST search/search/api/v1/suggestions` as the query is typed (`Groups`, one
per kind asked that the server searches) and `POST searchservice/api/v2/query` once it is entered (`EntitySets`, one per entity request, with
`MoreResultsAvailable`). An ask without entity requests (the web client's empty contextual one) or with more than 16
is answered 400; a kind the server doesn't search (`Event`, `App`, `Text`) finds nothing: a query gives it an empty
entity set, and the suggestions give it no group. A request takes at most 50
of its kind, from at most the 1000th.

The query is the user's words, never a syntax: each run of letters and digits (at most 16) must start a word, all of
them. Words are compared folded, the same on both backends: lower-cased, compatibility forms decomposed (NFKD), and
Latin letters without their accents (`é` as `e`, `ø` as `o`, `ß` as `ss`), so "creme" finds "Crème"; other scripts
keep their marks. Each kind finds only what the reader may see:

| Kind | What it finds |
|---|---|
| `Message` | the text of the messages of the threads the reader sits in, within the history they were given and up to when they left, the newest first; the text around the match marked (`<c0>`), with the thread, the id, the sender and the thread's other people, and in a channel the post it belongs to |
| `People` | the accounts that can sign in, by name, given name, surname or address, with their title and department |
| `Chat` | the reader's group chats (not a 1:1 chat, a channel or a team) by topic or another of their people's names |
| `Team`, `Channel`, `TeamsChannel` | the teams the reader is on, and the channels of them the reader sits in, by name, with the team's name |
| `File` | the files the reader owns or was sent, by name, with their sharing link |

Message text is indexed in the message's own transaction (`message_search`): as the reader sees it (a rich
message's text, a mention's name, an emoji's `alt`, a post's subject), replaced on an edit, emptied on a delete, with
its folded words (as the query is split); Postgres finds the words by full-text search (`to_tsvector('simple',
words)`, a GIN index, each word a prefix), SQLite by FTS5. Names are short and few: their folded words
(`search_names`, with the text they were folded from) pick the candidates, and the server matches each name. A name
changed since it was folded is a candidate of every search until the search that reads it folds it again. At its
start the server indexes the messages from before the index and folds the names not yet folded.

## Chat events

Chat creation publishes to every member, including the acting account's other endpoints, on `push:<mri>`.
The internal JSON envelope is
`{"resourceType":"ThreadUpdate","resource":<thread>}`; the resource has the thread-read shape. Trouter delivery
wraps this internal event separately. A message post publishes `NewMessage` with the stored message as `resource`;
an edit, soft deletion or reaction publishes `MessageUpdate` to the same recipients. Typing and peer read receipts
exclude the acting account.
An activity item is published to its account alone, as a `NewMessage` (or `MessageUpdate`) of its stream.
Edits require message ownership and preserve its ID, client message ID, original arrival time and reactions. Publication happens after the transaction commits. The fan-out is at most
once; a socket reads kept events from the journal instead (above), and clients catch up through conversation sync
after a reported loss.

Database versions advance a shared row inside the write transaction. A conversation page keeps its upper bound
and follows versions as a keyset; a chat changed during paging appears on the next incremental scan. This keeps
clocks on different HTTP nodes out of cursor ordering. Conversation arrays, including their last-message
previews, stay below a 3 MiB serialized budget; a cut page's forward link starts after the last included chat,
and every page's sync link keeps the snapshot's upper bound. If a single preview exceeds the array budget,
its reaction summary is omitted from the listing; its text, identity and stored message remain unchanged.

Posts use the authenticated account as sender. A repeated `(chat, sender, clientmessageid)` returns the original
message and emits no duplicate event. Message IDs and versions come from the shared version clock, and a
message page keeps its watermark in backward links. Incremental pages filter by changed version and order by
arrival ID. While an incremental answer is cut, its segment start stays above the prior watermark even when
the last row is a changed older message; only the final page reports that prior watermark. This keeps the
native engine following every backward continuation. Message arrays stay below a 3 MiB serialized budget.

Read horizons are stored per `(chat, account)` and returned only in that account's conversation listing.
Their own endpoints receive a `ConversationUpdate` with `propertiesUpdated` and the changed property. Other
members get an internal `ReadReceipt` resource (`id`, `mri`, `consumptionhorizon`); that is separate from the own
horizon update, which must never mark another member's conversation read. A conversation's
`consumptionHorizonBookmark` ("Mark as unread" and its end) is kept the same way, per `(chat, account)`: its listing
and its own endpoints only, and other members are told nothing.

Own user properties persist `userGraphSettings` as its native JSON text and `appConsumptionHorizon` as JSON text
or an object. Mark-all-read may be reset, as in Teams. Writes retain the account's other properties and publish
`CustomUserProperties` only to that account. An account with no own properties receives the native 404/subcode-1
shape that the client treats as empty preferences.

## Rich messages

A post or an edit is stored as Teams gives it back (`docs/protocol/chat.md`, "Mentions" and "A message as read"), so
every reader, the push channel and the sync see the same message (`src/chatsvc/rich.rs`):

- **Mentions.** The compose box's wrapper (`<readonly itemtype="…/Mention">`) is taken off and its span kept; such
  a message's HTML is then written out again by html5ever, as Teams writes it (`itemscope=""`). HTML without that
  wrapper, and a `Text` message's content, are kept byte for byte. `properties.mentions` stays a JSON string, each
  entry's `itemid` a number. A mention of everyone is an entry like any other, its `mri` the chat's id.
- **Quotes.** `properties.qtdMsgs` (a JSON string as sent) is read back as a list of objects: `messageId` a number,
  `sender` and `time` as sent, `message`, `sharedRefId` and `replyChainId` null, and `validationResult` `Valid` for
  a message of the same chat that the writer may read and that isn't deleted (`Invalid`, the server's own word, for
  any other, a deleted one as a missing one: nothing is told about a message elsewhere). `hasValidMsgReferences` is true only when there is at least one quote and every quote is valid (for an empty
  list it is false, the server's own choice: the native client sends `qtdMsgs` only when it quotes). The quote in the HTML is
  kept as sent; Teams' line feed after it is not added (it changes nothing a reader parses).
- **Edits** keep the message's id, client message id and arrival; `properties.edittime` is the edit's time and
  `properties.composetime` the compose time the editing client sends, as Teams keeps it (`edit.json`).
- **Refused** (400): `mentions` or `qtdMsgs` that aren't a list of objects, or a JSON string of one.
- **Not kept**: `importance`, `subject` and `title` (the native client sends them empty), a forward's
  `forwardTemplateId` and `originalMessageContext` (the native client doesn't forward), and the push delivery's
  `mentiontypes` beside the resource (no client reads it). Who a mention reaches is [below](#who-gets-an-item).

## The account's own streams

`48:notes`, `48:notifications`, `48:mentions` and `48:calllogs` are each kept as a thread only their account is in,
under an id no request can name. A request's `48:…` id always resolves to the caller's own stream, so the membership check that
guards every chat also keeps one account's stream from another. A stream is no thread: its thread and member reads
answer 404, as does any other `48:` id (`48:generic`: 400).

- **The server-written streams** (`48:notifications`, `48:mentions`, `48:calllogs`) are made at the account's first
  conversation list (when it has none yet) and are in the list from then on, empty too: a client takes a pushed item
  only into a conversation it has, and it can't add a stream by its thread read.
- **Notes** (`48:notes`): `POST …/48:notes/messages` writes a note, and its edit, delete and message pages work as
  a chat's. They answer 404 until the first note is written, and are in no conversation list, as with Teams. A
  note is pushed to the account's own endpoints.
- **The call log** (`48:calllogs`) is written by the server only, one entry per call ([above](#the-call-log)),
  and takes its account's `consumptionhorizon`.
- **The activity feed** (`48:notifications`) and **the mentions stream** (`48:mentions`) are written by the server
  only (a post there is refused, 403). The list shows their `consumptionhorizon` and `consumptionHorizonBookmark`.
  The account marks an item read or unread (`PUT …/messages/<item>/properties?name=isread`), removes it (`DELETE …/messages/<item>?behavior=softDelete`), moves
  the feed's own horizon and marks all read (the two properties). Each change moves the item's or the stream's
  version, so the account's other clients sync it.

### Who gets an item

An item is written in the transaction that writes its message, from the thread's people as they are then. It
carries a preview of the message, so only someone who is in the thread and may read the message gets one, whatever
the message's mentions name. A disabled account gets none. The sender gets none for their own message.

| What happened | Who gets an item | `activityType` / `activitySubtype` |
|---|---|---|
| a mention of a person | that person; also in their `48:mentions` (`mentions` / `mention` in a 1:1 chat, `person` in a group chat or channel) | 1:1 chat `mentions` / `mention`; group chat `mentionInChat` / `person`; channel `mention` / `person` |
| a mention of everyone (`everyone`, `team`, `channel`) naming the thread itself (a channel: or its team) | everyone in the thread; none in a 1:1 chat | `mentionInChat` or `mention` / `everyone`, `team`, `channel` |
| a reply in a channel post's thread | the people who wrote in that thread; people with "Follow all new threads" on (`followAllRc`) | `reply` / `reply` |
| a post or reply in a channel | people whose `channelNotificationSettings` have `allNewPosts` `On` (replies: with `includeReplies`) | `follow` / `channelNewMessage` (the server's own names: no capture shows Teams') |
| a reaction | the message's sender; one item per reactor | `reactionInChat` or `reaction` / the reaction's key |

The account's own settings of the thread apply: a muted thread (`alerts` `false`) tells of nothing but a mention of
the account itself. A person named and also taken in by a mention of everyone gets one item, their own.

A change of the message changes the items it made, for the people in the thread at that moment:

- **An edit** rewrites each item where it is, with the new wording and as read as it was; an item its account
  removed stays removed. Someone it newly mentions gets an item, and a mention's item of someone it no longer
  mentions is removed, in the feed and in `48:mentions`.
- **A reaction** given again, or a second one of the same person, is new activity: the reactor's earlier item is
  removed and a new one written, so it has the time of the new reaction. One taken back leaves the item on the
  newest reaction of theirs that stays, at the time it had; the last one taken back removes the item.
- **A delete** removes the message's items, so its text stays in no preview of theirs.

Someone who left the thread keeps their items as they were then: a later edit, delete or reaction changes none of
them and pushes them nothing, as it stays out of what they may read (below).

## A group chat's people

`PUT v1/threads/<chat>/members` (`{"members": [{"id", "role": "Admin", "shareHistoryTime"}]}`) adds people to a
group chat and `DELETE v1/threads/<chat>/members/<mri>` takes one out (the caller's own MRI: they leave). Anyone in
the chat may do both, as in Teams, where a group chat's every member is an `Admin`. A 1:1 chat keeps its two
people (400), a team's channel takes and loses people through its team (400), and only accounts that can sign in
are added (400). Someone already in the chat is no change (200).

- **History.** Each member has the first message they may read: all of it (`shareHistoryTime` `0`), from a time on
  (a positive time), or from their joining (`-1`). Message pages, the page around a time, the conversation list's
  preview, edits, reactions and activity items all keep to it. A message from before it answers 404.
- **Leaving.** Someone removed or gone keeps reading what they could until then, the event of their going
  included, and nothing written or changed after it; their thread and member reads and every write answer 403. The
  chat stays in their conversation list at the version it had when they left and never moves again. Added back,
  they read from where the new add says.
- **Events.** Each change writes the thread's own message (`ThreadActivity/AddMember` with each target's
  `sharehistorytime`, `ThreadActivity/DeleteMember`), moves the thread's `rosterVersion`, and publishes the message
  (`NewMessage`) and the thread (`ThreadUpdate`) to the people in the chat after the change; each running client
  reads the chat's people again on the update, so every member's list follows live. Someone taken out is told the
  event of their going and nothing after it. A thread's own message can't be edited, deleted or reacted to.
- **Concurrent changes.** A post, edit or reaction checks its sender's membership and reads the thread's people in
  its own transaction, under the shared version clock. Its event and its activity items go to those people and
  nobody else, so a member removed while the request ran is told nothing of a message written after their removal.
  A read receipt and typing go to the people their own write or moment of the clock finds, the same way. A message
  read (a page, the page around a time, the conversation list's preview) takes the reader's seat in the statement
  that reads the messages, so a removal never falls between a check and the read.

A group started from a 1:1 chat is a new chat (`POST v1/threads` with the three people, as the client sends it);
the 1:1 chat and its history stay its two people's.

## Scaling

- HTTP nodes keep no state of their own: Postgres is the source of truth, so any node answers any request. SQLite
  means a single node; it needs no Redis.
- On SQLite the operator's commands (`user add`, `migrate` and the others) may run beside a node that is running or
  still starting, on the same file. The file is in WAL mode, so a read does not wait for a writer. The waits for the
  write lock another process holds:
  - each statement that writes waits up to 5 seconds;
  - a process that finds migrations not yet applied (a new file, or a new version) takes the write lock for them
    and waits up to 2 minutes for it. A second process that migrates at once waits, then finds them applied. On a
    migrated file a command takes no lock for the migrations.

  If the wait ends first, the command fails with "another process holds the SQLite write lock on `<file>`: waited
  `<n>` s for it". Only processes on one machine share the file; never put it on a network filesystem.
- With Redis, a write's events go to `Fanout::publish` on a topic such as `push:<mri>`, and every node that holds a
  subscriber for it delivers them. Each node keeps one Redis subscription per topic and feeds its local subscribers
  from it. Without Redis the same `Fanout` trait is one process's broadcast.
- Delivery through the fan-out is at most once. A subscriber that falls behind learns how many payloads it missed.
  When a node's Redis subscription breaks, its subscriptions end: a subscriber subscribes again.
- Several deployments may share one Redis. Every Redis key and channel of a deployment is under its namespace,
  `ngts:<namespace>:<name>`: the sign-in and setup counters (`login:<hash>`, `setup:<hash>`), the fan-out's topics
  (`push:<mri>`, `trouter:<mri>`), the journal (`journal:<topic>`, `cursors:<topic>`) and presence
  (`presence:account:<account>`, `presence:due:accounts`, `presence:watchers:<mri>`,
  `presence:watching:<account>|<endpoint>`). The namespace is
  `redis_namespace`, or else the tenant id the nodes read from the database: the nodes of one deployment agree with
  no configuration, and another deployment's sign-in holds, pushes, replays and presence never meet them. The command line's
  account commands reach the nodes in the same namespace. The names are built in one place,
  `fanout::keyspace` (`Keyspace::key`, and `Shared`, the only way to a node's Redis connection): new code that keeps
  anything in Redis names it there, never with a bare key.
- To upgrade from a version without the namespaced keys, restart all nodes together. In a rolling upgrade, the old
  and the new nodes use different names: until all nodes run the new version, pushes and replays between them are
  lost, presence and sign-in holds are not shared, and the command line's disconnects do not reach the nodes of the
  other version. The upgrade also abandons the data kept under the old names: the journals' replay state, active
  sign-in holds and presence start again empty.
- A Trouter socket lives on one node. Its node subscribes to the account's `push:<mri>` and `trouter:<mri>` (control:
  a registration taken, a socket's claim to its endpoint, an event the journal didn't keep, a disconnect) topics. A
  kept event is appended to the account's journal before it is published, so the node that is woken reads it there,
  in order; a socket that lagged or reconnected reads from its endpoint's cursor. Registrations are rows, so any node takes one and any node reads it.
- Presence lives in Redis: any node takes an endpoint's report or a subscription, any node answers a read, and
  the node that sweeps a lapsed endpoint (the one that takes the account off the due set) tells its watchers. A
  change reaches a watcher's socket on any node through the watcher's `push:<mri>`.
- Pictures and files live in the shared store and their rows in the database, so one node takes an upload and
  another serves it. A download link one node signed opens on every node. Each account's stored bytes are counted
  in one row updated in a single statement, so two nodes can't both pass the quota.
- Signing keys are rows in the database. A node rotates the key when it is due (checked hourly) and drops keys that
  can no longer verify a live token or file link. Retired keys are kept for the access lifetime, or
  `media.link_lifetime` when that is longer, plus three hourly reload intervals and the verifier's 60-second clock
  allowance, covering two missed maintenance checks. Nodes that cannot reload for longer than three hours should be
  taken out of service until their database connection recovers.
  `ngteams-server keys rotate` creates a key immediately; running nodes switch at their next hourly check, or
  earlier if verifying an unknown key triggers a reload. Rotation is serialized in the database across nodes.
- Account administration keeps its state in the database: the setup code's digest, the admin role, the audit. A
  setup code issued by one node is taken by any node. Setup attempts are counted in Redis when it is configured,
  else per node.
- A call is a row (`calls`): its parties, their callbacks and where it stands, as JSON beside its version. A
  request on any node reads the row, makes its step and stores it only if the version is still the one it read;
  otherwise it reads again. What the step tells a party is published on `call:<endpoint>`, and the node that holds
  that calling endpoint's socket delivers it. Every node looks twice a second for calls that are due (a ring's end,
  a call whose keep-alives stopped, an ended call's row five minutes later); the version check lets one of them act.
  A group call's row also names its media node's room, so any node reaches it; a step that lets a leg or the room go
  drops it on the node once the step is stored.
- Media nodes (`serve --role media`, or the media node of `--role all`) carry the group calls' audio; add nodes for
  more calls. A new room goes to the live node with the fewest legs that isn't stopping: with Redis the registry
  lists them (`media:nodes`, a sorted set of node ids by their last heartbeat, and `media:node:<id>`, what reaches the
  node and what it carries, expiring after `mixer.dead_after`); without Redis a node's only media node is its own
  process's. A call's requests never ask Redis: they go where the call's row says.
- Migrations take a Postgres advisory lock, so nodes starting together take turns.

## The client

`ngteams` signs in to this server from its account window ("Own server": address, user name, password):
[personal-build.md](personal-build.md#own-server-accounts). A deployment with a certificate
the system doesn't trust gives its certificate authority to each account as `ca_file` (the form's "Certificate
authority file" field).

The client also uses the [first-admin setup](#the-first-admin), the [admin API](#admin-api) and the
[account's own endpoints](#the-accounts-own-endpoints): the account window asks `GET /api/admin/v1/setup` for a
server's address and creates the first administrator there; it replaces a password that must change with
`POST /api/account/v1/password`; and an administrator's Settings have a "Manage users" page over
`/api/admin/v1/users` (the page is offered when `GET /api/account/v1/me` says `admin`); with `NGTEAMS_BOTS=1` they also
have a "Manage apps" page over `/api/admin/v1/apps` (offered when that list answers) that reads
`/bot/v1/.well-known/openidconfiguration` for the settings it shows each bot.

## Tests

`cargo test -p ngteams-server` runs the e2e tests: the real client crates (`teams-auth`'s session and clients,
`teams-core`'s account bring-up and engine, `teams-call`'s call machine, `teams-media` on its file device backend
through its loopback TURN server) against an in-process server on loopback, on SQLite. With `NGTEAMS_SERVER_PG_URL`
(a Postgres the tests may create databases on) the Postgres cases run too, and with `NGTEAMS_SERVER_REDIS_URL` the
Redis cases. The picture and file tests keep their blobs in the S3 store `NGTEAMS_SERVER_S3_URL` names
(`http://<access key>:<secret>@<host>:<port>/<bucket>`; they make the bucket), else in a temporary directory. CI's
Coverage job sets all three from its service containers (RustFS for S3). Each test's deployment has a database and a
Redis namespace of its own, so the tests run in parallel on one Postgres and one Redis. That Postgres takes 300
connections (CI's too): four test threads of two deployments with two nodes each, each node's pool 16. Locally:

```sh
podman run -d --rm --name ngts-pg -e POSTGRES_PASSWORD=ngts -p 55432:5432 docker.io/library/postgres:17 \
  -c max_connections=300
podman run -d --rm --name ngts-redis -p 56379:6379 docker.io/library/redis:7
podman run -d --rm --name ngts-s3 -p 59000:9000 -e RUSTFS_ACCESS_KEY=ngtsaccess -e RUSTFS_SECRET_KEY=ngtssecret \
  docker.io/rustfs/rustfs:1.0.5
NGTEAMS_SERVER_PG_URL=postgres://postgres:ngts@127.0.0.1:55432/postgres \
NGTEAMS_SERVER_REDIS_URL=redis://127.0.0.1:56379 \
NGTEAMS_SERVER_S3_URL=http://ngtsaccess:ngtssecret@127.0.0.1:59000/ngts-test cargo test -p ngteams-server
podman stop ngts-pg ngts-redis ngts-s3
```

With `NGTS_TEST_COTURN` (a coturn's `host:port`) and `NGTS_TEST_COTURN_SECRET` one more case relays a
call through that coturn with the credentials made from its secret (coturn relays between loopback peers only
with `allow-loopback-peers`, which it takes only beside a password for its own CLI: `COTURN_CLI` is that option,
any value):

```sh
podman run -d --rm --name ngts-coturn --network host docker.io/coturn/coturn -n --listening-ip=127.0.0.1 \
  --listening-port=53478 --relay-ip=127.0.0.1 --use-auth-secret --static-auth-secret=test-secret \
  --realm=rtcmedia --allow-loopback-peers "$COTURN_CLI"
NGTS_TEST_COTURN=127.0.0.1:53478 NGTS_TEST_COTURN_SECRET=test-secret \
  cargo test -p ngteams-server relayed_by_coturn
podman stop ngts-coturn
```

`binary::bots::containers` runs each SDK's echo-and-cards bot (`tests/bots/`, the Teams SDK's and the Agents SDK's)
in a container against the real binary, when `NGTEAMS_SERVER_BOT_IMAGES` names them (`teams,agents`); without it the
cases pass and do nothing. `tests/bots/build.sh` builds the images. The engine is `podman` unless
`NGTEAMS_SERVER_BOT_ENGINE` names another, and a bot joins the host's network unless `NGTEAMS_SERVER_BOT_NETWORK`
names another (CI's Coverage job sets `docker` and the network of its test container, and sets the images):

```sh
tests/bots/build.sh
NGTEAMS_SERVER_BOT_IMAGES=teams,agents cargo test -p ngteams-server --test e2e -- binary::bots::containers
```

`binary::cluster` runs the server as [Operations](#operations) deploys it: two `serve --role api` nodes and a
`serve --role media` node of the real binary on one Postgres, one Redis and one bucket, coturn as the relay, and a
front per node that sends a connection to another node when its own is down, as Caddy does. The server is first
seeded with 300 synthetic accounts (odd names, a few disabled), group chats with them, a team, files, a picture and
calendar events. Three clients (`srv-a` and `srv-c` through one node, `srv-b` through the other) then chat both ways
(each push in under 2 s), react, edit, read on a second device, show presence, send a picture and a file, make a
1:1 and a group video call, and survive their node being killed. The single-node mode (SQLite, no Redis) runs the
chat and call part. The two-node tests need Postgres and Redis. S3 is optional: without it, the nodes share a
directory. coturn is optional: without it, the calls are skipped:

```sh
NGTEAMS_SERVER_PG_URL=… NGTEAMS_SERVER_REDIS_URL=… NGTEAMS_SERVER_S3_URL=… NGTS_TEST_COTURN=… \
NGTS_TEST_COTURN_SECRET=… cargo test -p ngteams-server --test e2e -- binary::cluster --test-threads 4
```

`cargo test -p ngteams --test own_server_nodes_window` runs the same deployment through the app's own windows, each
`srv-*` app in a UI sandbox of its own, signed in from the first sign-in screen ("Own server", typed). The nodes share
one `public_origin`, so a front on its port passes each TLS connection through to the node the app is pinned to, and
to the other node once that one is down. It needs Postgres, Redis and coturn (S3 is optional, as above) and the
server binary (`cargo build -p ngteams-server`); screenshots go to `target/ui-shots/own-server-nodes-*.png`. Without
coturn it says so and passes, also under `GPUI_SMOKE_REQUIRE=1`, as the other call tests do.
