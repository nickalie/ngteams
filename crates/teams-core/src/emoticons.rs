//! Reactions' animations, read when a view asks for one (the picker, under the pointer; a call reaction on a tile):
//! Teams' CDN's strip of the reaction's frames, or a call reaction's 3D emoji ([`ReactionAnimation`], a `GET` with no
//! token), the way the web client reads them.
//!
//! - **Kept for the run**: a strip read is kept in memory and answered from there after; a reaction the CDN has none for
//!   (an answer that isn't a success, or isn't a PNG) is kept as missing. A read that failed on the way, or that the
//!   CDN answered with a status that can pass (408, 429, 5xx), isn't kept and is answered
//!   [`EngineError::AnimationUnread`], so the next ask reads again.
//! - **One read per reaction at a time**: asks while its read runs wait for that read.
//! - Nothing is stored and nothing is written.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use teams_proto::{Client, Credentials, HttpError, HttpTransport, ReactionAnimation};

use crate::engine::{EngineError, Responder};

/// An animation strip's PNG bytes.
pub type Strip = Arc<[u8]>;

/// What is known of one reaction's animation.
enum Known {
    /// Read: its strip, or `None` when the CDN has none.
    Read(Option<Strip>),
    /// Its read runs; these wait for it.
    Reading(Vec<Responder<Option<Strip>>>),
}

pub(crate) struct Animations<C> {
    client: Arc<Client<HttpTransport<C>>>,
    known: Rc<RefCell<HashMap<String, Known>>>,
}

impl<C: Credentials + 'static> Animations<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self {
            client,
            known: Rc::default(),
        }
    }

    /// Answers `reply` with the animation `read` reads: at once when it is known, else once its read is over (the read
    /// started now unless one runs). No read (an id no read can name) is answered `None`.
    pub(crate) fn animation(&self, read: Option<ReactionAnimation>, reply: Responder<Option<Strip>>) {
        let Some(read) = read else {
            let _ = reply.send(Ok(None));
            return;
        };
        let id = read.key();
        let mut known = self.known.borrow_mut();
        match known.get_mut(&id) {
            Some(Known::Read(strip)) => {
                let _ = reply.send(Ok(strip.clone()));
            }
            Some(Known::Reading(waiting)) => waiting.push(reply),
            None => {
                known.insert(id.clone(), Known::Reading(vec![reply]));
                // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
                tokio::task::spawn_local(read_animation(
                    Arc::clone(&self.client),
                    read,
                    id,
                    Rc::clone(&self.known),
                ));
            }
        }
    }
}

/// Reads `id`'s animation and answers every ask that waits for it; keeps it, or that there is none, unless the read
/// failed on the way or the CDN failed for now ([`EngineError::AnimationUnread`]).
async fn read_animation<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    read: ReactionAnimation,
    id: String,
    known: Rc<RefCell<HashMap<String, Known>>>,
) {
    let answer = match client.read(&read).await {
        Ok(answer) => Some(read.read_answer(&answer).map(Strip::from)),
        Err(HttpError::Status(answer)) if !matches!(answer.status(), 408 | 429 | 500..=599) => Some(None),
        Err(_) => None,
    };
    let waiting = match answer.clone() {
        Some(strip) => known.borrow_mut().insert(id, Known::Read(strip)),
        None => known.borrow_mut().remove(&id),
    };
    let Some(Known::Reading(waiting)) = waiting else { return };
    for reply in waiting {
        let _ = reply.send(answer.clone().ok_or(EngineError::AnimationUnread));
    }
}
