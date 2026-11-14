//! End-to-end tests: the real client crates (`teams-auth`'s session and clients, `teams-core`'s account bring-up)
//! against the in-process server on loopback, on SQLite and, when `NGTEAMS_SERVER_PG_URL` is set, on Postgres
//! (Redis cases when `NGTEAMS_SERVER_REDIS_URL` is set).

mod admin;
mod authz;
mod migrations;
mod node;
mod shared;
mod sign_in;
mod support;
