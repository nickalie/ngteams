//! Each request answered on GPUI's foreground, over the app's accounts as they run ([`Running`]).
//!
//! - `hello`: the protocol's version checked (another one is [`ErrorCode::VersionMismatch`]) and the app's version
//!   told.
//! - `status`: each account with its engine's state (`connected`, `starting`, `down`, `stopped` or `signed_out`), the
//!   one the window shows, where the window is (shown, in the tray, closed) and the call up or ringing.
//! - `accounts`: the accounts as `status` lists them.
//! - the reads ([`reads`]): through the account the request addresses ([`address`]), awaited on the foreground as
//!   the views await the engine's replies.
//! - the UI commands ([`ui`]): the window, the views it opens, the account it shows and the actions dispatched to it.
//! - the message writes ([`writes`]): through the account addressed, its outbox; its own status as its status menu
//!   chooses it, and `sync`, its catch-up now.
//! - the call commands ([`calls`]): through the account addressed, or the one ringing or in the call, asked as the call
//!   buttons ask.
//! - the calendar commands ([`calendar`]): the account's events through the account addressed, and its meetings
//!   joined as the calendar's Join and the pre-join's Join now join them.
//! - `open-link` ([`links`]): a meeting's join link opened in its pre-join, any other link showing the window.
//! - `subscribe` ([`events`]): answered `subscribed` with the stream the connection writes from then on.
//!
//! A request the app doesn't serve yet is a [`ErrorCode::BadRequest`]; once the app is going, every request is
//! [`ErrorCode::Stopped`].

use gpui::{App, AsyncApp, Entity, WeakEntity};
use teams_ctl::PROTOCOL_VERSION;
use teams_ctl::protocol::{
    AccountRow, AppStatus, CallRow, CtlError, ErrorCode, Hello, Outcome, Request, RequestLine, Response, ResponseLine,
    WindowState,
};
use teams_ui::{Account, Bridge, Desktop, Engine, Link, Status};

use super::address::{self, Addressed};
use super::listener::Answer;
use super::{calendar, calls, events, links, reads, ui, writes};
use crate::app::Running;

/// The app's version, as `hello` and `status` tell it.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `line` answered (see the module docs).
pub async fn answer(running: &WeakEntity<Running>, line: RequestLine, cx: &mut AsyncApp) -> Answer {
    let RequestLine { id, account, request } = line;
    let line = |outcome| ResponseLine { id, outcome };
    if let Request::Subscribe { filter } = request {
        return match events::subscribe(running, account.as_deref(), filter, cx).await {
            Ok(events) => Answer {
                line: line(Outcome::Ok(Response::Subscribed)),
                events: Some(events),
            },
            Err(error) => line(Outcome::Error(error)).into(),
        };
    }
    let outcome = match ask(running, account.as_deref(), &request, cx).await {
        Ok(response) => Outcome::Ok(response),
        Err(error) => Outcome::Error(error),
    };
    line(outcome).into()
}

/// What a request needs once the app's state was read on the foreground.
enum Step {
    /// Answered from that state alone.
    Answered(Result<Response, CtlError>),
    /// A read of the account addressed, awaited.
    Read(Addressed),
    /// A write of the account addressed, awaited.
    Write(Addressed),
    /// A catch-up of the account addressed, awaited.
    Sync(Addressed),
}

async fn ask(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    if ui::serves(request) {
        return ui::answer(running, account, request, cx).await;
    }
    if calls::serves(request) {
        return calls::answer(running, account, request, cx).await;
    }
    if calendar::serves(request) {
        return calendar::answer(running, account, request, cx).await;
    }
    if links::serves(request) {
        return links::answer(running, account, request, cx).await;
    }
    if writes::serves_status(request) {
        return cx.update(|cx| writes::own_status(&alive(running, cx)?, account, request, cx));
    }
    let step = cx.update(|cx| {
        let running = alive(running, cx)?;
        let running = running.read(cx);
        Ok(match request {
            Request::Hello { protocol, .. } => Step::Answered(hello(*protocol)),
            Request::Status => Step::Answered(Ok(Response::Status(status(running, cx)))),
            Request::Accounts => Step::Answered(Ok(Response::Accounts {
                accounts: account_rows(running, cx),
            })),
            Request::Chats { .. }
            | Request::Messages { .. }
            | Request::Unread
            | Request::Search { .. }
            | Request::Members { .. }
            | Request::Activity { .. } => Step::Read(address::account(running, account, cx)?),
            _ if writes::serves(request) => Step::Write(address::account(running, account, cx)?),
            Request::Sync => Step::Sync(address::account(running, account, cx)?),
            _ => Step::Answered(Err(CtlError::new(
                ErrorCode::BadRequest,
                "this app doesn't serve that command yet",
            ))),
        })
    })?;
    match step {
        Step::Answered(answer) => answer,
        Step::Read(addressed) => reads::answer(request, &addressed, cx).await,
        Step::Write(addressed) => writes::answer(request, &addressed, cx).await,
        Step::Sync(addressed) => writes::sync(&addressed, cx).await,
    }
}

/// The app's accounts, unless it is gone or quitting.
pub(super) fn alive(running: &WeakEntity<Running>, cx: &App) -> Result<Entity<Running>, CtlError> {
    let quitting = || CtlError::new(ErrorCode::Stopped, "the app is quitting");
    let running = running.upgrade().ok_or_else(quitting)?;
    if running.read(cx).quitting() {
        return Err(quitting());
    }
    Ok(running)
}

fn hello(protocol: u32) -> Result<Response, CtlError> {
    if protocol != PROTOCOL_VERSION {
        return Err(CtlError::new(
            ErrorCode::VersionMismatch,
            format!("the app speaks protocol {PROTOCOL_VERSION}, the client {protocol}"),
        ));
    }
    Ok(Response::Hello(Hello {
        protocol: PROTOCOL_VERSION,
        app: APP_VERSION.to_owned(),
    }))
}

/// The app as `status` tells it.
fn status(running: &Running, cx: &App) -> AppStatus {
    let call = running
        .bridges()
        .iter()
        .find_map(|(_, upn, bridge)| call_row(upn, bridge.read(cx), cx));
    AppStatus {
        app: APP_VERSION.to_owned(),
        accounts: account_rows(running, cx),
        window: window_state(running.desktop().read(cx)),
        call,
    }
}

/// The app's accounts in their order, as `status` and `accounts` list them.
fn account_rows(running: &Running, cx: &App) -> Vec<AccountRow> {
    let rail = running.desktop().read(cx).accounts().read(cx);
    let shown = rail.active_key();
    running
        .bridges()
        .iter()
        .enumerate()
        .map(|(index, (mri, upn, bridge))| {
            let name = rail
                .tile(mri)
                .map(|tile| tile.name.to_string())
                .filter(|name| !name.is_empty());
            AccountRow {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                upn: upn.clone(),
                mri: mri.clone(),
                name,
                shown: shown == Some(mri.as_str()),
                state: engine_state(bridge.read(cx).models().status.read(cx)).to_owned(),
            }
        })
        .collect()
}

/// An account's engine as `status` names it: signed out and stopped first, otherwise its push channel's link.
fn engine_state(status: &Status) -> &'static str {
    if matches!(status.account, Account::SignedOut { .. }) {
        return "signed_out";
    }
    if status.engine == Engine::Stopped {
        return "stopped";
    }
    match status.link {
        Link::Starting => "starting",
        Link::Connected => "connected",
        Link::Down => "down",
        Link::Stopped => "stopped",
    }
}

fn window_state(desktop: &Desktop) -> WindowState {
    if desktop.window().is_some() {
        WindowState::Shown
    } else if desktop.in_tray() {
        WindowState::Tray
    } else {
        WindowState::Closed
    }
}

/// The call up on account `upn`, or else the invitation that rings there.
pub fn call_row(upn: &str, bridge: &Bridge, cx: &App) -> Option<CallRow> {
    calls::rows(upn, bridge.models().calls.read(cx)).into_iter().next()
}
