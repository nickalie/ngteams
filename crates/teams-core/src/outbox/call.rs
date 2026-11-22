//! The outbox's call kind (M3 Task 14): the typed call writes the `teams-call` machine builds go out from here too,
//! each attempt checked against the cross-account call floor, tracked and sent under the transport's rules,
//! but neither as a row nor as a transient: nothing of a call write is
//! stored, checked for, offered to send again or sent by a later run, and none waits in the outbox, is replaced by a
//! newer one or dropped by a message post. Its answer, whatever its status, goes back to the machine with its body
//! (the conversation a start opened, an `attach`'s or an `accept`'s answer), and the machine decides what a failed step
//! means (Task 14's decision with Codex).
//!
//! **One attempt at a time, and at most three.** [`Outbox::try_call`] makes one attempt; [`CallRetries`] says whether
//! another may follow, and only for an answer that says the service didn't act: a 401 gets one more attempt at once
//! (the transport renewed the refused skypetoken), a 429 or 503 (or a throttled token service) one more after its
//! `Retry-After` ([`DEFAULT_RETRY_WAIT`] without one) when that is at most [`MAX_TRANSIENT_WAIT`]; once each, never
//! reset. An outcome that isn't known (a network error, a timeout, a 5xx other than 503) is never followed by another
//! attempt: a call controller acting twice is worse than a failed step. The engine asks the machine before each further
//! attempt (`teams_call::Calls::may_send_again`): a call that ended wants nothing more of it but its cleanup.

use std::time::Duration;

use teams_proto::{DEFAULT_RETRY_WAIT, HttpError, Request, Response, Transport};

use super::{MAX_TRANSIENT_WAIT, Outbox, Tried};

/// How one attempt at a call write went.
#[derive(Debug)]
pub enum CallTried {
    /// The service answered (any status).
    Answered(Response),
    /// Call admission refused it: another account holds the call floor, and nothing went out.
    Denied(String),
    /// Nothing went out, and nothing will (the transport refused it or its credential failed).
    NotSent(String),
    /// Nothing went out: the token service is throttling for this long.
    Throttled(Duration),
    /// Whether it reached the service isn't known.
    Uncertain(String),
}

impl CallTried {
    /// What the attempt came to, for the log: call admission and the service's status, never a body.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Answered(answer) => format!("admitted, HTTP {}", answer.status()),
            Self::Denied(reason) => format!("call admission refused: {reason}"),
            Self::NotSent(why) => format!("not sent: {why}"),
            Self::Throttled(wait) => format!("throttled for {} ms", wait.as_millis()),
            Self::Uncertain(why) => format!("outcome unknown: {why}"),
        }
    }
}

/// The further attempts one call write has used (see the module docs): one after a 401, one after a throttle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallRetries {
    refreshed: bool,
    waited: bool,
}

impl CallRetries {
    /// Whether another attempt may follow `tried`, and after how long: `None` when `tried` is the write's outcome.
    pub fn again(&mut self, tried: &CallTried) -> Option<Duration> {
        let throttled = match tried {
            CallTried::Answered(answer) if answer.status() == 401 => {
                if self.refreshed {
                    return None;
                }
                self.refreshed = true;
                return Some(Duration::ZERO);
            }
            CallTried::Answered(answer) if matches!(answer.status(), 429 | 503) => {
                answer.retry_after().unwrap_or(DEFAULT_RETRY_WAIT)
            }
            CallTried::Throttled(wait) => *wait,
            _ => return None,
        };
        if self.waited || throttled > MAX_TRANSIENT_WAIT {
            return None;
        }
        self.waited = true;
        Some(throttled)
    }
}

impl<T: Transport<Error = HttpError>> Outbox<T> {
    /// A private upload or direct file grant. Each attempt verifies its intrinsic sharing details; unknown outcomes are not retried.
    pub async fn try_attachment(&self, write: teams_proto::DriveWrite) -> CallTried {
        let tried = routed(self.transport.deliver_attachment(write).await, "attachment");
        log::info!("attachment write: {}", tried.summary());
        tried
    }
    /// A channel file's write on its team's site (`crate::attachments`): one attempt, no row, its answer back to the
    /// asker; another attempt only as [`CallRetries`] allows.
    pub async fn try_channel_file(&self, write: teams_proto::ChannelFileWrite) -> CallTried {
        let tried = routed(self.transport.deliver_channel_file(write).await, "channel file");
        log::info!("channel file write: {}", tried.summary());
        tried
    }
    /// A bot invoke ([`crate::bots`]): one attempt, no row; its answer, or why it went nowhere, back to the asker.
    pub async fn try_bot_invoke(&self, invoke: teams_proto::BotInvoke) -> CallTried {
        let tried = routed(self.transport.deliver_bot_invoke(invoke).await, "bot");
        log::info!("bot invoke: {}", tried.summary());
        tried
    }
    /// A caption service operation, kept separate from failures of the voice call.
    pub async fn try_caption(&self, write: teams_proto::captions::CaptionWrite) -> CallTried {
        let tried = routed(self.transport.deliver_caption(write).await, "caption");
        match &tried {
            CallTried::Answered(answer) => log::info!("caption service operation: HTTP {}", answer.status()),
            _ => log::info!("caption service operation did not complete"),
        }
        tried
    }
    /// One attempt at a call write: call-floor admission and tracking, then the transport, which sends it once
    /// (see the module docs).
    pub async fn try_call<R: Request + Send>(&self, write: R) -> CallTried {
        let tried = self.try_once(write).await;
        log::info!("call write: {}", tried.summary());
        tried
    }

    /// One attempt at a calendar write (an event created, changed, cancelled or deleted: `crate::calendar`), as a call
    /// write's: no row, its answer back to the asker, another attempt only as [`CallRetries`] says.
    pub async fn try_calendar<R: Request + Send>(&self, write: R) -> CallTried {
        let tried = self.try_once(write).await;
        log::info!("calendar write: {}", tried.summary());
        tried
    }

    /// One attempt at a picture's write (its object made, its bytes put: `crate::attachments`), as a calendar write's.
    pub async fn try_picture<R: Request + Send>(&self, write: R) -> CallTried {
        let tried = self.try_once(write).await;
        log::info!("picture write: {}", tried.summary());
        tried
    }

    /// One attempt at a new channel's write (`crate::channels`), as a calendar write's.
    pub async fn try_channel<R: Request + Send>(&self, write: R) -> CallTried {
        let tried = self.try_once(write).await;
        log::info!("channel write: {}", tried.summary());
        tried
    }

    /// One attempt at a meeting's options change (its organizer's Controls: `crate::calling`), as a call write's: no
    /// row, its answer back to the asker, another attempt only as [`CallRetries`] says.
    pub async fn try_meeting<R: Request + Send>(&self, write: R) -> CallTried {
        let tried = self.try_once(write).await;
        log::info!("meeting options write: {}", tried.summary());
        tried
    }

    async fn try_once<R: Request + Send>(&self, write: R) -> CallTried {
        match self.deliver(write).await {
            Tried::Answered(answer) => CallTried::Answered(answer),
            Tried::Denied(denied) => CallTried::Denied(denied.clone()),
            Tried::NotSent(why) | Tried::Offline(why) => CallTried::NotSent(why),
            Tried::Throttled { wait, .. } => CallTried::Throttled(wait),
            Tried::Uncertain(why) => CallTried::Uncertain(why),
        }
    }
}

/// A routed write's outcome: `None` when the transport has no route of that `kind`.
fn routed(sent: Option<Result<Response, HttpError>>, kind: &str) -> CallTried {
    match sent {
        None => CallTried::NotSent(format!("This transport has no {kind} route.")),
        Some(Ok(answer)) => CallTried::Answered(answer),
        Some(Err(error)) => match super::from_transport(error) {
            Tried::Answered(answer) => CallTried::Answered(answer),
            Tried::Denied(reason) => CallTried::Denied(reason),
            Tried::NotSent(reason) | Tried::Offline(reason) => CallTried::NotSent(reason),
            Tried::Uncertain(reason) => CallTried::Uncertain(reason),
            Tried::Throttled { wait, .. } => CallTried::Throttled(wait),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use teams_proto::Response;

    use super::{CallRetries, CallTried};

    fn answered(status: u16) -> CallTried {
        CallTried::Answered(Response::new(status, Vec::new()))
    }

    #[test]
    fn a_401_and_a_short_throttle_each_get_one_more_attempt_and_nothing_else_does() {
        let mut retries = CallRetries::default();
        assert_eq!(retries.again(&answered(401)), Some(Duration::ZERO));
        assert_eq!(retries.again(&answered(401)), None, "once");
        let throttled = CallTried::Answered(Response::new(429, Vec::new()).with_retry_after(Duration::from_secs(2)));
        assert_eq!(retries.again(&throttled), Some(Duration::from_secs(2)));
        assert_eq!(retries.again(&answered(503)), None, "once, whichever throttles");

        let mut retries = CallRetries::default();
        assert_eq!(
            retries.again(&answered(503)),
            Some(Duration::from_secs(1)),
            "no Retry-After"
        );
        let mut retries = CallRetries::default();
        let long = CallTried::Answered(Response::new(429, Vec::new()).with_retry_after(Duration::from_secs(6)));
        assert_eq!(retries.again(&long), None, "longer than a call step is worth waiting");
        assert_eq!(
            retries.again(&CallTried::Throttled(Duration::from_millis(500))),
            Some(Duration::from_millis(500))
        );

        for outcome in [
            answered(200),
            answered(500),
            answered(404),
            CallTried::Uncertain("timeout".into()),
            CallTried::Denied("partial".into()),
            CallTried::NotSent("refused".into()),
        ] {
            assert_eq!(CallRetries::default().again(&outcome), None, "{outcome:?}");
        }
    }
}
