//! Apps and bots as an own server's admin, operator and bot developers meet them: the real `ngteams-server` binary
//! (one `all` node on SQLite; two `api` nodes and a `media` node on Postgres and Redis), the admin API with the admin's
//! own session token, the `app` commands, MSAL's client-credentials requests (raw, and msal-python, msal-node and the
//! Bot Framework SDK in podman) and the keys a bot checks activities with. Every account, app and bot is synthetic.

mod containers;
mod credentials;
mod delivery;
mod nodes;
mod operator;
mod packages;
mod people;
mod registry;
mod sdk;
pub mod support;
