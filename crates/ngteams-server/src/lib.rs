//! The own Teams-compatible server (`docs/server.md`): ngteams signs in to it instead of Microsoft and finds every
//! service it serves under one origin, with the same wire as Microsoft's. No client crate depends on it.
//!
//! - [`config`]: the configuration (figment: defaults, TOML, `NGTEAMS_SERVER_*`);
//! - [`db`]: Postgres or SQLite through Diesel: accounts, refresh tokens, signing keys;
//! - [`auth`]: passwords, signing keys, tokens and the request extractors that check them;
//! - [`routes`]: the endpoints; [`services`]: the served services and the GTMs authz names them by;
//! - [`fanout`]: payloads between nodes (in-process or Redis); [`storage`]: files (`object_store`);
//! - [`throttle`]: failed sign-ins per account;
//! - [`app`]: the shared state and the router; [`serve`]: the listener, plain or TLS; [`cli`]: the command line;
//!   [`admin`]: the account commands' operations.

pub mod admin;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod db;
pub mod fanout;
pub mod routes;
pub mod serve;
pub mod services;
pub mod storage;
pub mod throttle;
