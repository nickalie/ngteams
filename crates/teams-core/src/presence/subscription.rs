//! The subscription of the account's own Trouter endpoint to the watched people's presence (UPS's
//! `v1/pubsub/subscriptions/<endpoint>`, [`PresenceSubscription`]), kept in step as the web client keeps its own
//! (`docs/protocol/trouter.md`, "Others' presence"):
//!
//! - **When it may go**: only while the push channel is up (its connection's push URL, where UPS delivers) and a
//!   registration since that connect registered the endpoint ([`RegisteredEndpoint`]), with the presence base; one
//!   subscription at a time, never sooner than [`PRESENCE_MIN_GAP`](super::PRESENCE_MIN_GAP) after the last one began.
//! - **In full** (the earlier subscriptions purged, then everyone readable added, [`MAX_NAMED`] per request, the first
//!   one purging): after every registration (a connect's, a reconnect's with its new push URL, the hourly renewal's),
//!   every [`SUBSCRIPTION_RENEWAL`] (the web client's refresh), and once someone subscribed has been no longer watched
//!   (or no longer readable) for [`SUBSCRIPTION_PRUNE`] (the web client's removal of those it no longer tracks), which
//!   so leaves them out rather than naming them again in a removal.
//! - **Additions**: someone newly watched and readable is added at once (no purge).
//! - **What is live**: those UPS took (`20200`) on this connection and registration; anyone else (UPS's polling only, a
//!   failure, not asked yet) is not, and the polling reads them at its minute's pace. A subscription that failed is
//!   tried again after [`SUBSCRIPTION_RETRY`] (or its `Retry-After`, when longer); an answer from before the last
//!   connect, disconnect or registration is dropped.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use teams_proto::Mri;
use teams_proto::{Client, Credentials, HttpError, HttpTransport, PresenceSubscription, RegisteredEndpoint};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{MAX_NAMED, PRESENCE_MIN_GAP};

/// How long a subscription is kept before it is made again in full: the web client's refresh, 55 minutes.
pub const SUBSCRIPTION_RENEWAL: Duration = Duration::from_mins(55);

/// How long someone subscribed and no longer watched stays subscribed before a full subscription leaves them out: the
/// web client's removal of those it no longer tracks, every 6 minutes.
pub const SUBSCRIPTION_PRUNE: Duration = Duration::from_mins(6);

/// The next try after a subscription that failed (or its `Retry-After`, when longer).
pub const SUBSCRIPTION_RETRY: Duration = Duration::from_mins(1);

/// What became of one request of a subscription.
#[derive(Debug)]
enum Outcome {
    /// Answered: whom UPS took.
    Taken(HashSet<String>),
    /// It failed (the network, a status, an answer of another shape or naming anyone not asked for), with how long
    /// UPS said to wait.
    Failed(Option<Duration>),
    /// Not sent: an earlier request of the same subscription failed.
    NotSent,
}

/// A finished subscription: the connection it was for, whether it was in full, and each request's names and outcome.
#[derive(Debug)]
pub(crate) struct Subscribed {
    generation: u64,
    full: bool,
    requests: Vec<(Vec<String>, Outcome)>,
}

/// The subscription's state (see the module docs).
#[derive(Default)]
pub(crate) struct Subscription {
    /// The push URL of the connection up now; `None` while the push channel is down.
    push_url: Option<String>,
    /// The endpoint a registration since that connect registered.
    endpoint: Option<RegisteredEndpoint>,
    /// Counts connects, disconnects and registrations: an answer of an earlier one is dropped.
    generation: u64,
    /// Whom UPS took on this connection and registration.
    live: HashSet<String>,
    /// Whom a subscription of this connection and registration named (taken or not, or under way): not added again
    /// until the next one in full.
    asked: HashSet<String>,
    /// A subscription in full is due.
    full_due: bool,
    /// When the last subscription in full began.
    last_full: Option<Instant>,
    /// Since when someone asked is no longer readable.
    stale_since: Option<Instant>,
    /// No subscription before this: the gap after the last one, or a failed one's wait.
    not_before: Option<Instant>,
    running: JoinSet<Subscribed>,
}

impl Subscription {
    /// A connection is up at `push_url`: nothing is live on it until a registration (then a subscription in full).
    pub(crate) fn connected(&mut self, push_url: String) {
        self.reset();
        self.push_url = Some(push_url);
        self.endpoint = None;
    }

    /// A registration of the endpoint went through: a subscription in full is due.
    pub(crate) fn registered(&mut self, endpoint: RegisteredEndpoint) {
        self.reset();
        self.endpoint = Some(endpoint);
    }

    /// The push channel is down (or stopped): nothing is live, and nothing is subscribed until it is up again.
    pub(crate) fn down(&mut self) {
        self.reset();
        self.push_url = None;
        self.endpoint = None;
    }

    fn reset(&mut self) {
        self.generation += 1;
        self.live.clear();
        self.asked.clear();
        self.full_due = true;
        self.stale_since = None;
        self.not_before = None;
    }

    /// Whether UPS's subscription covers everyone in `readable` (someone at least): then the reads slow down.
    pub(crate) fn covers(&self, readable: &[String]) -> bool {
        !readable.is_empty() && readable.iter().all(|mri| self.live.contains(mri))
    }

    /// Whether a subscription is under way.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The subscription's result once it is over; never while none runs ([`Self::busy`]). One that panicked or was
    /// cancelled is dropped (`None`), what it named asked again later.
    pub(crate) async fn next(&mut self) -> Option<Subscribed> {
        let joined = self.running.join_next().await?;
        if joined.is_err() {
            self.asked.clear();
            self.full_due = true;
        }
        joined.ok()
    }

    /// Starts a subscription of `readable` (the watched a read may name, in the watch's order) when one is due at
    /// `now` (see the module docs).
    pub(crate) fn start_due<C: Credentials + 'static>(
        &mut self,
        client: &Arc<Client<HttpTransport<C>>>,
        readable: &[String],
        now: Instant,
    ) {
        let stale = self.asked.iter().any(|mri| !readable.contains(mri));
        self.stale_since = stale.then(|| self.stale_since.unwrap_or(now));
        let (Some(push_url), Some(endpoint)) = (self.push_url.clone(), self.endpoint.clone()) else {
            return;
        };
        if self.busy() || self.not_before.is_some_and(|at| now < at) || readable.is_empty() {
            return;
        }
        let full = self.full_due
            || self.last_full.is_none_or(|at| now >= at + SUBSCRIPTION_RENEWAL)
            || self.stale_since.is_some_and(|since| now >= since + SUBSCRIPTION_PRUNE);
        let names: Vec<String> = if full {
            self.live.clear();
            self.asked.clear();
            self.full_due = false;
            self.last_full = Some(now);
            self.stale_since = None;
            readable.to_vec()
        } else {
            readable
                .iter()
                .filter(|mri| !self.asked.contains(*mri))
                .cloned()
                .collect()
        };
        if names.is_empty() {
            return;
        }
        self.asked.extend(names.iter().cloned());
        self.not_before = Some(now + PRESENCE_MIN_GAP);
        let generation = self.generation;
        self.running.spawn(subscribe(
            Arc::clone(client),
            endpoint,
            push_url,
            full,
            names,
            generation,
        ));
    }

    /// Keeps what a subscription gave: whom UPS took is live; whom a failed or unsent request named is asked again
    /// later (in full, when the subscription was).
    pub(crate) fn settle(&mut self, subscribed: Subscribed) {
        if subscribed.generation != self.generation {
            return;
        }
        for (names, outcome) in subscribed.requests {
            match outcome {
                Outcome::Taken(taken) => self.live.extend(taken),
                Outcome::Failed(wait) => {
                    let retry = wait.map_or(SUBSCRIPTION_RETRY, |wait| wait.max(SUBSCRIPTION_RETRY));
                    self.not_before = Some(Instant::now() + retry);
                    self.forget(&names, subscribed.full);
                }
                Outcome::NotSent => self.forget(&names, subscribed.full),
            }
        }
    }

    /// `names` weren't subscribed: they are asked again (a subscription in full again, when it was one).
    fn forget(&mut self, names: &[String], full: bool) {
        for mri in names {
            self.asked.remove(mri);
        }
        self.full_due |= full;
    }
}

/// One subscription of `names`: [`MAX_NAMED`] per request, one after the other, the first one purging the endpoint's
/// earlier subscriptions when it is in `full`; after a request that failed, the rest aren't sent.
async fn subscribe<C: Credentials>(
    client: Arc<Client<HttpTransport<C>>>,
    endpoint: RegisteredEndpoint,
    push_url: String,
    full: bool,
    names: Vec<String>,
    generation: u64,
) -> Subscribed {
    let mut requests = Vec::new();
    let mut failed = false;
    for (index, batch) in names.chunks(MAX_NAMED).enumerate() {
        let batch = batch.to_vec();
        if failed {
            requests.push((batch, Outcome::NotSent));
            continue;
        }
        let outcome = subscribe_one(&client, &endpoint, &push_url, full && index == 0, &batch).await;
        failed = matches!(outcome, Outcome::Failed(_));
        requests.push((batch, outcome));
    }
    Subscribed {
        generation,
        full,
        requests,
    }
}

/// One request of a subscription, adding `names`.
async fn subscribe_one<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    endpoint: &RegisteredEndpoint,
    push_url: &str,
    purge: bool,
    names: &[String],
) -> Outcome {
    let added = names.iter().map(|mri| Mri::new(mri.as_str()));
    let Some(request) = PresenceSubscription::new_in(client.transport().cloud(), endpoint, push_url, purge, added, [])
    else {
        return Outcome::Failed(None);
    };
    let answer = match client.lookup(request.clone()).await {
        Ok(answer) => answer,
        Err(HttpError::Status(answer)) => {
            log::debug!("the presence subscription was answered HTTP {}", answer.status());
            let wait = matches!(answer.status(), 429 | 503)
                .then(|| answer.retry_after())
                .flatten();
            return Outcome::Failed(wait);
        }
        Err(error) => {
            // the error names no token and no body
            log::debug!("the presence subscription failed: {error}");
            return Outcome::Failed(None);
        }
    };
    match request.read_answer(answer.body()) {
        Ok(statuses) => {
            let taken: HashSet<String> = statuses
                .into_iter()
                .filter(|status| status.taken && !status.removed)
                .map(|status| status.mri.as_str().to_owned())
                .collect();
            if taken.len() < names.len() {
                log::debug!(
                    "UPS took {} of the {} presence subscriptions asked; the rest are polled",
                    taken.len(),
                    names.len()
                );
            }
            Outcome::Taken(taken)
        }
        Err(error) => {
            log::debug!("the presence subscription's answer wasn't read: {error}");
            Outcome::Failed(None)
        }
    }
}
