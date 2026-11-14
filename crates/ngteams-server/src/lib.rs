//! The own Teams-compatible server (`docs/server.md`): ngteams signs in to it instead of Microsoft and finds every
//! service it serves under one origin, with the same wire as Microsoft's. No client crate depends on it.
//!
//! - [`config`]: the configuration (figment: defaults, TOML, `NGTEAMS_SERVER_*`);
//! - [`db`]: Postgres or SQLite through Diesel: accounts, refresh tokens, signing keys;
//! - [`app`]: the shared state and the HTTP router; [`serve`]: the listener, plain or TLS.

pub mod admin;
pub mod app;
pub mod auth;
pub mod config;
pub mod db;
pub mod serve;
pub mod cli;
