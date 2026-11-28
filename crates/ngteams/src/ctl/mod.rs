//! The control socket's server: what `ngteamsctl` talks to (`docs/plans/20261106-ngteamsctl.md`; the protocol is
//! `teams-ctl`'s).
//!
//! It is one more caller of what the window calls: the accounts' `Core`, the desktop and the settings. Nothing in it
//! builds a request, and it opens no store.
//!
//! - [`listener`]: the socket (`0600` in a `0700` directory, a stale one replaced, a live one refused), a thread
//!   accepting connections and a thread per connection reading its lines.
//! - [`dispatch`]: each request answered on GPUI's foreground, over the app's accounts as they run.
//! - [`address`]: the account and the chat a request names.
//! - [`reads`]: the read commands, through the account's `Core`.
//! - [`ui`]: the UI commands (the window, the views it opens, the account it shows) and the actions dispatched to it.
//! - [`writes`]: the message writes, through the account's `Core::write`, its outbox.
//! - [`calls`]: the call commands, asked of the account's calls as the call buttons ask them.
//! - [`calendar`]: the account's calendar, and its meetings joined as the calendar's Join joins them.
//! - [`download`]: the attachments of a message saved in a folder, read by the account's engine.
//! - [`people`]: the people search, one person's profile card and others' presence, through the account's `Core`.
//! - [`links`]: a Teams link opened (a meeting join link's pre-join), what a second launch hands over.
//! - [`events`]: the event stream a `subscribe` turns a connection into, fed by each account's bridge tap,
//! - [`watch`]: by the app's own state where it changes (unread, own status, sign-out, notifications, the window),
//! - and [`ticker`]: by each account's calendar, for the meetings about to start.
//!
//! The requests cross from the connections' threads to the foreground through a channel, where one task takes them in
//! turn and answers each in a task of its own; each answer goes back through a oneshot to the thread that asked (a
//! `subscribe`'s with its stream, which that thread writes from then on). The
//! server serves from the app's launch (`Launch::ctl`) until its quit, which closes every connection and removes the
//! socket file.

pub mod address;
pub mod calendar;
pub mod calls;
pub mod dispatch;
pub mod download;
pub mod events;
pub mod links;
pub mod listener;
pub mod people;
pub mod reads;
pub mod ticker;
pub mod ui;
pub mod watch;
pub mod writes;

use std::path::Path;

use gpui::{App, AsyncApp, WeakEntity};

use crate::app::Running;
pub use listener::{BindError, Listener, bind};

/// Serve the control socket at `path` over the app's accounts (`running`) until [`Listener::stop`].
///
/// # Errors
/// The socket wasn't bound ([`BindError`]: another app serves there, say) or its thread wasn't started.
pub fn serve(path: &Path, running: WeakEntity<Running>, cx: &mut App) -> Result<Listener, BindError> {
    let bound = bind(path)?;
    let (jobs, asked) = async_channel::unbounded::<listener::Job>();
    let listener = bound.serve(jobs).map_err(|source| BindError::Io {
        path: path.to_owned(),
        source,
    })?;
    // ends once every connection's thread and the listener's are gone (the stop), each having dropped its sender
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(job) = asked.recv().await {
            let running = running.clone();
            // each answered by a task of its own: a read awaiting the engine holds up no other connection
            cx.spawn(async move |cx: &mut AsyncApp| {
                let answer = dispatch::answer(&running, job.line, cx).await;
                // the connection may be gone meanwhile: nobody to tell
                let _ = job.reply.send(answer);
            })
            .detach();
        }
    })
    .detach();
    log::info!("the control socket is served");
    Ok(listener)
}
