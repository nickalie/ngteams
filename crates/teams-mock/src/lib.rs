//! A local Teams chat service for tests (M1 Task 9), built from the synthetic chat fixtures in
//! `tests/fixtures/chatsvc/`, so the whole native client can be tested offline. Its answers have the shapes those
//! fixtures recorded (`docs/protocol/chat.md`, `docs/protocol/chat-service.md`); no capture or reference copy is
//! read, here or by a test.
//!
//! **Reach and use.** A dev-dependency only, never a production client's dependency (a test in this crate checks every
//! workspace manifest). Clients reach it only through the `test-support` mock origins:
//! `teams_proto::HttpTransport::with_mock_origin`,
//! given [`ChatServiceMock::origin`] with [`CHAT_BASE`] as the chat base, so every URL is built and checked as it would
//! be for the real service first.
//!
//! ```text
//! let mock = ChatServiceMock::start().await?;
//! let trouter = mock.start_trouter()?; // its push channel, at trouter.origin() (M1 Task 16)
//! let token = mock.token(LAB_A).ok_or("no account")?; // a's `ic3` token, for the client's credentials
//! let transport = HttpTransport::with_mock_origin(credentials, CHAT_BASE, &mock.origin())?;
//! // … a sends to AB_CHAT through its outbox …
//! let events = mock.take_events(LAB_B); // what b would be told, while no socket of b's takes it
//! ```
//!
//! **The world** it starts with ([`ChatServiceMock::start`]): the lab accounts a, b and c and one outsider, x; the a–b
//! 1:1 chat holding the messages of `messages.json` (an add-member event, plain, edited, quoted and deleted
//! messages); an empty a–c chat (`conversations.json`); an empty a–x chat with the outsider; the a–b–c group chat of
//! `thread-group.json`; and, in a's list only, a team channel and a's notifications stream (`conversations.json`), the
//! stream read-only; the team's other channel, in nobody's list, holding the posts and replies of
//! `channel-messages.json` (M2 Task 16). A channel takes its members' writes as a chat does, and a reply in a post's
//! thread posted to the post's chain (`…/conversations/<channel>;messageid=<post>/messages`) is stored with the
//! channel's messages, its `conversationLink` naming the post. The ids are the constants below, made up (never a real tenant's).
//!
//! **What it serves** (paths under [`CHAT_BASE`]):
//! - reads: the conversation list (`GET v1/users/ME/conversations`, and the sync link it hands out, which names the
//!   caller's MRI), message pages (`GET …/conversations/<id>/messages`, first page, sync and backward links) and the
//!   page around a time (`…/messages/epochTimeStamp/<ms>`), the thread read (`GET v1/threads/<id>`), the member list
//!   (`…/members`) and the filtered view (`…/filtered`, refused as the service refused it);
//! - writes: message posts and typing (`POST …/messages`), edits (`PUT …/messages/<id>`), soft deletes, reactions
//!   (`PUT`/`DELETE …/messages/<id>/properties?name=emotions`), read receipts (`PUT
//!   …/conversations/<id>/properties?name=consumptionhorizon`) and chat creation (`POST v1/threads`). Each changes
//!   the state every later read shows and queues a [`ChatEvent`] for each **other** member of the chat
//!   ([`ChatServiceMock::take_events`]); the writer gets none.
//!
//! **CSA** (`csa.rs`, M2 Task 16): its one read the native client sends, the account's teams and their channels (`GET
//! /api/csa/<region>/api/v3/teams/users/me` with the web client's query), from `tests/fixtures/csa/teams.json`, under
//! the CSA base `https://teams.microsoft.com/api/csa/amer`, taking the account's token alone.
//!
//! **The calendar** (`calendar.rs`): the middle tier's calendar view (`GET /api/mt/part/amer-03/v2.0/me/calendars/
//! default/calendarView` with the web client's query) under [`MIDDLE_TIER_BASE`], taking the account's token alone: the
//! same made-up working week for every account, repeated every week.
//!
//! **The team pages** (`team_pages.rs`): what Manage team and "Your teams and channels" read, made up.
//!
//! **A new channel** (`channels.rs`): the middle tier's standard channel, the provisioning service's private one under
//! [`PROVISIONING_BASE`] (its status, its people added), the user profile service's search of a team's people under
//! [`USER_PROFILE_BASE`]; what was made is kept ([`ChatServiceMock::channel_writes`]) and listed by CSA's teams read.
//!
//! **UPS** (`ups.rs`, M2 Task 19): the account's own availability, its presence endpoint's `PUT` and its manual status
//! (set, or cleared with no body), in the shapes the web client sent, under the presence base [`PRESENCE_BASE`],
//! naming an endpoint the account registered with the registrar here; answered `201` and `200` with no body, as UPS
//! answered, and kept ([`ChatServiceMock::own_availability`]).
//!
//! **Loki** (`loki.rs`), on request ([`ChatServiceMock::serve_loki`]): the persona card service's configuration and
//! the profile card's reads of the people a test gives cards, from `tests/fixtures/loki/persona-card.json`.
//!
//! **Calls** (`calling.rs`, M3 Task 13): the flight proxy's call requests under `/api/v2/` with the account's
//! skypetoken ([`ChatServiceMock::skypetoken`]): the start, the callee's `attach`, `progress`, endpoint state, `accept`
//! and `reject`, the acknowledgement, the endpoint metadata, `add`, `leave` and the join of a running call, each
//! answered in its captured shape; the invitations, acceptances and ends they cause delivered to the calling endpoints
//! the accounts registered, through the mock's Trouter; calls a caller the mock plays places
//! ([`ChatServiceMock::ring`]), their ring's end and their hang-up; every call's record ([`ChatServiceMock::calls`]).
//! The TURN relay's credentials (`GET /trap/tokens`, with the account's token). With the `media` feature and the media
//! turned on ([`ChatServiceMock::start_media`], M3 Task 14), a loopback media peer stands behind the answers and offers
//! of the parties the mock plays (the test call's media controller, a group call's mixer, a caller of its own) and
//! their renegotiations, on a loopback TURN server whose credentials `/trap/tokens` then hands out (`peers.rs`).
//!
//! **Trouter** ([`ChatServiceMock::start_trouter`], `trouter.rs`): connect, authenticate, `trouter.connected`, pings,
//! the deliveries of the queued events and their replay on connect, from the synthetic `tests/fixtures/trouter/`
//! fixtures; the registrar's `POST /registrar/prod/V2/registrations` is served on this origin.
//!
//! **Headers** (M1 Task 3, `docs/protocol/auth.md`): every request must carry `Authorization: Bearer <token>` with a
//! token the mock handed out ([`ChatServiceMock::token`]), plus `X-MS-Migration: True` and `BehaviorOverride:
//! redirectAs404`, the web client's set the native transport sends. One exception: the thread read
//! (`GET v1/threads/<id>`) takes the token alone, the captured form of the member read the service answered in
//! full. Anything short of that is a 401 with no body (no 401 of the chat service was ever
//! captured, so its body isn't known).
//!
//! **What it doesn't know** it doesn't make up: a request it doesn't model is a 404, a malformed body of one it
//! models a 400, a write by someone who isn't a member (or to a read-only conversation, or an edit or delete of
//! someone else's message) a 403, all with no body. The service's answers to those weren't captured. Its own links
//! (`syncState`, `backwardLink`) carry opaque tokens of its own: the real ones are opaque too, and a client follows
//! them as full URLs. A 1:1 chat's thread read wasn't kept: it takes the group chat's (`thread-group.json`, its
//! `capabilities` among them) with the `uniquerosterthread` the conversation list gives the chat; its `fixedRoster`
//! and its own capabilities aren't known, so they aren't made up.

mod calendar;
mod calling;
mod channels;
mod chatsvc;
mod csa;
mod fixtures;
mod front;
mod loki;
#[cfg(feature = "media")]
pub mod media;
mod meeting_options;
#[cfg(feature = "media")]
pub mod mixer;
mod peers;
mod reads;
mod roster;
mod startup;
mod team_apps;
mod team_pages;
mod trouter;
mod ups;
mod world;
mod writes;

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use wiremock::matchers::{any, method};
use wiremock::{Match, Mock, MockServer, Request, Respond, ResponseTemplate};

pub use calling::{ANSWERED_ELSEWHERE, CallPhase, Leg, MockCall, TIMED_OUT};
pub use channels::{ChannelWrite, NOT_ADDED};
pub use fixtures::FixtureError;
pub use front::Front;
pub use loki::{LOKI_REGION, LokiCard};
pub use startup::{Hold, MAX_HOLD, synthetic_chat, synthetic_text, synthetic_topic};
pub use trouter::{CALLING_APP, MESSAGING_APP, REGIONAL_NAME, Registration, SocketRecord, TrouterMock};
pub use ups::{AvailabilityChange, OwnAvailability};

/// The region of the mock's chat base.
pub const REGION: &str = "amer";

/// The chat base a client is built with (`chatServiceAfd` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";

/// The CSA base a client is given (`chatSvcAggAfd` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const CSA_BASE: &str = "https://teams.microsoft.com/api/csa/amer";

/// The middle-tier base a client is given (`middleTier` as the region's GTMs give it); its calendar view
/// (`calendar.rs`) is served at [`ChatServiceMock::origin`] with this base's path.
pub const MIDDLE_TIER_BASE: &str = "https://teams.microsoft.com/api/mt/part/amer-03";

/// The provisioning service's base a client is given (`teamsAndChannelsProvisioningService` as the region's GTMs give
/// it); its requests go to [`ChatServiceMock::origin`] with this base's path (`channels.rs`).
pub const PROVISIONING_BASE: &str = "https://teams.microsoft.com/fabric/amer/templates/api";

/// The user profile service's base a client is given (`userProfileService` as the region's GTMs give it); its requests go
/// to [`ChatServiceMock::origin`] with this base's path (`channels.rs`).
pub const USER_PROFILE_BASE: &str = "https://teams.microsoft.com/api/userprofilesvc/amer";

/// The AMS base a client is given (`amsV2` as the region's GTMs give it: the async gateway); its requests go to
/// [`ChatServiceMock::origin`] with this base's path, where [`ChatServiceMock::serve_bytes`] answers them.
pub const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";

/// The presence base a client is given (`presenceUPS` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";

/// The regional host the service names new resources on in a 201's `Location` (`docs/protocol/chat-service.md`).
pub const REGIONAL_HOST: &str = "https://amer.ng.msg.teams.microsoft.com";

/// The made-up tenant every account and chat belongs to.
pub const TENANT: &str = "00000000-0000-4000-8000-000000000001";

/// Lab account a.
pub const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
/// Lab account b.
pub const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
/// Lab account c.
pub const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
/// An additional synthetic account, in a 1:1 chat with a ([`AX_CHAT`]).
pub const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";

/// The echo service (a bot): the test call adds it.
pub const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";

/// The a–b 1:1 chat, holding the messages of `messages.json`.
pub const AB_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// The a–c 1:1 chat, empty.
pub const AC_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The 1:1 chat of a and the outsider, empty.
pub const AX_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
/// The a–b–c group chat of `thread-group.json`, empty.
pub const ABC_CHAT: &str = "19:00000000000000000000000000000abc@thread.v2";

/// A team channel in a's list. It is the team's id and its general channel's (`csa/teams.json`).
pub const CHANNEL: &str = "19:channel0001@thread.tacv2";
/// The team's other channel (`csa/teams.json`, and the team's `topics` in `conversations.json`): a, b and c are its
/// members, it holds the posts and replies of `channel-messages.json`, it is in nobody's conversation list (the chat
/// service lists a team's general channel only), and its thread is a `topic`.
pub const TEAM_CHANNEL: &str = "19:channel0002@thread.tacv2";
/// a's notifications stream: read-only.
pub const NOTIFICATIONS: &str = "48:notifications";

/// What a write did, queued for each other member of its chat, until the mock's Trouter ([`TrouterMock`]) delivers it
/// to one of the member's registered endpoints (a message, typing, an edit, a delete, a reaction; read receipts and
/// chat creation stay queued: no capture showed their delivery to another member).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatEvent {
    /// The chat it happened in.
    pub conversation: String,
    /// The writer's MRI.
    pub from: String,
    pub kind: EventKind,
    /// The message as a read gives it after the write ([`EventKind::Message`], [`EventKind::Edit`],
    /// [`EventKind::Delete`], [`EventKind::Reaction`]); the thread read for [`EventKind::ChatCreated`];
    /// `{"consumptionhorizon": …}` for [`EventKind::ReadReceipt`]; `{"messagetype": …}` for [`EventKind::Typing`].
    pub resource: Value,
}

/// The kind of a [`ChatEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Message,
    Typing,
    Edit,
    Delete,
    Reaction,
    ReadReceipt,
    ChatCreated,
}

/// The running mock: a local HTTP server on loopback and the state its answers come from. Dropping it stops serving.
pub struct ChatServiceMock {
    server: MockServer,
    state: Arc<Mutex<world::State>>,
}

/// Matches a request whose path (still percent-encoded) ends with the string held.
struct PathEnds(String);

impl Match for PathEnds {
    fn matches(&self, request: &Request) -> bool {
        request.url.path().ends_with(&self.0)
    }
}

impl std::fmt::Debug for ChatServiceMock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatServiceMock")
            .field("origin", &self.server.uri())
            .finish_non_exhaustive()
    }
}

impl ChatServiceMock {
    /// Starts the mock with its world (see the crate docs) on a free loopback port.
    ///
    /// # Errors
    /// When an embedded fixture doesn't have the shape the mock reads from it.
    pub async fn start() -> Result<Self, FixtureError> {
        let state = Arc::new(Mutex::new(world::State::lab()?));
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(chatsvc::ChatService::new(Arc::clone(&state)))
            .mount(&server)
            .await;
        Ok(Self { server, state })
    }

    /// The origin clients send to (`http://127.0.0.1:<port>`), for the `test-support` mock origins.
    #[must_use]
    pub fn origin(&self) -> String {
        self.server.uri()
    }

    /// The bearer token the mock takes as `mri`'s (an `ic3` token), if `mri` is one of its accounts.
    #[must_use]
    pub fn token(&self, mri: &str) -> Option<String> {
        self.lock().account(mri).map(|account| account.token.clone())
    }

    /// Replaces `mri`'s token: from now on the old one gets a 401. `false` when `mri` isn't one of its accounts.
    pub fn set_token(&self, mri: &str, token: impl Into<String>) -> bool {
        self.lock().set_token(mri, token.into())
    }

    /// Starts the mock's Trouter over this mock's world, on a free loopback port of its own: it delivers the events
    /// queued here to the sockets of registered endpoints (see [`TrouterMock`]), and the registrar it needs is served
    /// on this mock's own origin.
    ///
    /// # Errors
    /// When no port can be bound, or an embedded Trouter fixture doesn't have the shape the mock reads.
    pub fn start_trouter(&self) -> std::io::Result<TrouterMock> {
        TrouterMock::start(Arc::clone(&self.state))
    }

    /// Takes the events queued for `mri` so far, oldest first; the queue is empty afterwards.
    #[must_use]
    pub fn take_events(&self, mri: &str) -> Vec<ChatEvent> {
        self.lock().events.remove(mri).unwrap_or_default()
    }

    /// Queues `event` for `mri` as the service would for one of its endpoints: the writer's own echo of its message,
    /// say, which the mock never queues by itself. It is delivered as any queued event is (see [`TrouterMock`]).
    pub fn queue_event(&self, mri: &str, event: ChatEvent) {
        let mut world = self.lock();
        world.events.entry(mri.to_owned()).or_default().push(event);
        world.changed();
    }

    /// A new group chat of `members` (its creator first) called `topic`, with no messages yet, as the conversation list
    /// then gives it; its id, or `None` when the fixtures have no empty chat to make its list entry from.
    #[must_use = "None says no chat was made"]
    pub fn add_group(&self, topic: &str, members: &[&str]) -> Option<String> {
        let mut state = self.lock();
        let empty = state.fixtures.entries.get(1).cloned()?;
        let id = state.new_group_id();
        let time = state.tick();
        let mut conversation = world::created(&empty, &id, world::Kind::Group, members, time);
        conversation.entry["threadProperties"]["topic"] = serde_json::json!(topic);
        state.conversations.push(conversation);
        Some(id)
    }

    /// `from` posts `html` (a rich-text message) to `chat`, as one of its clients would: stored as a read gives it back
    /// and queued for the chat's other members, as any post. Its id, or `None` when `from` may not write there.
    #[must_use = "None says the post was refused"]
    pub fn post_as(&self, from: &str, chat: &str, html: &str) -> Option<String> {
        writes::post_text(&mut self.lock(), from, chat, html)
    }

    /// Changes the message of `conversation` that carries `client_message_id` as `change` says, as every later read
    /// gives it (a test's stand-in for an entry a client can't read); its `version` is whatever `change` leaves, so a
    /// sync link already past it doesn't bring it again. `false` when there is no such message.
    pub fn change_message(&self, conversation: &str, client_message_id: &str, change: impl FnOnce(&mut Value)) -> bool {
        let mut world = self.lock();
        let message = world.conversation_mut(conversation).and_then(|conversation| {
            conversation
                .messages
                .iter_mut()
                .find(|message| message["clientmessageid"].as_str() == Some(client_message_id))
        });
        let Some(message) = message else {
            return false;
        };
        change(message);
        true
    }

    /// `from` (an app's bot too: `28:<app id>`, named `name`) posts a message of `message_type` with `content` to
    /// `conversation`, stored as its newest entry as a read gives it back and queued for its members: a test's
    /// stand-in for a post no client of the mock makes, such as a workflow's card in a channel. Its id, or `None` when
    /// there is no such conversation.
    #[must_use = "None says no message was added"]
    pub fn add_message(
        &self,
        conversation: &str,
        (from, name): (&str, &str),
        message_type: &str,
        content: &str,
    ) -> Option<String> {
        let mut world = self.lock();
        let arrival = world.tick();
        let sequence = world.conversation(conversation)?.next_sequence();
        let message = serde_json::json!({
            "sequenceId": sequence,
            "conversationid": conversation,
            "conversationLink": format!("{CHAT_BASE}/v1/users/ME/conversations/{conversation}"),
            "type": "Message",
            "id": arrival.to_string(),
            "version": arrival.to_string(),
            "clientmessageid": (9_000_000_000_000_000_000_u64 + arrival).to_string(),
            "messagetype": message_type,
            "contenttype": "Text",
            "content": content,
            "from": writes::contact(from),
            "imdisplayname": name,
            "composetime": world::iso_time(arrival, 7),
            "originalarrivaltime": world::iso_time(arrival, 7),
            "properties": {},
        });
        let stored = world.conversation_mut(conversation)?;
        stored.messages.push(message.clone());
        stored.version = arrival;
        world.queue(conversation, from, EventKind::Message, &message);
        Some(arrival.to_string())
    }

    /// An item in `owner`'s activity feed ([`NOTIFICATIONS`]): its `activity` as the service's feed carries it (made up
    /// by the test: `activityType`, `sourceUserId`, …), read or not, stored as the feed's newest entry and queued for
    /// `owner`, as a new item reaches the feed live. Its id, or `None` when the mock has no feed.
    #[must_use = "None says no item was added"]
    pub fn add_activity(&self, owner: &str, activity: serde_json::Value, read: bool) -> Option<String> {
        let mut world = self.lock();
        let arrival = world.tick();
        let sequence = world.conversation(NOTIFICATIONS)?.next_sequence();
        let message = serde_json::json!({
            "sequenceId": sequence,
            "conversationid": NOTIFICATIONS,
            "conversationLink": format!("{CHAT_BASE}/v1/users/ME/conversations/{NOTIFICATIONS}"),
            "type": "Message",
            "id": arrival.to_string(),
            "version": arrival.to_string(),
            "clientmessageid": (9_100_000_000_000_000_000_u64 + arrival).to_string(),
            "messagetype": "Text",
            "contenttype": "text",
            "content": "",
            "from": writes::contact(owner),
            "imdisplayname": "",
            "composetime": world::iso_time(arrival, 7),
            "originalarrivaltime": world::iso_time(arrival, 7),
            "properties": {"activity": activity, "isread": read, "s2spartnername": "skypespaces"},
        });
        let stored = world.conversation_mut(NOTIFICATIONS)?;
        stored.messages.push(message.clone());
        stored.version = arrival;
        // the service tells the feed's owner: no member is its sender
        world.queue(NOTIFICATIONS, "", EventKind::Message, &message);
        Some(arrival.to_string())
    }

    /// Adds a copy of the message of `conversation` that carries `client_message_id`, changed as `change` says, as
    /// the conversation's newest entry, the original kept as it is (a test's stand-in for a second entry of one
    /// message, one a client can't read, met by the same read as the readable one). `false` when there is no such
    /// message.
    pub fn copy_message(&self, conversation: &str, client_message_id: &str, change: impl FnOnce(&mut Value)) -> bool {
        let mut world = self.lock();
        let Some(conversation) = world.conversation_mut(conversation) else {
            return false;
        };
        let Some(mut copy) = conversation
            .messages
            .iter()
            .find(|message| message["clientmessageid"].as_str() == Some(client_message_id))
            .cloned()
        else {
            return false;
        };
        change(&mut copy);
        conversation.messages.push(copy);
        true
    }

    /// Answers the next request of `http_method` whose path ends with `path_end` with `status` and no body, before the
    /// world sees it: a test's stand-in for a write the service refuses (the world doesn't change and nobody is told).
    /// Every later request is answered as before.
    pub async fn refuse_next(&self, http_method: &str, path_end: &str, status: u16) {
        Mock::given(method(http_method))
            .and(PathEnds(path_end.to_owned()))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// [`Self::refuse_next`], its answer held back for `delay`: a service that is slow to refuse.
    pub async fn refuse_next_after(&self, http_method: &str, path_end: &str, status: u16, delay: std::time::Duration) {
        Mock::given(method(http_method))
            .and(PathEnds(path_end.to_owned()))
            .respond_with(ResponseTemplate::new(status).set_delay(delay))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// Answers the next request of `http_method` whose path ends with `path_end` as the world would, held back for
    /// `delay`: a service that is slow to answer (a call's relay credentials, while its media is being set up). Every
    /// later request is answered at once.
    pub async fn answer_next_after(&self, http_method: &str, path_end: &str, delay: std::time::Duration) {
        let world = chatsvc::ChatService::new(Arc::clone(&self.state));
        Mock::given(method(http_method))
            .and(PathEnds(path_end.to_owned()))
            .respond_with(move |request: &Request| world.respond(request).set_delay(delay))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// Answers the next request of `http_method` whose path ends with `path_end` with `status` and the JSON `body`,
    /// before the world sees it: a test's stand-in for an answer the client can't read (the world doesn't change).
    pub async fn answer_next(&self, http_method: &str, path_end: &str, status: u16, body: &Value) {
        Mock::given(method(http_method))
            .and(PathEnds(path_end.to_owned()))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// Answers every `GET` of exactly `path` with `body` as `content_type`, before the world sees it: a test's stand-in
    /// for a static file (a message's picture on AMS, an emoticon's animation on Teams' CDN), which the world has none
    /// of.
    pub async fn serve_bytes(&self, path: &str, content_type: &str, body: Vec<u8>) {
        Mock::given(method("GET"))
            .and(wiremock::matchers::path(path))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// Answers a pasted picture's send as the async gateway did (`docs/protocol/chat.md`, "Pictures sent"): every
    /// object's creation (`POST /v1/objects/`) `201` with `{"id": object}`, and its bytes put into it
    /// (`PUT /v1/objects/<object>/content/imgpsh`) `201`; then a `GET` of its `imgo` view answers `png`.
    pub async fn serve_picture_sends(&self, object: &str, png: Vec<u8>) {
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/v1/objects/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": object })))
            .with_priority(1)
            .mount(&self.server)
            .await;
        Mock::given(method("PUT"))
            .and(wiremock::matchers::path(format!("/v1/objects/{object}/content/imgpsh")))
            .respond_with(ResponseTemplate::new(201))
            .with_priority(1)
            .mount(&self.server)
            .await;
        self.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), "image/png", png)
            .await;
    }

    /// Answers every `GET` of exactly `path` with the part of `body` its `Range: bytes=<first>-<last>` asks for, as a
    /// `206` with its `Content-Range` (the whole of it as a `200` without one), before the world sees it: a test's
    /// stand-in for a shared file at the download link Graph hands out, which the world has none of. Below the answers
    /// [`Self::answer_next`] mounts, so a test can have the host refuse the next read.
    pub async fn serve_ranged(&self, path: &str, content_type: &str, body: Vec<u8>) {
        let content_type = content_type.to_owned();
        Mock::given(method("GET"))
            .and(wiremock::matchers::path(path))
            .respond_with(move |request: &Request| {
                let total = body.len();
                let asked = request
                    .headers
                    .get("range")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("bytes="))
                    .and_then(|value| value.split_once('-'))
                    .and_then(|(first, last)| Some((first.parse::<usize>().ok()?, last.parse::<usize>().ok()?)));
                match asked {
                    None => ResponseTemplate::new(200).set_body_raw(body.clone(), &content_type),
                    // A malformed reversed range must not panic the loopback server while slicing the body.
                    Some((first, last)) if first >= total || last < first => ResponseTemplate::new(416),
                    Some((first, last)) => {
                        let last = last.min(total - 1);
                        ResponseTemplate::new(206)
                            .insert_header("content-range", format!("bytes {first}-{last}/{total}"))
                            .set_body_raw(body[first..=last].to_vec(), &content_type)
                    }
                }
            })
            .with_priority(2)
            .mount(&self.server)
            .await;
    }

    /// Answers every presence read (`POST` of UPS's `v1/presence/getpresence/` under [`PRESENCE_BASE`]) before the
    /// world sees it, in the shape of `tests/fixtures/ups/getpresence-answer.json`: one item per MRI asked, with the
    /// availability `presences` gives it, `Offline` for anyone else. A test's stand-in: the world keeps no one's
    /// presence.
    pub async fn serve_presence(&self, presences: &[(&str, &str)]) {
        let presences: Vec<(String, String)> = presences
            .iter()
            .map(|(mri, availability)| ((*mri).to_owned(), (*availability).to_owned()))
            .collect();
        let path = format!(
            "{}/v1/presence/getpresence/",
            PRESENCE_BASE.trim_start_matches("https://teams.microsoft.com")
        );
        Mock::given(method("POST"))
            .and(wiremock::matchers::path(path))
            .respond_with(move |request: &Request| {
                let asked: Vec<Value> = serde_json::from_slice(&request.body).unwrap_or_default();
                let items: Vec<Value> = asked
                    .iter()
                    .filter_map(|ask| ask["mri"].as_str())
                    .map(|mri| {
                        let availability = presences
                            .iter()
                            .find(|(known, _)| known == mri)
                            .map_or("Offline", |(_, availability)| availability.as_str());
                        serde_json::json!({
                            "mri": mri,
                            "presence": {
                                "sourceNetwork": "SameEnterprise",
                                "calendarData": {"isOutOfOffice": false},
                                "capabilities": [],
                                "availability": availability,
                                "activity": availability,
                                "lastActiveTime": "0001-01-01T00:00:00Z",
                            },
                            "etagMatch": false,
                            "etag": "made-up-tag",
                            "status": 20000,
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_raw(Value::Array(items).to_string(), "application/json")
            })
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// Serves Loki, the persona card service (`loki.rs`), before the world sees it: its configuration (naming
    /// [`LOKI_REGION`]) and, for each of `cards`, the person, organization, working-with, schedule and out-of-office
    /// reads of their object id, in the shapes of `tests/fixtures/loki/persona-card.json` (the people each works with:
    /// the other cards' people). A person without a card isn't served (the world
    /// doesn't know the read). A test's stand-in: the world keeps no
    /// one's card.
    pub async fn serve_loki(&self, cards: &[LokiCard<'_>]) {
        loki::mount(&self.server, cards).await;
    }

    /// Answers every presence subscription (`POST` of UPS's `v1/pubsub/subscriptions/<endpoint>` under
    /// [`PRESENCE_BASE`]) before the world sees it, in the shape of `tests/fixtures/ups/subscription.json`: a body of
    /// exactly its four keys is answered with one status per MRI it adds or removes, `20200` (taken) but for those in
    /// `polling_only` (`30010`, which the web client polls instead); any other body is a `400`. The mock delivers
    /// nothing for it ([`TrouterMock::deliver_presence`] does). A test reads what was asked from
    /// [`ChatServiceMock::received_requests`].
    pub async fn serve_presence_subscriptions(&self, polling_only: &[&str]) {
        let polling_only: Vec<String> = polling_only.iter().map(|mri| (*mri).to_owned()).collect();
        let prefix = format!(
            "{}/v1/pubsub/subscriptions/",
            PRESENCE_BASE.trim_start_matches("https://teams.microsoft.com")
        );
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(format!("^{prefix}[0-9a-f-]+$")))
            .respond_with(move |request: &Request| subscription_answer(&request.body, &polling_only))
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// The skypetoken the mock's calling service takes as `mri`'s (`X-Skypetoken`), if `mri` is one of its accounts.
    #[must_use]
    pub fn skypetoken(&self, mri: &str) -> Option<String> {
        self.lock().account(mri).map(|account| {
            account
                .skypetoken
                .clone()
                .unwrap_or_else(|| calling::skypetoken_of(&account.mri))
        })
    }

    /// Installs an explicit synthetic calling credential for an existing mock account.
    /// The default remains the opaque fixture credential until a test replaces it.
    pub fn set_skypetoken(&self, mri: &str, token: impl Into<String>) -> bool {
        let mut state = self.lock();
        let Some(account) = state.accounts.iter_mut().find(|account| account.mri == mri) else {
            return false;
        };
        account.skypetoken = Some(token.into());
        true
    }

    /// A caller the mock plays (`from`, no client of its own) rings `to` in their 1:1 call, on every calling endpoint
    /// `to` registered: the call's id.
    #[must_use = "the call's id names it"]
    pub fn ring(&self, from: &str, to: &str) -> String {
        calling::ring(&mut self.lock(), from, &[to.to_owned()], None)
    }

    /// A caller the mock plays (`from`) rings `callees` in the group chat `chat`: the call's id.
    #[must_use = "the call's id names it"]
    pub fn ring_group(&self, from: &str, chat: &str, callees: &[&str]) -> String {
        let callees: Vec<String> = callees.iter().map(|callee| (*callee).to_owned()).collect();
        calling::ring(&mut self.lock(), from, &callees, Some(chat.to_owned()))
    }

    /// Call `id`'s ring ends with `code` and `phrase` (its caller cancelled, it timed out: see [`TIMED_OUT`]). `false`
    /// when there is no such call.
    #[must_use = "false says there was no such call"]
    pub fn end_ring(&self, id: &str, code: i64, phrase: &str) -> bool {
        calling::end_ring(&mut self.lock(), id, code, phrase)
    }

    /// Call `id` is ended by the party the mock plays: everyone in it gets its end. `false` when there is no such call.
    #[must_use = "false says there was no such call"]
    pub fn hang_up_call(&self, id: &str) -> bool {
        calling::hang_up(&mut self.lock(), id)
    }

    /// The calling service's calls so far, oldest first.
    #[must_use]
    pub fn calls(&self) -> Vec<MockCall> {
        self.lock().calling.calls.clone()
    }

    /// The next acceptance the media controller or a group call's mixer delivers names another call in its links (an
    /// acceptance the call tracker cannot bind).
    pub fn unbind_next_acceptance(&self) {
        self.lock().calling.unbind_next_acceptance = true;
    }

    /// The next call operation whose path ends with `segment` (`accept`, `join`'s conversation, …) is answered `delay`
    /// later; the world changes at once, as the service's would (the caller is told, the callee waits for its answer).
    pub fn delay_next_call_op(&self, segment: &str, delay: std::time::Duration) {
        self.lock().calling.delay_next = Some((segment.to_owned(), delay));
    }

    /// The peer of call `id` renegotiates its media with `account`, which answered the call (the media on, see
    /// [`ChatServiceMock::start_media`]). `false` without such a call, answer or peer.
    #[must_use = "false says nothing was renegotiated"]
    pub fn renegotiate(&self, id: &str, account: &str) -> bool {
        calling::renegotiate(&mut self.lock(), id, account)
    }

    /// The peer of call `id` renegotiates with `account` as [`Self::renegotiate`] does, offering its video: its camera
    /// (the test pattern) on. `false` without such a call, answer or peer.
    #[must_use = "false says nothing was renegotiated"]
    pub fn renegotiate_video(&self, id: &str, account: &str) -> bool {
        calling::renegotiate_video(&mut self.lock(), id, account)
    }

    /// The peer of call `id` renegotiates with `account` as [`Self::renegotiate`] does, sharing its screen (`on`: the
    /// file backend's 720p test pattern on a sharing line of its own, as a 1:1 call's far end shares) or no longer.
    /// `false` without such a call, answer or peer (or, stopping, a share).
    #[must_use = "false says nothing was renegotiated"]
    pub fn renegotiate_share(&self, id: &str, account: &str, on: bool) -> bool {
        calling::renegotiate_share(&mut self.lock(), id, account, on)
    }

    /// Turns the calling service's media on (see the crate docs): a TURN server on loopback, whose UDP address it
    /// returns (the clients' relay) and whose credentials `/trap/tokens` hands out from now on, and a loopback peer
    /// behind each party the mock plays, playing `tone` and recording what it hears into `dir`.
    ///
    /// # Errors
    /// When the TURN server can't bind its ports.
    #[cfg(feature = "media")]
    pub fn start_media(
        &self,
        dir: &std::path::Path,
        tone: teams_media::tone::Tone,
    ) -> std::io::Result<std::net::SocketAddr> {
        // libwebrtc started before the first peer: its start would hold up the mock's own runtime
        teams_media::rtc::warm_up();
        self.lock().peers.start(dir, tone)
    }

    /// Turns the calling service's media on as [`Self::start_media`] does, a group call's parties each with a mixer of
    /// its own ([`mixer::MixerPeer`]): its lines' video chosen by source requests over the data channel, its roster's
    /// sources the mixer's.
    ///
    /// # Errors
    /// When the TURN server can't bind its ports.
    #[cfg(feature = "media")]
    pub fn start_group_video(
        &self,
        dir: &std::path::Path,
        tone: teams_media::tone::Tone,
    ) -> std::io::Result<std::net::SocketAddr> {
        // libwebrtc started before the first peer: its start would hold up the mock's own runtime
        teams_media::rtc::warm_up();
        self.lock().peers.start_group_video(dir, tone)
    }

    /// What the mixer of call `id` on `account`'s side saw and did so far (group video on).
    #[cfg(feature = "media")]
    #[must_use]
    pub fn mixer_stats(&self, id: &str, account: &str) -> Option<mixer::MixerStats> {
        self.lock().peers.mixer_stats(id, account)
    }

    /// Sends a synthetic service packet over the existing mixer's real data channel.
    #[cfg(feature = "media")]
    #[must_use]
    pub fn send_call_data(&self, id: &str, account: &str, packet: Vec<u8>) -> bool {
        self.lock()
            .peers
            .known_mixer(id, account)
            .is_some_and(|mixer| mixer.send_channel(packet))
    }

    /// The mixer of call `id` on `account`'s side tells the dominant speakers, newest first: the audio sources of
    /// `speakers`' mixers. `false` without such a mixer.
    #[cfg(feature = "media")]
    #[must_use = "false says nobody was told"]
    pub fn speak(&self, id: &str, account: &str, speakers: &[&str]) -> bool {
        let state = self.lock();
        let sources: Vec<i64> = speakers
            .iter()
            .filter_map(|speaker| state.mixer_sources(id, speaker).map(|(audio, _)| audio))
            .collect();
        state
            .peers
            .known_mixer(id, account)
            .map(|mixer| mixer.speak(sources))
            .is_some()
    }

    /// Turns the calling service's media on as [`Self::start_media`] does, each peer the echo service's stand-in: it
    /// plays back what it hears `delay` later ([`media::EchoDevices`]) and records it into `dir`.
    ///
    /// # Errors
    /// When the TURN server can't bind its ports.
    #[cfg(feature = "media")]
    pub fn start_echo_media(
        &self,
        dir: &std::path::Path,
        delay: std::time::Duration,
    ) -> std::io::Result<std::net::SocketAddr> {
        // libwebrtc started before the first peer: its start would hold up the mock's own runtime
        teams_media::rtc::warm_up();
        self.lock().peers.start_echo(dir, delay)
    }

    /// Where the peer of call `id` on `account`'s side records (once the media is on).
    #[cfg(feature = "media")]
    #[must_use]
    pub fn peer_recording(&self, id: &str, account: &str) -> Option<std::path::PathBuf> {
        self.lock().peers.recording(id, account)
    }

    /// The counts so far of the peer of call `id` on `account`'s side.
    #[cfg(feature = "media")]
    #[must_use]
    pub fn peer_stats(&self, id: &str, account: &str) -> Option<teams_media::session::SessionStats> {
        self.lock().peers.stats(id, account)
    }

    /// Hangs up and closes the peer of call `id` on `account`'s side: its final counts.
    #[cfg(feature = "media")]
    #[must_use]
    pub fn close_peer(&self, id: &str, account: &str) -> Option<teams_media::session::SessionStats> {
        self.lock().peers.close(id, account)
    }

    /// The channel writes the mock took, in order (`channels.rs`).
    #[must_use]
    pub fn channel_writes(&self) -> Vec<ChannelWrite> {
        self.lock().channels.writes.clone()
    }

    /// The own-availability writes UPS took, in order (see the crate docs).
    #[must_use]
    pub fn own_availability(&self) -> Vec<OwnAvailability> {
        self.lock().availability.clone()
    }

    /// Every request the mock got, in order (to check what a client sent, and how often).
    pub async fn received_requests(&self) -> Vec<Request> {
        self.server.received_requests().await.unwrap_or_default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, world::State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// UPS's answer to a presence subscription's `body` (see [`ChatServiceMock::serve_presence_subscriptions`]).
fn subscription_answer(body: &[u8], polling_only: &[String]) -> ResponseTemplate {
    const KEYS: [&str; 4] = [
        "trouterUri",
        "shouldPurgePreviousSubscriptions",
        "subscriptionsToAdd",
        "subscriptionsToRemove",
    ];
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else {
        return ResponseTemplate::new(400);
    };
    if body.len() != KEYS.len() || KEYS.iter().any(|key| !body.contains_key(*key)) {
        return ResponseTemplate::new(400);
    }
    let status = |mri: &str| {
        if polling_only.iter().any(|polled| polled == mri) {
            30_010
        } else {
            20_200
        }
    };
    let statuses = |key: &str| -> Vec<Value> {
        body[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["mri"].as_str())
            .map(|mri| serde_json::json!({"mri": mri, "source": "ups", "status": status(mri)}))
            .collect()
    };
    let answer = serde_json::json!({
        "addStatuses": statuses("subscriptionsToAdd"),
        "removeStatuses": statuses("subscriptionsToRemove"),
    });
    ResponseTemplate::new(200).set_body_raw(answer.to_string(), "application/json")
}
