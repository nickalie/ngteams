//! End-to-end tests: the real client crates (`teams-auth`'s session and clients, `teams-core`'s account bring-up)
//! against the in-process server on loopback, on SQLite and, when `NGTEAMS_SERVER_PG_URL` is set, on Postgres.

mod authz;
mod sign_in;
mod support;
