//! A group call's operations: its start in a group chat, a callee's answer, a member's join or roster-only
//! subscription, an add and a renegotiation. Each asks the call's media node first (a leg's requests repeat safely
//! by its id) and then makes the step that records what the node answered ([`super::state`]); a join or an add first
//! reserves its seat, so the sources the node is given are the ones the step records. The legs and the room a step
//! lets go of are dropped once it is stored ([`super::carry_out`]).

use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::future::try_join_all;
use serde_json::{Value, json};

use super::media::{MediaHome, MediaNodes, Sdp, drop_room};
use super::repo::{self, Key};
use super::state::{At, Call, Kind, Leg, Out, Party, Phase, Refusal, Reserved, Seat, Status};
use super::wire;
use super::{Caller, Failure, call_events, calling_endpoints, carry_out, needed_links, now_ms, own_links, publish};
use super::{step, step_any};
use crate::app::App;
use crate::db::{DbError, users};

/// What a group call's start names: the caller as it names itself, the chat, whom it rings and its offer.
pub(super) struct Start<'a> {
    pub from: &'a Value,
    pub chat: &'a str,
    /// The `participants.to` entries.
    pub to: &'a [Value],
    pub offer: &'a Value,
}

/// The media nodes this node reaches; none, and a group call isn't served.
pub(super) fn nodes(app: &App) -> Result<&Arc<dyn MediaNodes>, Failure> {
    app.mixers.get().ok_or(Failure::NotServed)
}

/// The offer or answer a request's `mediaContent` carries.
pub(super) fn sdp(media: Option<&Value>) -> Result<Sdp, Failure> {
    media
        .filter(|media| media.is_object())
        .and_then(|media| serde_json::from_value(media.clone()).ok())
        .ok_or(Failure::Refused(Refusal::Bad))
}

/// An offer or answer of the media node's, as a delivery's `mediaContent` starts from it.
pub(super) fn content(sdp: &Sdp) -> Value {
    json!({"blob": sdp.blob, "contentType": sdp.content_type})
}

/// Whether `account` is in the chat `chat` now.
async fn member(app: &App, chat: Option<&str>, account: &str) -> Result<bool, DbError> {
    let Some(chat) = chat else { return Ok(false) };
    let members = call_events::members(app, chat).await?;
    Ok(members.is_some_and(|members| members.iter().any(|member| member == account)))
}

/// Hands the node the bandwidth a party seeded in `content` (a `mediaContent`'s `applyChannelParameters`) for its leg
/// `leg`, if it seeded any: no video goes to the leg before. One not handed on is logged.
pub(super) async fn seed_leg(app: &App, home: &MediaHome, call: &str, leg: &str, content: Option<&Value>) {
    let Some(seed) = content
        .and_then(|content| content.get("applyChannelParameters"))
        .and_then(wire::seed)
    else {
        return;
    };
    let Ok(nodes) = nodes(app) else { return };
    if let Err(error) = nodes.seed(home, call, leg, seed).await {
        tracing::warn!(%error, "a call leg's bandwidth seed wasn't handed to its media node");
    }
}

pub(super) fn home_of(call: &Call) -> Result<&MediaHome, Failure> {
    call.media.as_ref().ok_or(Failure::Refused(Refusal::Gone))
}

fn leg_of(call: &Call, seat: Seat) -> Result<&Leg, Failure> {
    call.party(seat).leg.as_ref().ok_or(Failure::Refused(Refusal::Gone))
}

/// `POST epconv` in a group chat: the call is made on a media node, the caller accepted with the node's answer, each
/// other member it names rung with the node's offer to its own leg, and the chat told where the call runs. A chat
/// whose call still runs refuses it, naming that call; a meeting's chat refuses it (its call is the meeting's).
pub(super) async fn start(app: &App, caller: &Caller, asked: Start<'_>, body: &Value) -> Result<Response, Failure> {
    let nodes = nodes(app)?;
    let members = starter_in(app, caller, asked.chat).await?;
    let offer = sdp(Some(asked.offer))?;
    let (endpoint, links) = needed_links(app, caller, body).await?;
    if !["acceptance", "end"].iter().all(|name| links.contains_key(*name)) {
        return Err(Refusal::Bad.into());
    }
    let refusal = links.get("conversationEnd").cloned();
    if let Some(running) = repo::running_in(&app.db, asked.chat).await? {
        return Ok(refuse(app, &running.call, refusal.as_deref()).await);
    }
    let mut from = super::ops::party(&caller.id, &caller.name, asked.from);
    from.keep(&endpoint, links);
    from.active = Some(endpoint);
    let mut call = new_call(asked.chat, from);
    let rung = callees(app, &mut call, asked.to, &members).await?;
    let (out, offers) = match open(app, nodes.as_ref(), &mut call, &offer, &rung).await? {
        Opened::Made { out, offers } => (out, offers),
        Opened::Running(running) => return Ok(refuse(app, &running, refusal.as_deref()).await),
    };
    let origin = app.config.origin();
    let conversation = wire::conversation_url(&origin, &call.conversation, None);
    super::ops::tell_chat_started(app, caller, &call, Some(&conversation)).await;
    let tenant = crate::db::meta::tenant(&app.db).await?;
    let region = &app.config.region;
    for ((seat, endpoints), offer) in rung.into_iter().zip(offers) {
        let invitation = wire::group_invitation(
            &origin,
            &call,
            (Seat::CALLER, seat),
            (&tenant, region),
            &content(&offer),
        );
        super::ops::ring(app, &call.id, &invitation, endpoints).await;
    }
    carry_out(app, &call, out).await;
    if let (Some(home), Some(leg)) = (call.media.as_ref(), call.party(Seat::CALLER).leg.as_ref()) {
        seed_leg(app, home, &call.id, &leg.id, Some(asked.offer)).await;
    }
    Ok(Json(wire::conversation(&origin, region, &call)).into_response())
}

/// The people of `chat`, where `caller` may start a group call: it is one of them, and the chat is no meeting's and
/// no channel (a channel's call is its Meet now, [`super::meeting::meet_now`]).
async fn starter_in(app: &App, caller: &Caller, chat: &str) -> Result<Vec<String>, Failure> {
    let members = call_events::members(app, chat).await?.ok_or(Refusal::Missing)?;
    if !members.contains(&caller.id) {
        return Err(Refusal::Forbidden.into());
    }
    if crate::calendar::meeting_of(&app.db, chat).await?.is_some()
        || crate::chatsvc::channels::place(&app.db, chat).await?.is_some()
    {
        return Err(Refusal::Bad.into());
    }
    Ok(members)
}

/// Seats the members of the chat `to` names (once each, the caller aside, each whose account is enabled) as the
/// call's callees: each with the calling endpoints it rings on.
async fn callees(
    app: &App,
    call: &mut Call,
    to: &[Value],
    members: &[String],
) -> Result<Vec<(Seat, Vec<String>)>, Failure> {
    let mut rung = Vec::new();
    for named in to {
        let id = named
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.strip_prefix("8:orgid:"));
        let Some(id) = id.filter(|id| members.iter().any(|member| member == id) && call.seat_of(id).is_none()) else {
            continue;
        };
        let Some(user) = users::by_id(&app.db, id).await?.filter(|user| !user.disabled) else {
            continue;
        };
        call.parties
            .push(super::ops::party(&user.id, &user.display_name, named));
        rung.push((Seat(call.parties.len() - 1), calling_endpoints(app, &user.id).await?));
    }
    Ok(rung)
}

/// What [`open`] made of a start.
pub(super) enum Opened {
    /// The call is stored; `out` tells the caller, `offers` are the node's to each callee rung.
    Made { out: Out, offers: Vec<Sdp> },
    /// Another start in the chat was first: its call.
    Running(Box<Call>),
}

/// Gives every party its leg on a media node (the caller's answering its `offer`), then stores the call. A room whose
/// call isn't stored is dropped.
pub(super) async fn open(
    app: &App,
    nodes: &dyn MediaNodes,
    call: &mut Call,
    offer: &Sdp,
    rung: &[(Seat, Vec<String>)],
) -> Result<Opened, Failure> {
    for seat in 0..call.parties.len() {
        let sources = call.next_sources();
        call.party_mut(Seat(seat)).leg = Some(Leg {
            id: uuid::Uuid::new_v4().to_string(),
            sources,
            before: None,
        });
    }
    let origin = app.config.origin();
    let at = At {
        origin: &origin,
        now: now_ms(),
    };
    call.started_group(at, at.seconds() + super::ring_seconds(app));
    let home = nodes.pick().await?;
    call.media = Some(home.clone());
    let (answer, offers) = match make_legs(nodes, call, offer, rung).await {
        Ok(made) => made,
        Err(error) => {
            drop_room(nodes, &home, &call.id).await;
            return Err(error.into());
        }
    };
    let mut out = Out::default();
    call.tell_started(&origin, &content(&answer), &mut out);
    let Err(error) = repo::insert(&app.db, call).await else {
        return Ok(Opened::Made { out, offers });
    };
    drop_room(nodes, &home, &call.id).await;
    let DbError::Conflict = error else {
        return Err(error.into());
    };
    let chat = call.chat.as_deref().unwrap_or_default();
    let running = repo::running_in(&app.db, chat).await?.ok_or(Failure::Busy)?;
    Ok(Opened::Running(Box::new(running.call)))
}

/// A group call in `chat` placed by `from`, before its legs are made.
pub(super) fn new_call(chat: &str, from: Party) -> Call {
    let now = now_ms();
    Call {
        id: uuid::Uuid::new_v4().to_string(),
        conversation: uuid::Uuid::new_v4().simple().to_string(),
        phase: Phase::Connected,
        parties: vec![from],
        offer: Value::Null,
        started_at: now,
        connected_at: Some(now),
        ended_at: None,
        due_at: now / 1000,
        roster_sequence: 0,
        negotiation: None,
        negotiations: 0,
        answered: None,
        held_by: None,
        chat: Some(chat.to_owned()),
        accepted_as: None,
        kind: Kind::Group,
        media: None,
        next_source: 0,
        watchers: Vec::new(),
        alive_due: 0,
        meeting: None,
    }
}

/// The caller's leg with its `offer` (the node's answer) and each rung callee's (the node's offer to it).
async fn make_legs(
    nodes: &dyn MediaNodes,
    call: &Call,
    offer: &Sdp,
    rung: &[(Seat, Vec<String>)],
) -> Result<(Sdp, Vec<Sdp>), super::MediaError> {
    let home = home_of(call).map_err(|_| super::MediaError::Gone)?;
    let leg = |seat: Seat| call.party(seat).leg.clone();
    let caller = leg(Seat::CALLER).ok_or(super::MediaError::Gone)?;
    let answer = nodes
        .put_leg(home, &call.id, &caller.id, caller.sources, Some(offer))
        .await?;
    let offers = try_join_all(rung.iter().map(|(seat, _)| async move {
        let leg = leg(*seat).ok_or(super::MediaError::Gone)?;
        nodes.put_leg(home, &call.id, &leg.id, leg.sources, None).await
    }))
    .await?;
    Ok((answer, offers))
}

/// A start in a chat whose call runs: answered with a conversation that never runs, and ended at once at its
/// `conversationEnd` callback naming the running call (as the service refuses it).
async fn refuse(app: &App, running: &Call, conversation_end: Option<&str>) -> Response {
    let origin = app.config.origin();
    let mut out = Out::default();
    let refused = wire::running_elsewhere(&origin, running);
    if let Some((endpoint, path)) = conversation_end.and_then(|link| wire::callback(&origin, link)) {
        out.deliveries.push(super::state::Delivery {
            endpoint: endpoint.to_owned(),
            path: path.to_owned(),
            body: refused,
        });
    }
    for delivery in &out.deliveries {
        publish(app, &running.id, delivery).await;
    }
    let never = uuid::Uuid::new_v4().simple().to_string();
    Json(wire::unrun_conversation(&origin, &app.config.region, &never)).into_response()
}

/// A ringing party's `accept`: its answer to the node's offer goes to its leg, then it is in the call on that leg.
pub(super) async fn accept(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let (endpoint, links) = needed_links(app, caller, body).await?;
    let answer = sdp(body.pointer("/callAcceptance/mediaContent"))?;
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let call = kept.call;
    let seat = call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    let leg = call.answerable(seat)?;
    let content = body.pointer("/callAcceptance/mediaContent");
    nodes(app)?.answer_leg(home_of(&call)?, &call.id, leg, &answer).await?;
    seed_leg(app, home_of(&call)?, &call.id, leg, content).await;
    let answered = step(app, key, caller, |call, seat, at, out| {
        call.group_accept(seat, (leg, &endpoint), (links.clone(), body), at, out)
    })
    .await?;
    Ok(Json(answered).into_response())
}

/// A party's renegotiation: the node answers it, and the answer goes back to the party alone. One while the party's
/// last answer isn't acknowledged is turned away (`491`).
pub(super) async fn renegotiate(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let (endpoint, links) = needed_links(app, caller, body).await?;
    let offer = sdp(body.pointer("/mediaNegotiation/mediaContent"))?;
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let call = kept.call;
    let seat = call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    call.in_call(seat)?;
    if call.waits_for_acknowledgement(seat, now_ms()) {
        let turned_back = step(app, key, caller, |call, seat, at, out| {
            Ok(call.turned_back(seat, &links, at, out))
        })
        .await?;
        if turned_back {
            return Ok(StatusCode::OK.into_response());
        }
    }
    let leg = &leg_of(&call, seat)?.id;
    let answer = nodes(app)?
        .renegotiate_leg(home_of(&call)?, &call.id, leg, &offer)
        .await?;
    let offered = body.pointer("/mediaNegotiation/mediaContent");
    seed_leg(app, home_of(&call)?, &call.id, leg, offered).await;
    step(app, key, caller, |call, seat, at, out| {
        call.group_renegotiated(seat, (&endpoint, links.clone()), &content(&answer), at, out)
    })
    .await?;
    Ok(StatusCode::OK.into_response())
}

/// `updateMediaDescriptions` of a party in a group call: what it sends now (its camera, its screen) goes into the
/// roster, which tells the others. A body that describes nothing changes nothing.
pub(super) async fn describe(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    match wire::showing(body) {
        Some(showing) => {
            step(app, key, caller, |call, seat, at, out| {
                call.group_described(seat, showing, at, out)
            })
            .await?;
        }
        None => {
            step(app, key, caller, |call, seat, _, _| call.described(seat)).await?;
        }
    }
    Ok(StatusCode::OK.into_response())
}

/// `applyChannelParameters` of a party in a group call: a bandwidth seed among them goes to its leg on the media node.
pub(super) async fn parameters(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let call = kept.call;
    let seat = call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    call.in_call(seat)?;
    let leg = leg_of(&call, seat)?;
    seed_leg(app, home_of(&call)?, &call.id, &leg.id, Some(body)).await;
    Ok(StatusCode::OK.into_response())
}

/// `POST conv/<id>` on a group call: with a `callInvitation` a chat member joins it (a meeting's call: its lobby decides,
/// [`super::meeting::enter_at_conversation`]); without, a party that rings or is in it hands out more callbacks, or a
/// chat member follows the roster. Anyone else finds no call.
pub(super) async fn subscribe(
    app: &App,
    caller: &Caller,
    key: Key<'_>,
    call: &Call,
    body: &Value,
) -> Result<Response, Failure> {
    let joins = body
        .get("callInvitation")
        .is_some_and(|invitation| !invitation.is_null());
    if joins && call.meeting.is_some() {
        return super::meeting::enter_at_conversation(app, caller, call, body).await;
    }
    let present = call
        .seat_of(&caller.id)
        .is_some_and(|seat| matches!(call.party(seat).status, Status::Ringing | Status::InCall));
    if (joins || !present) && !member(app, call.chat.as_deref(), &caller.id).await? {
        return Err(Refusal::Missing.into());
    }
    if joins {
        return join(app, caller, key, call, body).await;
    }
    let origin = app.config.origin();
    let region = &app.config.region;
    let Some((endpoint, links)) = own_links(app, caller, body).await? else {
        return Ok(Json(wire::conversation(&origin, region, call)).into_response());
    };
    let answer = step_any(app, key, |call, at, out| {
        let own = call.seat_of(&caller.id).filter(|seat| {
            let endpoints = &call.party(*seat).endpoints;
            endpoints.iter().any(|known| known.endpoint == endpoint)
        });
        match own {
            Some(seat) => call.more_links(seat, &endpoint, links.clone()),
            None => call.watch(&caller.id, &endpoint, links.clone(), at, out)?,
        }
        Ok(wire::conversation(at.origin, region, call))
    })
    .await?;
    Ok(Json(answer).into_response())
}

/// A join with its offer of an account the caller already let in (a chat member, or one a meeting's lobby lets in):
/// its seat reserved, its leg made with the node (the node's answer), then it is in the call.
pub(super) async fn join(
    app: &App,
    caller: &Caller,
    key: Key<'_>,
    call: &Call,
    body: &Value,
) -> Result<Response, Failure> {
    let nodes = nodes(app)?;
    let offer = sdp(body.pointer("/callInvitation/mediaContent"))?;
    let (endpoint, links) = needed_links(app, caller, body).await?;
    if !links.contains_key("acceptance") {
        return Err(Refusal::Bad.into());
    }
    let named = body
        .pointer("/participants/from")
        .filter(|from| from.is_object())
        .ok_or(Refusal::Bad)?;
    let account = super::ops::party(&caller.id, &caller.name, named);
    let leg = uuid::Uuid::new_v4().to_string();
    let reserved = step_any(app, key, |call, _, _| call.reserve(&account, &leg)).await?;
    let home = home_of(call)?;
    let made = nodes
        .put_leg(home, &call.id, &leg, reserved.sources, Some(&offer))
        .await;
    let answer = match made {
        Ok(answer) => answer,
        Err(error) => {
            unreserve(app, key, &reserved, &leg).await;
            return Err(error.into());
        }
    };
    seed_leg(app, home, &call.id, &leg, body.pointer("/callInvitation/mediaContent")).await;
    let region = &app.config.region;
    let joined = step_any(app, key, |call, at, out| {
        call.join(
            reserved.seat,
            (&leg, &endpoint, links.clone()),
            &content(&answer),
            at,
            out,
        )?;
        Ok(wire::conversation(at.origin, region, call))
    })
    .await;
    match joined {
        Ok(joined) => Ok(Json(joined).into_response()),
        Err(failure) => {
            unreserve(app, key, &reserved, &leg).await;
            Err(failure)
        }
    }
}

/// `POST conv/<id>/add` of a party in a group call (a meeting's: of a host): a member of the call's chat rings as a
/// callee, with the node's offer to a leg of its own. Someone with no calling endpoint isn't brought in, and the adder
/// is told at its `addParticipantFailure` callback.
pub(super) async fn add(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let call = kept.call;
    let seat = call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    if call.kind != Kind::Group {
        return Err(Refusal::Missing.into());
    }
    call.in_call(seat)?;
    if call.meeting.is_some() {
        call.hosts(seat)?;
    }
    let named = body
        .pointer("/participants/to/0")
        .filter(|to| to.is_object())
        .ok_or(Refusal::Bad)?;
    let invitee = named
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| id.strip_prefix("8:orgid:"))
        .ok_or(Refusal::Bad)?;
    if !member(app, call.chat.as_deref(), invitee).await? {
        return Err(Refusal::Forbidden.into());
    }
    let user = users::by_id(&app.db, invitee)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(Refusal::Missing)?;
    let failure = own_links(app, caller, body)
        .await?
        .and_then(|(_, links)| links.get("addParticipantFailure").cloned());
    let endpoints = calling_endpoints(app, &user.id).await?;
    if endpoints.is_empty() {
        add_failed(app, &call, failure.as_deref(), invitee).await;
    } else {
        let account = super::ops::party(&user.id, &user.display_name, named);
        ring_added(app, key, &call, (seat, &account, failure), endpoints).await?;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

/// The adder is told at its `addParticipantFailure` callback (`failure`) that `invitee` has nothing to ring.
async fn add_failed(app: &App, call: &Call, failure: Option<&str>, invitee: &str) {
    let origin = app.config.origin();
    let mut out = Out::default();
    if let Some((endpoint, path)) = failure.and_then(|link| wire::callback(&origin, link)) {
        out.deliveries.push(super::state::Delivery {
            endpoint: endpoint.to_owned(),
            path: path.to_owned(),
            body: wire::add_failure(&format!("8:orgid:{invitee}")),
        });
    }
    carry_out(app, call, out).await;
}

/// Seats `account` with a leg of its own, the node's offer to it, and rings it on `endpoints` with an invitation from
/// its adder (the party at `adder`, whose `failure` link hears a decline): the original caller added back must not
/// be invited by itself, which its client ignores. One that still rings rings on as it is.
async fn ring_added(
    app: &App,
    key: Key<'_>,
    call: &Call,
    (adder, account, failure): (Seat, &Party, Option<String>),
    endpoints: Vec<String>,
) -> Result<(), Failure> {
    let nodes = nodes(app)?;
    let home = home_of(call)?;
    let tenant = crate::db::meta::tenant(&app.db).await?;
    let leg = uuid::Uuid::new_v4().to_string();
    let reserved = step_any(app, key, |call, _, _| call.reserve_added(account, &leg)).await?;
    let Some(reserved) = reserved else {
        return Ok(());
    };
    let offer = match nodes.put_leg(home, &call.id, &leg, reserved.sources, None).await {
        Ok(offer) => offer,
        Err(error) => {
            unreserve(app, key, &reserved, &leg).await;
            return Err(error.into());
        }
    };
    let ring_due = now_ms() / 1000 + super::ring_seconds(app);
    let region = &app.config.region;
    let invitation = step_any(app, key, |call, at, out| {
        call.ring(reserved.seat, &leg, ring_due, out)?;
        call.party_mut(reserved.seat).add_failure.clone_from(&failure);
        Ok(wire::group_invitation(
            at.origin,
            call,
            (adder, reserved.seat),
            (&tenant, region),
            &content(&offer),
        ))
    })
    .await;
    let invitation = match invitation {
        Ok(invitation) => invitation,
        Err(failure) => {
            unreserve(app, key, &reserved, &leg).await;
            return Err(failure);
        }
    };
    super::ops::ring(app, &call.id, &invitation, endpoints).await;
    Ok(())
}

/// Undoes `reserved`, whose leg `leg` the node didn't make (or made late) or whose step wasn't stored; the leg is
/// dropped once that is stored.
pub(super) async fn unreserve(app: &App, key: Key<'_>, reserved: &Reserved, leg: &str) {
    let undone = step_any(app, key, |call, _, out| {
        call.unreserve(reserved, leg, out);
        Ok(())
    })
    .await;
    if let Err(Failure::Database(error)) = undone {
        tracing::warn!(%error, "a call's reserved seat wasn't let go");
    }
}
