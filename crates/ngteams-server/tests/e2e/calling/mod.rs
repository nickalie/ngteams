//! Calls: the client's own call machine (`teams-call`) and its engine's calls with their media (`teams-core`,
//! `teams-media` on the file device backend) against the server's call controller, its Trouter and its TURN
//! credentials.

mod client;
mod engines;
mod group_media;
mod group_signaling;
mod group_video;
mod held_socket;
mod history;
mod meeting_share;
mod meetings;
mod mixer;
mod qa_group_log;
mod qa_meeting_request;
mod raw;
mod redelivery;
mod relay;
mod signaling;

use crate::support::{Backend, TestServer};
use crate::trouter::{Account, server_with};
use client::Client;

/// A server with two accounts, each with a native calling client on it.
pub struct Pair {
    pub server: TestServer,
    pub a: Client,
    pub b: Client,
    pub a_id: String,
    pub b_id: String,
}

impl Pair {
    pub async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    pub async fn start_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        let server = server_with(backend, configure).await;
        let a_id = server.add_user("a@own.test", "Ada").await;
        let b_id = server.add_user("b@own.test", "Bert").await;
        let a = client(&server, &server, "a@own.test", &a_id, "Ada").await;
        let b = client(&server, &server, "b@own.test", &b_id, "Bert").await;
        Self {
            server,
            a,
            b,
            a_id,
            b_id,
        }
    }
}

/// `upn`'s calling client: its socket on `socket`, its requests through `http`.
pub async fn client(socket: &TestServer, http: &TestServer, upn: &str, id: &str, name: &str) -> Client {
    let account = Account::sign_in(http, upn, id).await;
    Client::start(&account, name, socket, http).await
}
