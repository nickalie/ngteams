//! The stand-ins of a build without call media (the `calls` feature off, a phone's chats build): no value of them can
//! be made, so the engine has no calling, every call action is refused ([`super::CallActionError::NoCalling`]) and no
//! call event comes.

use std::convert::Infallible;
use std::marker::PhantomData;
use std::sync::Arc;

use teams_proto::{Client, HttpTransport};
use tokio::sync::mpsc;

use super::CallingTask;
use crate::engine::Event;
use crate::outbox::Outbox;

/// The account's calling, which a build without call media cannot make.
#[derive(Debug)]
pub enum CallParts {}

/// A caption line, which only a call's media receives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caption {}

/// A call's health, which only a call's media measures.
#[derive(Clone, Debug, PartialEq)]
pub enum CallHealth {}

/// The engine's calling, which a build without call media never runs.
pub(crate) struct Calling<C, T>(Infallible, PhantomData<fn() -> (C, T)>);

impl<C, T> Calling<C, T> {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the signature of the calling with call media, which keeps them"
    )]
    pub(crate) fn new(
        parts: CallParts,
        _outbox: Arc<Outbox<T>>,
        _client: Arc<Client<HttpTransport<C>>>,
        _events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        match parts {}
    }

    pub(crate) fn spawn(self) -> CallingTask {
        match self.0 {}
    }
}
