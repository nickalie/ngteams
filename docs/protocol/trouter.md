# Trouter: the push channel for messaging

Status: 2026-10-20, M1 Task 14; the native client's part (below) 2026-10-21, M1 Task 16; others' and own presence 2026-10-23, M2 Tasks 17–18. Written from live runs of `proto-lab run ab-trouter --headless` (accounts a and b,
Chrome for Testing 155 headless, under the capture policy of that period), from M0's `echo-call` runs
(`calling-echo.md`), and from the web bundle `26260.1717.5229.7049` (read locally, notes in our own words). The
captures stay outside the repository; the fixtures in `tests/fixtures/trouter/` are synthetic, shaped like them
(`session.json`: one socket's frames both ways; `registration.json`: both stacks' registrations; `deliveries.json`:
the chat deliveries and the web client's answers). Ids, tokens, host numbers and addresses below are placeholders.

Current code: the web capture scenarios are `proto-lab/src/scenario/ab_trouter.rs`; the quiet experiment holds
`user.activity` with a selective capture wrapper. The native socket is `teams-trouter`, with typed frames and
connection-bound call responses from `teams-proto`. There is no browser policy/classifier. Dated denials and old
logs below describe capture-era restrictions only; they are not current behavior.

Current native live tests run `scripts/run-lab.sh <alias>` (with `--pipewire` for media) and drive the sandboxed
`ngteams` UI. Before each run the agent lists its chats, calls and people and verifies the [lab rule](../lab-rules.md).
The runner owns shared live-lock and per-alias lifetime locks, supports concurrent aliases and defaults to an 1800 s
TERM deadline. `--status` and `--stop` address that alias's sandbox only.

## Outcome

- Chat events reach the messaging stack's own socket as `POST /v4/f/<endpoint>/messaging` deliveries, `async` with a
  15-minute lifetime: new messages, typing, the chat's update, edits, reactions, deletes. a's message showed in b's
  open chat 3.5–4 s after a's compose box let it go (typing and the client's own rendering included; the delivery
  itself came 200–250 ms after the message's `originalarrivaltime`), an edit 1.2–1.6 s after a sent it, and a
  reaction's delivery reached b's socket 0.9 s after a's reaction request went out (0.13 s after its `200`).
- **Unanswered messaging deliveries cost a bounded replay on every reconnect, nothing more**: ordinary chat deliveries stay unanswered; the messaging socket builds no response path ([Answering deliveries](#answering-deliveries)).
- **`user.activity` is not needed** for deliveries; the native client doesn't send it in M1
  ([`user.activity` and presence](#useractivity-and-presence)).
- The web client never re-authenticates an open socket; a renewed token takes a reconnect
  ([Token renewal](#token-renewal-on-a-live-socket)).

## Capture setup

`proto-lab` captures network and WebRTC traffic, enables target hooks and bypasses page service workers so page
network IDs match held requests. A scenario may hold a matching HTTP request or selected `user.activity` frames to
measure their role without sending them. Those holds are explicit experiment controls, not participant checks.
Historical held-marker messages remain held on later loads to prevent the browser retrying them then.

Both the messaging registration (`TeamsCDLWebWorker`) and calling registration (`SkypeSpacesWeb`) make the account's
own endpoints reachable. A capture scenario only answers or joins its own run's invitation when designed to do so.

## What the native client sends

M1 Task 15. The native client's socket sends only `teams_proto::OwnFrame`s, through `teams_proto::FrameSink`: typed
frames of `teams-proto`, each built by its constructor in the messaging stack's shape. The trait is sealed only to keep
frame text out of other crates' hands (no constructor takes frame text); which frames exist isn't a reviewed list, and
adding one is an ordinary change. Both frame directions are logged in full at debug with credentials redacted; info has no payload. The frames:

| kind | frame |
|---|---|
| `trouter_authenticate` | `5:::{"name":"user.authenticate",…}`, the headers exactly `X-Ms-Test-User: False`, `Authorization`, `X-MS-Migration: True` (the messaging stack sent all three on every socket, the first one included; the calling stack's sockets sent no `X-Ms-Test-User`), plus the seven `connectparams` when a `trouter.connected` handed some over |
| `trouter_ping` | `5:<n>+::{"name":"ping"}` |
| `trouter_message_loss_ack` | `5:<n>+::{"name":"trouter.processed_message_loss",…}`, the indicators as they came (never an empty list) |
| `trouter_disconnect` | `0::` |

The sink numbers the acknowledged events itself (`<n>`, from 1 per socket). No `user.activity` (nothing needs it) and
no answer to a chat delivery: neither has a type, so neither can be sent. A call's deliveries are answered only on the
calling socket, as typed responses (`AcceptanceAcknowledgement`, `InvitationReceipt`) through `FrameSink::respond`,
bound to the delivery's connection ([the calling socket](#the-calling-stacks-socket-m3-task-12)).

## The native client (M1 Task 16)

`crates/teams-trouter` is the native push channel, tested against `teams-mock`'s Trouter (`TrouterMock`, which
answers as the fixtures here do and delivers the chat mock's queued events once a registration names the endpoint):

- **Connect**: `wss://go.trouter.teams.microsoft.com/v4/c`, then the `reconnectUrl` a `trouter.connected` handed over
  (a `<name>-t` host under `.trouter.teams.microsoft.com`, port 443 dropped as the web client's connect URL dropped
  it) with its `connectparams`, until those are within an hour of `se`, three connects in a row failed (any that
  ended before its `trouter.connected`, but for a renewal) or two `dup` disconnects came within 10 s. The query is
  the messaging stack's as captured: `tc` (`{"cv":…,"ua":"TeamsCDL","hr":"","v":"1415/<build>"}`, braces and quotes
  percent-encoded, `:`, `,` and `/` left), `timeout=40`, `epid` (one GUID per client run), `cor_id` (a GUID per
  connect), `con_num` (`<ms>_<n>`); the handshake carries the web client's `Origin: https://teams.microsoft.com` and
  no credential. TLS is checked by the system's verifier, as the HTTP transport's. The host allowlist and the TLS
  are lifted only by the `test-support` mock origin (`ws` on loopback), which the production compile-contract tests keep out of a normal build.
- **Frames**: `user.authenticate` at once after `1::` (never before it: a stop before then sends nothing at all),
  pings every 40 s and a reconnect when a pong is 5 s late, the message-loss acknowledgement once the owner has caught
  up, and `0::` on a stop or a renewal (once authenticated). Each goes through `teams_proto::FrameSink`, with debug wire logging;
  the socket's write half goes only to the sink. No `user.activity`, no answer to any delivery, an incoming
  call's (a `callNotification`, whatever its path) included.
- **Registration**: the messaging stack's, as `teams_proto::EndpointRegistration` (a plain request through
  the transport's typed own-state registration path): the body above with `templateKey` `TeamsCDLWebWorker_2.6` and `platformUIVersion` the `tc`'s `v`, on every
  `trouter.connected`, again 30 s before its hour ends, after a failure again 123 s later. The transport sends the
  registrar `X-MS-Migration: True` and `X-Ms-Test-User: False` with the token. The push URL's endpoint id is
  base64url; until M1 Task 21 the registration refused one with a `-` or `_` (11 of the 28 ids in the captures), so such an
  endpoint went unregistered and got no deliveries.
- **Reconnect**: at once for a renewal, a missing pong, an `unauthorized` disconnect (with a new token) or a `dup`,
  but never twice within 10 s; otherwise after a backoff of `1 s · 2^n` ±20 %, capped at 300 s, whose count starts
  again after a connection stayed up a minute; a throttled token service's `Retry-After` is a floor on that wait,
  never a shortcut past it. `skypetoken-deprecated` stops the client.
- **Renewal**: an open socket is never authenticated again, so the client reconnects with a new token 5 minutes
  (the session's refresh margin) before the held one expires, checking the expiry against the wall clock every 30 s,
  and at once when the session announces another `ic3` token (told by its value, not its expiry). A renewal before
  the service's `1::` sends nothing on that socket.

### The calling stack's socket (M3 Task 12)

`Trouter::for_calls` gives the account a calling socket and endpoint separate from its messaging socket. The
engine's call stack owns it, tested against `TrouterMock` in `teams-trouter/tests/calling.rs`.

- **Connect/authenticate:** `tc` names `"ua":"SkypeSpaces"`, the query adds `dom=teams.microsoft.com`, and
  `Authenticate::calling` carries `Authorization` and `X-MS-Migration`. Regional movement, backoff and renewal follow
  the messaging socket's rules.
- **Registration:** `CallingRegistration` (`SkypeSpacesWeb`, `SkypeSpacesWeb_2.6`) uses the typed own-state path on
  every `trouter.connected`, including reconnects. This does not create the `RegisteredEndpoint` used by presence;
  own availability uses the messaging registration.
- **Deliveries:** the endpoint root and `callAgent/` callbacks are decoded (base64/gzip as declared) and observed by
  `teams-call::track`. A malformed or unoffered invitation becomes `PushEvent::Ignored`; accepted observations reach
  the consumer as `PushEvent::Call`. Other paths, including `messaging`, are ignored on this socket.
- **Replies:** only typed `AcceptanceAcknowledgement` or `InvitationReceipt` values go through the delivery's
  `CallResponder` and `FrameSink::respond`. A reply for a closed connection cannot be moved to another connection.
  Message-loss reports need no chat catch-up here and are acknowledged directly.
- **Dropped socket/renewal:** cached `connectparams` preserve the push endpoint and callback URLs; the new socket
  registers again. Nothing is sent to the call merely because the socket reconnects.
- **Tracking/floor:** readable invitations must address the account and are deduplicated; ended rings stay ended.
  The core floor coordinates one call or ring across accounts. Only user actions answer, decline or join.

## Connect and the regional move

- The client opens `wss://go.trouter.teams.microsoft.com/v4/c?tc=<json>&timeout=40&epid=<endpoint guid>&ccid=…&
  cor_id=<guid>&con_num=<ms>_<n>`. `tc` names the client (`"ua":"TeamsCDL"` for the messaging stack,
  `"SkypeSpaces"` for the calling stack, plus its version). `timeout=40` is the ping interval in seconds. The
  handshake carries no credential.
- The service answers `1::` (socket.io 0.9 connect). The client must authenticate at once: once, with the
  authentication held about 1.5 s for the fence's decision under load, the service closed the socket with
  `0:::{"reason":"timeout"}` 1.1 s after `1::`; the client opened a new socket and authenticated in time.
- After `trouter.connected` (below) the client keeps the regional host and reconnects straight to `reconnectUrl`
  (`wss://<name>-t.trouter.teams.microsoft.com:443/v4/c`) next time, sending the cached `connectparams` in its
  `user.authenticate`. The cache survives a page load and even a browser restart (it lives in the profile): b's
  sockets reconnected to the same two regional hosts with the same endpoint ids in runs 45 minutes apart. Per the bundle
  the cache is dropped when `se` is less than an hour away, the service URL changes, three connects in a row fail, or
  the service disconnects twice for a duplicate within 10 s.
- Each stack opens its own socket and its own endpoint (`epid`), and one page load can leave one socket per document:
  b had two `TeamsCDL` sockets (one per page load) and one `SkypeSpaces` socket.

## Authentication: the first frame

`5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False","Authorization":"Bearer <ic3 token>",
"X-MS-Migration":"True"},"connectparams":{"issuer":"","scae":"1","sig":"","sr":"<endpoint id>","sp":"<regional
name>","se":"<ms>","st":"<ms>"}}]}`

- The token is the `https://ic3.teams.office.com` one (the web client's is a CAE token, `xms_cc` `CP1`, living about
  24 hours); the registrar takes the same token. The calling stack's socket sends only `Authorization` and
  `X-MS-Migration`.
- `connectparams` is what an earlier `trouter.connected` handed over, sent back as it came; a first connect has none.
- The service takes **our** non-CAE `ic3` token on Trouter, as on the chat service (`auth.md` question 8): Task 16
  built the native client against the mock, and its first live socket, in M1 Task 19's retired native probe
  (2026-10-21), authenticated, registered and got the delivery; so did every socket of Task 21's acceptance runs.

## `trouter.connected`: the endpoint

`5:1::{"name":"trouter.connected","args":[{…}]}` with:

| field | meaning |
|---|---|
| `id` | the endpoint (connection) id, 22 base64url characters (letters, digits, `-`, `_`); the last part of the push URLs |
| `ccid` | a short form of `id` |
| `url`, `surl` | the push URLs, `https://<name>-f.trouter.teams.microsoft.com:8443/v4/f/<id>/` and the same on `:3443`; the registration names `surl` |
| `curlb` | `https://<name>-t.trouter.teams.microsoft.com:443`, a base for client-to-client links |
| `healthUrl` | `…-t…:443/v4/h` |
| `reconnectUrl` | `wss://<name>-t…:443/v4/c`, where the next connect goes |
| `registrarUrl` | a v3 registrar on another host (`…/registrar/<geo>/v3/registrations`); the web client still registers through `<teams>/registrar/prod/V2/registrations` |
| `socketio` | `https://<name>-t…:443/` |
| `ttl` | seconds the connection's parameters stay valid (about 6.4 days: `se` minus now) |
| `dur` | `"1"` |
| `connectparams` | `issuer`, `scae`, `sig`, `sr` (the endpoint id), `sp` (the regional name), `se`/`st` (expiry and start, ms) |

## Keep-alives and acknowledgements

- The client sends `5:<n>+::{"name":"ping"}` every 40 s per socket; the service answers `6:::<n>+["pong"]` within
  about 100 ms. Every event the client sends with `+` (ping, `user.activity`, `processed_message_loss`) is
  acknowledged with `6:::<n>+[…]`. Per the bundle a pong missing for 5 s makes the client reconnect at once, and a
  `user.activity` not acknowledged within 10 s does too.
- The client also pings right after every answer it sends to a delivery (seen: each denied answer was followed by a
  ping within 100 ms).
- The bundle keeps a 72 s socket.io heartbeat timeout; any packet resets it.

## Message loss

On connect the service sends `5:<n>::{"name":"trouter.message_loss","args":[{"droppedIndicators":[{"tag":"messaging",
"etag":"<time>"},…]}]}`, sometimes twice or three times. The messaging stack saw only the `messaging` tag; the calling
stack's socket also saw `""`, `messagingsync`, `pinnedchannel` and `tps`. The client acknowledges each report with
`trouter.processed_message_loss` carrying the same indicators. Per the bundle the `messaging` tag makes the chat
layer run a delta sync (it missed pushes while away), and a new connection id does too. For the native client: a
`messaging` loss report means "catch up now" (Task 17), then acknowledge it.

## Registration and its TTL

`POST <teams>/registrar/prod/V2/registrations` (both `teams.microsoft.com` and `teams.cloud.microsoft` were used),
`Authorization: Bearer <ic3>`, `X-MS-Migration: True`, `X-Ms-Test-User: False`, JSON:

```json
{"clientDescription":{"appId":"TeamsCDLWebWorker","aesKey":"","languageId":"en-US","platform":"chrome",
 "templateKey":"TeamsCDLWebWorker_2.6","platformUIVersion":"<version>"},
 "registrationId":"<guid>","nodeId":"",
 "transports":{"TROUTER":[{"context":"","path":"<surl>","ttl":3600}]}}
```

- Answer `202`, empty body. The `registrationId` is the socket's `epid`.
- The messaging stack registered on every `trouter.connected` (every page load and reconnect). Per the bundle it
  registers again 30 s before the TTL ends (3600 s by default), retries a failed registration after 123 s, and
  deletes its registration only on a sign-out-like stop (a normal stop leaves it to expire).
- The calling stack registers its own socket as `SkypeSpacesWeb` (`SkypeSpacesWeb_2.6`); denied, it retried every few
  seconds for the whole run.

## Deliveries

Chat events reach the **messaging stack's** socket as socket.io messages:

`3:::{"id":<number>,"method":"POST","url":"/v4/f/<endpoint id>/messaging","headers":{…},"body":"<json text>"}`

- `id` is a signed 32-bit number: of the 2515 deliveries in the captures 267 had a negative one. Until M1 Task 21 the
  native client read it as unsigned and silently dropped those deliveries (3 to 18 of 50 messages in a live run); it
  reads it signed now.
- Headers: `X-Trouter-Delivery-Control: async; ttl=900; flow=messaging; prio=normal` (typing adds
  `resilience=nostorenoloss`), `X-Microsoft-Skype-Message-ID` (a GUID per delivery), `trouter-request`
  (`{"id":"<guid>","src":"<address>","port":3443}`), `Trouter-Timeout` / `Trouter-TimeoutMs` (about 117 s), `MS-CV`,
  `traceparent`, `Content-Type: text/xml` (the body is JSON nonetheless), `Content-Length`, `Host`, `User-Agent`
  (`Skype-NotificationHub/…`), `X-IC3-Needle` (a server-side trace) and `__RequestTunnelId__`.
- The body is an `EventMessage`: `time`, `type`, `resourceLink` (`https://notifications.skype.net/v1/users/ME/
  conversations/<chat>/messages/<id>`), `resourceType`, `resource`, sometimes `mentiontypes`, and `isactive`.
- Resource types seen, in the order a's actions caused them:

| a's action | `resourceType` | `resource` |
|---|---|---|
| typing | `NewMessage` | a message of `messagetype` `Control/Typing`, empty content, `contenttype` `Application/Message` |
| the message | `NewMessage` | the message as the chat service stores it: `id`, `version`, `clientmessageid` (a's own), `content` (HTML), `messagetype` `RichText/Html`, `from` (a `contacts/8:orgid:<guid>` link), `imdisplayname`, `originalarrivaltime`, `composetime`, `sequenceId`, `properties` (`mentions`, `cards`, `links`, `files` as JSON text, `formatVariant`, `languageStamp`), `conversationLink`, `to` (the chat id), `threadtype` |
| (follows the message) | `ConversationUpdate` | the chat: `id`, `lastMessage` (the message again; its `mentions` as a JSON array this time, plus `ams_references`), `lastUpdatedMessageId`/`Version`, `properties` (`lastimreceivedtime`, `addedBy`, an AI reply suggestion), `threadProperties` (`uniquerosterthread`, `rosterVersion`, `lastSequenceId`, `threadType`, `tenantid`, …), `version`, `propertiesUpdated`, `memberProperties` |
| the edit | `MessageUpdate` | the same message id with a new `version`, the new `content` and `properties.edittime` |
| a reaction | `MessageUpdate` | the same message, a new `version`, and `properties.emotions`: the whole set, `[{"key":"like","users":[{"mri":"8:orgid:<guid>","time":<ms>,"value":"<ms>"}]}]` |
| a reaction removed | `MessageUpdate` | `emotions` with the key kept and its `users` empty |
| a delete | `MessageUpdate` | the same message id, a new `version`, empty `content`, `properties` only `deletetime`, `importance`, `subject` |
| a reply with a quote | `NewMessage` | `content` starting with the quote's `<blockquote itemscope itemtype="http://schema.skype.com/Reply" …>`, `properties.qtdMsgs` and `hasValidMsgReferences` |
| b reading the chat (to b's own endpoints) | `ConversationUpdate` | the chat, its `propertiesUpdated` `["consumptionhorizon"]` and `properties.consumptionhorizon` the new horizon (`<arrival id>;<ms>;<clientmessageid>`, `chat.md`) |

The reaction row was captured live in run 13 (below): a liked its edited message from the message's hover bar
(`message-actions-like`, the control `ab-chat-full` uses), its `PUT …/properties?name=emotions` was answered `200`
741 ms after it went out, and b's open socket got one live `MessageUpdate` 873 ms after it went out: the message
whole (its `properties` still holding `edittime`), `emotions` `[{"key":"like","users":[one user: `mri`, `time`,
`value`]}]`, the live delivery's headers as above (`X-Trouter-Delivery-Control: async; ttl=900; flow=messaging;
prio=normal`, `Trouter-Timeout`/`Trouter-TimeoutMs`, `__RequestTunnelId__`, no `mstore`). a's own socket got the same
event. Left unanswered, it wasn't delivered again on b's stable socket in the 16 minutes that followed. The removal,
delete and reply rows come from deliveries b's endpoints got as stored replays of an `ab-chat-full` run (Task 7, the
same a–b chat), seen again at run 13's connects. A reaction by b to a's message reaches b's own endpoints too: an
account's endpoints get its own
events.

The native client turns these into typed events (Task 16, `teams_trouter::events`): a `NewMessage` whose
`messagetype` is `Control/Typing` is a signal (typing), not a message, and so is every other `Control/…` one
(`Control/ClearTyping`, known from the bundle only, is typing's end when one is delivered: the web client sent none in
three `chat-shapes` runs, [chat.md](chat.md#typing-chat-shapes); one no capture showed is `Unknown`); a message's own `clientmessageid` matches it
to our outbox row (Task 13). A `MessageUpdate` carries the whole message, so what changed is told from it the way
every one in the captures of runs 12 and 13 (below; both accounts' sockets) could be told (read locally, the times
compared, none printed): an
edit's `edittime` equals its new `version` (6 of 6), a delete's `deletetime` does (3 of 3), and a reaction change
carries `emotions` with neither (14 of 14: an edited message keeps its old `edittime`). A `ConversationUpdate` whose
`propertiesUpdated` names `consumptionhorizon` is the account's own read horizon (4 of 4, the last row above, in
`deliveries.json` as the eighth delivery), unless it also carries `consumptionHorizonBookmark`: it then stays a
`ConversationUpdate` so the engine stores both properties. The engine stores a carried bookmark even when
`propertiesUpdated` does not name the horizon. Any other delivery is the chat's update. A `CustomUserProperties`
(`resourceLink` `/v1/users/ME/properties`, seen in a personal build's debug log with `recentReactions` and
`inQuarantine`) carries the account's own properties that changed; it is read only when it names
`appConsumptionHorizon` ([chat.md](chat.md#which-chats-are-unread)). Any other `resourceType` is kept as `Unknown` by
its name. No delivery of another member's read receipt and none of a new chat were seen, so the mock
doesn't deliver those (`teams_mock::TrouterMock`).

## Answering deliveries

The web client answers each chat delivery `3:::{"id":<the delivery's id>,"status":200,"headers":{"MS-CV":"<its
MS-CV>.0","trouter-request":"<echoed>","trouter-client":"{\"cd\":<ms spent>}"},"body":""}` and pings right after.
The messaging set denied every answer, as the frame rule did until the lab's call rule let them through (socket.io
type `3`), so every run then showed what an endpoint that never answers gets:

- **On a stable socket, nothing more.** Each live delivery came once; in 16 minutes of observation after the last one
  (two runs) none came again. The socket stayed up, every ping was answered within about 100 ms, and later deliveries
  kept coming promptly (an edit showed on b 1.2–1.3 s after a sent it, after earlier deliveries had gone unanswered).
- **On every new connection, a replay.** When an endpoint connects (and every other endpoint the account still has
  registered: registrations live an hour, and each web page load made a new one), the service delivers again, as
  stored deliveries, the account's non-typing chat events of the last 15 minutes or so that went unanswered: events
  delivered live to an open socket and left unanswered, and events from while the account was offline. A stored
  delivery's `trouter-request` gains `"mstore":true,"flow":"messaging","seq":<n>`, its `Trouter-Timeout` is `7500`,
  and it adds `Connection: keep-alive`; its `id` stays the same from one connect of an endpoint to the next, and each
  endpoint gets its own copy. One run's live, unanswered message, conversation update and edit were replayed at b's
  connects 9, 12.5 and 13.5 minutes later and not at 19 minutes, which fits `ttl=900`; typing
  (`resilience=nostorenoloss`) was never stored. The bursts seen held 3 to 14 events.
- **Under socket churn, retries.** In `ab-trouter-quiet` the web client reconnected about every 10 s (its own
  `user.activity` never got an acknowledgement, below). A live unanswered delivery was then sent again on each new
  socket with a fresh `id` for about 95–120 s, and then joined the stored set, replayed on every connect (36 times in
  about 6 minutes).
- No throttling, no socket closed by the service, no stop of deliveries, and no expected test event missing, in any
  run.

**Decision** (Claude and Codex, 2026-10-20; `discussions/task-14-unanswered-deliveries` in the plan run's state):
no operational harm was shown under these conditions. The cost of not answering is the bounded replay on reconnect,
which the native client absorbs by design: it syncs every chat after every reconnect anyway (Task 17) and stores by
id and version (Task 11), so a replay adds no new state. The answers therefore **stay unanswered** and no response
path is built (Task 15's `DeliveryResponse` is struck); M0's rule stands. What the native client must do instead
(Task 16): apply replays idempotently by conversation, message id and version (the transport's
`X-Microsoft-Skype-Message-ID` is at most a hint) so that neither a replay nor its meeting with the sync produces a
repeated user-visible effect; keep reconnects rare (one endpoint, no `user.activity` acknowledgement to wait for). How
newly found offline messages are notified is M2's.

## `user.activity` and presence

- The web client sends `5:<n>+::{"name":"user.activity","args":[{"state":"active","cv":"<cv>"}]}` right after
  `trouter.connected` (the bundle: again 10 s later, and on every change; only `active` and `inactive`), and the
  service acknowledges it. The bundle shows no idle timer in the Trouter layer: the chat layer reports active when it
  starts and inactive when it stops.
- `ab-trouter-quiet` held every `user.activity` back. The service still delivered live chat events to sockets that
  had never sent one. What changed was the web client: per the bundle it reconnects when its activity event isn't
  acknowledged within 10 s, so it reconnected about every 10 s for the whole run, with the replays above. That is the
  web client's own dependency, not the service's: a client that never sends the event has no acknowledgement to wait
  for.
- **Decision** (same discussion): the native client sends no `user.activity` in M1. Revisit only if a long-lived
  native session without it shows the service closing the socket or delivering less; adding the frame is then an
  ordinary change (it reaches nobody).
- Presence (for M2), not settled here: alongside `user.activity` the web client PUTs its presence endpoint,
  `PUT <teams>/ups/<geo>/v1/me/endpoints/` with `{"id":"<the Trouter epid>","availability":"Available","activity":
  "Available","activityReporting":"Transport","deviceType":"Web"}`, and subscribes to others' presence
  (`POST …/ups/<geo>/v1/pubsub/subscriptions/<epid>`). The fence of the time denied both in every run (they were on the
  run's expected list), and other people's presence reads were denied too, so what makes an account show online (the PUT,
  the activity, or both) was not tested. Others' presence is read since M2 Task 17 and subscribed to since 2026-10-27
  ([below](#others-presence)); the
  account's own was measured in M2 Task 18 ([Own presence](#own-presence-m2-task-18)): the endpoint `PUT` is what
  makes it show online, `user.activity` alone does nothing others see.

## Others' presence

M2 Task 17. Others' presence is read from UPS (GTM `presenceUPS`, `<AFD host>/ups/<geo>`) by `POST
v1/presence/getpresence/`, a read the web client sends as a `POST`.

- **The request** (5461 of them in the captures of 2026-10-19 to 2026-10-21, all denied then): a JSON list of
  `{"mri": …}`, up to 8 seen, the account's own sometimes with `"source": "ups"` beside it; no query; the trailing
  slash; `behavioroverride: redirectAs404`, `x-ms-client-type: cdlworker`, `x-ms-client-user-agent: Teams-V2-Web`, an
  empty `x-ms-object-id`, `Content-Type: application/json`, the client's caller and correlation ids, and on 2065 of
  them `x-ms-endpoint-id` (the client's endpoint). The client asks with a caller per component
  (`presence-sync-strategy:onInterval` about every four minutes, 218–258 s apart; others on a subscription event, on
  Trouter's state, for its own "me" queries). Its token: `https://presence.teams.microsoft.com/`, CAE.
- **The answer** (`ab-chat` of 2026-10-22, the first run that let lab-only reads through: 14 reads, all 200): one item
  per MRI asked and no other, each `mri`, `presence` (`sourceNetwork`: `Self` for the account's own, `SameEnterprise`
  for another; `calendarData.isOutOfOffice`; `capabilities`, empty; `availability`; `activity`; `lastActiveTime`; and
  `source` when asked with it), `etagMatch`, `etag` and the item's `status`, `20000`. Every lab account was
  `Offline`/`Offline` then (nothing sets their presence yet), `lastActiveTime` `0001-01-01T00:00:00Z`.
- **Capture scope:** the early experiments restricted requests to lab MRIs. Current reads use the typed UPS entry with no participant-policy classifier ([chat-service.md](chat-service.md#the-presence-read)).
- **Last seen and out of office** (lab capture of 2026-11-11, the profile card): `lastActiveTime` is an RFC 3339 UTC
  time (`2026-11-11T14:15:38.438Z`) once the account has been active, `0001-01-01T00:00:00Z` for one never seen; the
  web card's "Last seen 4 hours ago" is `Offline` with a `lastActiveTime` four hours back, and plain "Offline" with the
  never value. `calendarData.isOutOfOffice` is a boolean (only `false` captured). Both come in the read's answer and
  in every push alike; the native client reads them into `UserPresence::last_active_ms` (the never value as none) and
  `UserPresence::out_of_office`, in the read and the push (`UserPresence::of_pushed`), and the profile card shows them
  ([loki.md](loki.md#with-presence-and-the-short-profile)).
- **The native client** sends it as `PresenceRead`, a typed `PostRead` through the protocol transport (debug wire logged), by
  MRI alone (`[{"mri": …}]`, never `source`), with the captured header values and its own non-CAE presence token,
  which UPS took (the retired native probe, 2026-10-23: two reads, `Offline` for b and c;
  [auth.md](auth.md#open-questions) question 4). The engine polls it every minute for the people the views asked
  about, a colleague outside the lab included (`teams-core` `presence.rs`): at most 20 watched, the one asked about longest ago dropped (and told
  unknown), at most 8 named per read (the most any of the web client's captured reads named; more watched are read 8
  at a time); someone newly asked about brings the read forward, never within 10 s of the last; a 429 or 503 holds every
  read until its `Retry-After`, counted from the answer, is over. The views show a dot, "unknown" before the first read,
  after a failed one and for someone no longer watched.

### The subscription and its deliveries

Read from the lab's captures of 2026-10-20 to 2026-10-26 (755 subscriptions, allowed since 2026-10-23, and 2971
presence deliveries) and the web bundle `26260.1717.5229.7049` (read locally; notes in our own words).

- **The request**: `POST <presence base>/v1/pubsub/subscriptions/<epid>`, the path ending in the endpoint's `epid`
  (the registration's GUID, the one the endpoint `PUT` names) with **no** trailing slash and no query, the presence
  read's headers plus `x-ms-endpoint-id` (the same `epid`) and the client's caller, version, session and correlation ids.
  The lab's 51 captured subscriptions of 2026-11-04 had a fresh correlation GUID on each request, one session id per
  browser session, and `x-ms-object-id` and `x-ms-request-id` present but empty. Its
  body: exactly `trouterUri` (the messaging socket's `surl` from `trouter.connected`, the `-f` host on port 3443, then
  `unifiedPresenceService`), `shouldPurgePreviousSubscriptions` (a boolean), `subscriptionsToAdd` and
  `subscriptionsToRemove` (each a list of `{"mri": …, "source": "ups"}`; the web client adds its own MRI too). The
  answer, `200`: `{"addStatuses": [{"mri", "source", "status"}], "removeStatuses": […]}`, one item per MRI asked and
  no other, `status` `20200` in every captured one. The bundle names the others: `30010` polling only, `40010` invalid
  source, `40020` MRI not found, `40030` too many subscribers, `40040` too many subscriptions, `50000` error, `50001`
  snapshot failed, `50002` not supported; the web client polls whom UPS didn't take.
- **The lifecycle**: the web client subscribes when its push channel's state changes (connected), the first request
  purging, then more people as it tracks them (`shouldPurgePreviousSubscriptions: false`, only the new ones); it
  removes whom it no longer tracks on a six-minute timer (`subscriptionsToAdd: []`), refreshes the whole subscription
  every 55 minutes, and subscribes again with the purge when its Trouter URL changes. It tracks up to 650 people; the
  lab's captures name 3 at most per subscription (every lab account), so no request size beyond that was seen.
- **The deliveries**: on the messaging socket, `POST /v4/f/<endpoint>/unifiedPresenceService` from
  `Microsoft.Skype.Presence.App/1.0`, `X-Trouter-Delivery-Control: async; ttl=600`, `X-Microsoft-Skype-Message-ID`
  `0`; the body a JSON string of `{"presence": [{"mri", "etag", "source"?, "presence": {"sourceNetwork",
  "calendarData", "capabilities", "availability", "activity", "lastActiveTime"?, "deviceType"?}}]}`, with
  `"isSnapshot": true` on the snapshot UPS sends right after a subscription (everyone it named, as they are), and
  without it on each change (`lastActiveTime` and `calendarData.isOutOfOffice` in the read's forms,
  [above](#others-presence)) (`Available`, `Busy`/`InACall` during a call, `Offline`). The same change often came
  several times over within a run. The `etag` grows with time.
- **No answer needed**: the web client answers every delivery (`3:::{"id",…,"status":200}`), but in the runs of
  2026-10-23 to 2026-10-26 the fence held back most of those answers (2764 of the 2971 deliveries went unanswered),
  and UPS kept delivering: changes came live for the whole of a 49-minute `ab-call` run whose every presence delivery
  went unanswered. The unanswered ones were stored and replayed on later connects (`"mstore":true`, `"flow":""`,
  `Trouter-Timeout: 7500`), as chat deliveries are ([Answering deliveries](#answering-deliveries)). So the native
  client answers none, as it answers no delivery but a call's.
- **Polling beside it**: the bundle's sync reads every four minutes only whom the subscription doesn't cover (and,
  after a message-loss report, everyone); in the captures, once subscriptions went through, the interval reads
  stopped (none in 49 minutes of `ab-call`), while in the runs that denied the subscriptions they came every four
  minutes.

The native client (`teams-core` `presence/subscription.rs`, `teams_proto::PresenceSubscription`,
`teams_trouter::PresenceDelivery`):

- It subscribes the people it watches (those a read may name: never a `28:` or `48:` id, which have no presence)
  once the messaging socket is connected and a registration of
  that connection went through, **in full** (the first request purging, 8 MRIs per request) after every registration
  (a connect's, a reconnect's with its new `surl`, the hourly renewal's), every 55 minutes, and once someone no longer
  watched has stayed subscribed for 6 minutes (left out rather than named in a removal: it sends no removal); anyone
  newly watched is added at once. One subscription at a time, at least 10 s apart; one that failed is tried again
  after a minute (or its `Retry-After`). It uses the typed `PostRead` entry `presence_subscription`, with the presence
  read's header values plus `x-ms-correlation-id` (a fresh GUID for each request), `x-ms-client-version` (the
  registration's client build, `teams_proto::CLIENT_BUILD`) and `x-ms-endpoint-id` (the path's `epid`). It sends no
  caller, session id or cookie, and leaves `x-ms-object-id` empty as the web client does. UPS answers `400` "Missing
  mandatory header '<name>'" without the correlation id, and then without the client version. Until 2026-11-04 every
  try got that answer (a lab account's and a personal build's logs), so no change came live and the reads polled every
  minute. A lab run of that day had a's subscription of a, b and the user's account answered `200` (`20200` each),
  the snapshot delivered, and b's start reached a as a pushed `Available` change.
- A delivery at the endpoint's `unifiedPresenceService` path becomes `PushEvent::Presence`, never answered; the engine
  applies each watched person's availability as a read's would be applied, telling each change once
  (`Event::Presence`). A stored replay is left alone: it is up to ten minutes old, and the connect's subscription
  (whose snapshot follows) and read give the current state.
- The reads go on every minute while the subscription doesn't cover everyone it may name (the socket down, someone
  UPS didn't take, a subscription that failed or not yet answered), and every four minutes while it does; a
  message-loss report brings the next read forward.

## Own presence (M2 Task 18)

What makes a lab account show online to another, and what a manual status sends, measured under the user's
exception of 2026-10-21 to the fence of the time (a lab account could set its own availability, in its captured
shapes, availability values only, every id it named the account itself or its own registered endpoint). Those were
capture-era restrictions. The retained experiment scenarios are
`crates/proto-lab/src/scenario/own_presence.rs` (`own-presence-activity`, `-endpoint`, `-both`, `-manual-held`,
`-manual`): only a's browser runs, as in `ab-trouter` ([above](#capture-setup)), and opens the a–b chat
(the historical capture used a held probe first). b's view came from a retired native presence probe ([Others' presence](#others-presence)) about every 16 s, from before a's browser starts until minutes after it closed (the retired presence sampling script, under the live
lock). Each experiment holds back the component it measures without: `-activity` the endpoint
`PUT`, `-endpoint` every `user.activity` (a selective frame hold), `-manual-held` the manual status; the runs below used
the fence of the time instead ([Capture holds for availability](#capture-holds-for-availability)).

### What the web client sends

- **The endpoint `PUT`**: `PUT <teams origin>/ups/<geo>/v1/me/endpoints/`, no query, a body of exactly
  `{"id","availability","activity","activityReporting":"Transport","deviceType":"Web"}`. `id` is the client's Trouter
  endpoint id: the messaging stack's registration's `registrationId`, its socket's `epid`, and the request's
  `x-ms-endpoint-id`, a lower-case GUID. The pairs seen (403 in the captures of 2026-10-20 to 2026-10-21):
  `Available`/`Available` while the client runs (388) and `Busy`/`InACall` in a call (15, `echo-call`). Its callers
  (`x-ms-client-caller`): `presence-sync-strategy:TrouterStateChanged` (the push channel's state changed),
  `presence-sync-strategy-client` and the two `set-callstate-presence-worker-resolver:setCallStatePresence…`.
  Answered `201` with no body when let through. The web client sends it a few seconds after a Trouter connect, after
  its presence subscriptions (denied) were tried: 11–60 s after the page began in runs 1, 2, 4 and 5 below, and not
  at all in runs 3 (within the 79 s its page was open) and 6–10, the same client version (`1415/26100212011`)
  throughout; what decides it is not known.
- **The manual status** (the account menu's status list, captured with it denied in run 7): `PUT
  <teams origin>/ups/<geo>/v1/me/forceavailability/`, no query, the same headers as the endpoint `PUT` (its
  `x-ms-endpoint-id` the current messaging endpoint), the caller `TeamsClient:setPresenceAvailability` and a body of
  exactly `{"availability": <Busy | DoNotDisturb | Away | Available>}` (the menu's "Busy", "Do not disturb", "Appear
  away", "Available"; three tries each when denied). **"Reset status"** clears it with the same `PUT` and **no body
  at all** (no `postData`, no `hasPostData` in Chrome's event), the caller `TeamsClient:setPresenceAvailability-reset`,
  `Content-Type: application/json` still sent. Each was answered `200` with no body when let through (runs 8–10).
  Choosing "Available" by hand is a set like the others (it forces Available); only the reset clears. In runs 9 and
  10 the client sent nothing for the choices after Busy (Do not disturb, Away, Available) within 20 s of each, while
  run 8 sent all five; its own presence reads, denied (they carry `x-ms-endpoint-id`, [above](#others-presence)),
  may be why. Not driven, so not captured: "Be right back", "Appear offline", a duration
  (`set-availability-duration`), and a status note.
- **`user.activity`** (`active`) on each Trouter socket, as [above](#useractivity-and-presence); the browser fence
  lets it through (a socket.io event).
- When the account menu opens, the client also sends `PUT …/api/mt/part/<partition>/beta/me/engagementSurfaces/
  displayed` (which of its prompts it showed); denied in these runs, it passes under the lab rule.

### What b saw

All `proto-lab run <scenario> --headless` from a normal lab build, under the live lock, 2026-10-22 23:31 to
2026-10-23 05:28 UTC, the machine otherwise idle. Every run's probe was force-denied and failed in a's page; the only
writes the fence let through were a's typing on the a–b chat (2 permits a run) and the own availability below; no
target failed to arm, **no run was breached**.

| run | start (UTC) | scenario | what reported a | b's reads of a |
|---|---|---|---|---|
| 1 | 23:31 | `own-presence-activity` (ok) | `user.activity` (2 sent); the endpoint `PUT` denied (3) | `Offline` throughout: before, during the 5 min the page was open, after |
| 2 | 23:48 | `own-presence-endpoint` (ok) | the endpoint `PUT` (2 answered `201`, two endpoints after a reconnect); every `user.activity` held back (the client reconnected about every 10 s, as in `ab-trouter-quiet`) | `Available` from 4 s after the `PUT` until the page closed; `Offline` 10–25 s after the browser closed |
| 3 | 00:09 | `own-presence-both` (failed: no endpoint `PUT` within the 60 s it then waited) | `user.activity` (2) | `Offline` throughout |
| 4 | 00:25 | `own-presence-both` (ok) | both (`user.activity` 2, the `PUT` 1 answered `201`) | `Available` from 7 s after the `PUT`; still `Available` 5 min 37 s after the browser closed, `Offline` by 5 min 55 s |
| 5 | 00:44 | `own-presence-manual-held` (failed: the status entries' `data-tid`s had changed, no status chosen) | both (the `PUT` answered `201`) | `Available`, then `Offline` about 5 min 50 s after the browser closed |
| 6 | 01:04 | `own-presence-manual-held` (failed: no endpoint `PUT` within 180 s) | `user.activity` | `Offline` throughout |
| 7 | 01:26 | `own-presence-manual-held` (failed on one unexpected deny, `engagementSurfaces/displayed`, since listed) | `user.activity`; no endpoint `PUT` sent; the five manual requests denied (15 with retries: the capture of their shape) | `Offline` throughout |
| 8 | 02:01 | `own-presence-manual` (its exit not kept) | `user.activity`; no endpoint `PUT` sent; the five manual requests let through, each answered `200` | not kept: the run's printed report and b's samples were in `/tmp`, lost in the machine's reboot of 2026-10-23; `fence.log` holds b's 92 reads, let through, without their answers |
| 9 | 05:04 | `own-presence-manual` (ok) | `user.activity`; no endpoint `PUT`; Busy let through (`200`), nothing sent for the next three choices, the reset let through (`200`) 6 min after Busy | `Offline` throughout, Busy included |
| 10 | 05:28 | `own-presence-manual` (ok) | as run 9 | `Offline` throughout, Busy included |

So:

- **The endpoint `PUT` makes an account show online; `user.activity` alone does not** (run 1). One `PUT` held it
  `Available` for the whole 5 minutes the page stayed open (runs 2, 4): no refresh was needed within that time.
- **Going offline** follows the browser's close: within 25 s when no `user.activity` was sent (run 2), about 6 minutes
  when it was (runs 4, 5). Run 2's socket also reconnected every 10 s, so which of the two (the frame or a steady
  socket) holds the endpoint open after its socket closes is not separated. No explicit "offline" request was seen:
  the web client sent none when its page closed.
- **A manual status shows nothing while the account has no live endpoint**: in runs 9 and 10 UPS took Busy (`200`)
  and, 6 minutes later, its reset, and b read `Offline` before, during and after. Whether a manual status overrides
  an endpoint's `Available` as b sees it (what the menu promises) was not observed: the web client sent no endpoint
  `PUT` in any run that let the manual status through. The native client sends its endpoint `PUT` itself (Task 19),
  so its first live runs settle it: Busy set beside a live endpoint, then the reset, each read by b. **Settled by
  Task 19** ([below](#the-native-clients-own-availability-m2-task-19)): it does, Busy shows while the endpoint is
  live, and the reset brings back `Available`.
- **A manual status is cleared** by the reset (the bodiless `PUT`, answered `200`); no other request clears it.
- For the native client (Tasks 19–20): the endpoint `PUT` is needed and enough to show online; `user.activity` isn't
  needed for it, and without it the account went offline within seconds of the client's end rather than minutes.

### The native client's own availability (M2 Task 19)

- **The writes**: `teams_proto::SetEndpointAvailability` (the endpoint `PUT`, `Available`/`Available` only: the
  captured `Busy`/`InACall` is a call's) and `ForceAvailability` (`set` of `Busy`, `DoNotDisturb`, `Away` or
  `Available`; `reset` with no body), typed writes in the captured shapes ([endpoints.md](endpoints.md)).
  The endpoint each names (the body's `id`, and `x-ms-endpoint-id` on both) is a
  `RegisteredEndpoint`, which only `teams_proto::register` gives, once a registration of this run that the account's
  native HTTP transport sent was answered with a success (a sealed `EndpointRegistrar`: no fake transport outside
  `test-support`), recorded for the registering account: the push channel hands it on (`PushEvent::Registered`) and the engine keeps it while the channel
  runs; once the channel stopped, an availability still waiting out a `Retry-After` is dropped and no other is sent.
  They go out as transient writes of the outbox (`EngineRequests::own_availability`), under the presence token and
  the presence read's header values, without the web client's caller, session ids or cookies, which UPS took. No `user.activity` (above: not needed to show online).
- **Live** (the retired availability sampling script, 2026-10-23 07:26–07:35 UTC, under the live lock, from
  a normal retired lab build; a's availability probe and b's presence sampler at roughly 16 s intervals). The first attempt (run 1, 07:15) sent nothing: the
  fence permitted each of its three steps, but the outbox's transport hadn't been given UPS's base, so the transport
  refused them before sending (`no presence base`); b read `Offline` throughout. Fixed then by giving the writes' transport the same UPS base, then run 2:

  | after the `PUT` | a sent (each answered with a success) | b read a as |
  |---|---|---|
  | 0 s (1.2 s after the registration went through) | the endpoint `PUT`, `Available` | `Available` from the first sample after it (12 s) |
  | 119 s | the manual status `Busy` | `Busy` from the first sample after it (6 s) |
  | 239 s | the reset | `Available` at once (the sample taken as it was sent) |
  | 320 s | nothing (the socket still up, its pings answered) | `Offline` from then on |
  | 360 s | (the command ended, its socket closed) | `Offline` throughout the 3 min after |

  `fence.log` holds, for each run, the three `own_availability` permits and nothing denied; b's 72 presence reads
  each a permitted read by `POST`.
- **So**: the native endpoint `PUT` alone makes the account show online, as the browser's did; **a manual status
  overrides it** as another account sees it (Busy beside a live endpoint), and the reset clears it back to the
  endpoint's `Available`. **One endpoint `PUT` lapses on its own**: b read `Offline` 5 min 20 s after it, with the
  push channel up and the manual status cleared. The browser runs of Task 18 kept a page open 5 min after one `PUT`
  and didn't reach that point. A client that wants to stay online sends it again before then (Task 20's policy); what
  interval the web client keeps, and whether a manual status lapses the same way, are open.

### The app's policy (M2 Task 20)

When the app sends which of the writes above: `teams_core::own_presence` (`OwnPresencePolicy`), driven by
`teams-ui`'s `OwnPresence` (held by the desktop for the app's life, told of the window's use by the shell's mouse and
key input, of the push channel by the engine's events, of the manual status and idle time by the settings). Only the
captured requests are used; settled with Codex (2026-10-23, `discussions/task-20-away-offline`):

- **Available**: the endpoint `PUT` once a registration went through, if the window was used within the idle time
  (the app's start counts as use) or a manual status is chosen; sent again every 2 min (`AVAILABLE_REFRESH`), a
  policy number well inside the 5 min 20 s after which one `PUT` was read `Offline`. A registration while idle sends
  nothing; the window used again sends the `PUT` at once.
- **Idle** (the settings' idle time, 5 min by default): nothing is sent; the endpoint lapses on its own, so others see
  `Offline` some minutes later, **not Away**. No captured request reports Away for one endpoint: the manual `Away`
  is account-wide server state (it overrode a live endpoint's `Available` as b saw it, Task 19), so sent for idle it
  would override the account's other devices, clear a status set elsewhere when reset on the next use, and outlive
  the app after a suspend or a crash. How the web client reports idle is open
  (`docs/backlog/own-presence-idle-and-offline-shapes.md`). Cross-device aggregation is inferred, not observed.
- **Quit**: nothing is sent (no captured request reports Offline); the refresh stops, the engine's shutdown closes the
  push channel, and the endpoint goes after it (timing not measured natively). A manual status stays as set.
- **A manual status** (the account menu, or the settings; Automatic and the four captured ones, "Be right back" and
  "Appear offline" not offered): sent once when chosen; Automatic sends the reset, only after a manual status. It
  overrides Available until cleared, and the endpoint keeps being refreshed meanwhile, idle or not (a manual status
  with no live endpoint read `Offline`, runs 9–10). The saved choice is sent once at the run's first registration;
  Automatic at the start sends no reset.
- **A failed report** is shown in the account menu and never sent again on its own (the transient kind's own rules
  are the only retries); a failed `PUT` doesn't bring the refresh forward. A manual status or reset that failed is
  shown as not set (what the account shows isn't known: a failed reset may leave it in its manual status), and the
  user choosing it again in the menu sends it again, a new write through the engine/outbox. A definitive admission refusal stops reporting for that run.

### Capture holds for availability

The `own-presence-*` experiments use selective holds to compare the endpoint PUT, manual availability and
`user.activity`. Historical capture tables above retain their observed statuses and restrictions. Current native
availability writes use the typed outbox path; no browser mapping or member policy runs on UPS.

## Reconnect parameters and backoff (from the bundle)

- Backoff `1000 · 2^n` ms, ±20 % jitter, capped at 300 s for the messaging stack (30 s elsewhere); the count resets
  after a long quiet spell; a connect times out after 20 s.
- The service can disconnect with a reason (`{"reason":…,"claims":…}`): `unauthorized` (reconnect with a fresh token;
  `claims` is a CAE challenge), `skypetoken-deprecated` (stop), `dup` (reconnect at once).
- `trouter.reconnect` `{"target":"self"|"host","url":…}` asks the client to reconnect (make-before-break for `self`,
  to `url` for `host`). Not seen live.

## Token renewal on a live socket

- Not captured live: the web client's `ic3` token lives about 24 hours (CAE), so no run renews it on an open socket.
- Per the bundle the client **never re-authenticates an open socket**: `user.authenticate` is sent once per connect,
  and a renewed token is used at the next connect. An expired token surfaces as a service disconnect with reason
  `unauthorized`, after which the client reconnects with a fresh token.
- For the native client, whose `ic3` token lives about 2 hours: reconnect (a new socket, a new `user.authenticate`
  with the cached `connectparams`, a new registration) before the token expires, and treat an `unauthorized`
  disconnect the same way. Task 16 tests it with a fake clock on the mock; whether the service closes an open socket
  when its token expires is open (it may keep it, since it checked the token once).

## Live runs

All on 2026-10-20 (UTC), `proto-lab run ab-trouter[-quiet] --headless` from a normal build, accounts a and b, the
machine shared with other agents' builds (load average 15–30 for runs 1–9, 2–4 for runs 10–12, about 30 for run
13). Every write the fence allowed in these runs was on the a–b 1:1 chat (messages, typing, edits, reactions, read
receipts), both of whose members
are lab accounts; the probe was force-denied and failed in a's page in every run that got that far.

| run | start | outcome |
|---|---|---|
| 1 | 19:09 | a's browser killed to contain an iframe not armed in time (contained, no breach); failed at a's compose box; the calling stack's registration and presence denied as unexpected (listed afterwards; both pass under the lab rule) |
| 2 | 19:23 | **breached**: b, an iframe whose resume got no answer within 10 s (resume sent); browser killed. Its capture and `fence.log` show nothing sent unguarded (b's 3 frames all allowed, no mutating request answered without a verdict) |
| 3 | 19:29 | failed at a's compose box (60 s; under load) |
| 4 | 19:40 | **breached**: a, two `browser_ui` targets (Chrome's own omnibox pages, not held at attach) whose auto-attach got no answer; b contained a worker. Nothing sent unguarded |
| 5 | 20:13 | message and edit delivered; b's check of the edit failed on a no-break space the editor put in; two unexpected deny classes (activity feed read state, agent invocation; now listed). No breach |
| 6 | 20:24 | failed at a's compose box (load ~30) |
| 7 | 20:28 | **breached**: b, an iframe whose resume got no answer within 10 s; browser killed. Capture and `fence.log`: nothing sent unguarded |
| 8 | 20:29 | a's browser killed to contain a worker (contained); a didn't load |
| 9 | 20:34 | message delivered; the edit's hover landed elsewhere; 16 min observed: no redelivery on the open socket |
| 10 | 20:54 | `ab-trouter-quiet`: message and edit delivered; `user.activity` denied 37 times; the web client reconnected about every 10 s; reaction control not found |
| 11 | 21:00 | message and edit delivered; the connect replayed run 10's unanswered events; 16 min observed: no redelivery |
| 12 | 21:20 | as run 11; the reaction's fallback (the "more options" menu) found no reaction control either |
| 13 | 22:14 | after the review fixes (the hover bar's `message-actions-like`; each of b's documents must get a registration answered `2xx`): **exit 0**, no breach. Message shown on b 12.7 s after a's Enter (under load), edit 1.6 s after it was sent, reaction from the hover bar answered `200` and delivered live to b 0.9 s after it went out; 16 min observed: no redelivery. Registrations: a 2 answered, 36 denied; b 2 answered, 39 denied (the calling stack's retries). `fence.log`: the only allowed writes were a message, typing, an edit and a reaction (2 permits each), all on the a–b chat, no participant added; the other allows were the two accounts' token refreshes, the messaging stack's registrations (8) and Trouter session frames (135); 25 forced denies (the probe and the client's resends of it), every other deny unclassified |

Runs 1–4 were made by an earlier agent on this task before it was cut off; their printed output was kept. Runs 1–12
didn't exit 0: each failed on a step of the web client's UI (a's compose box under load, the reaction control) or on
an arming timeout, never on an unexpected deny after run 5. Run 13, after the reaction step moved to the hover bar's
like, was the first to exit 0. The three breached runs count as breaches (the CLAUDE.md
rule of that capture period: a resume sent, or a target not held, was a breach); the later clean runs are reported next to them, not in their
place.

## Open

- ~~Whether the service takes our own non-CAE `ic3` token on Trouter and the registrar, and the native connect's
  query, `Origin` and registration as built.~~ **Answered (live, M1 Task 19, 2026-10-21): yes**, all of them as built;
  right after each connect the service sent three message-loss reports, each acknowledged after a catch-up that found
  nothing.
- What the service does when a token expires on an open socket.
- The v3 registrar (`registrarUrl`) the service hands over; the web client doesn't use it.
- Whether answering ordinary messaging deliveries would reduce reconnect replay remains unmeasured. The native messaging socket deliberately sends no delivery response; calling replies are typed and connection-bound.
- Whether a long-lived native session without `user.activity` is treated differently (the sockets that never sent
  one here lived seconds to minutes).
- ~~What makes an account show online ([presence](#useractivity-and-presence)).~~ **Answered (live, M2 Task 18,
  2026-10-23)**: UPS's presence endpoint `PUT`; `user.activity` alone doesn't
  ([Own presence](#own-presence-m2-task-18)).
- How often the endpoint `PUT` has to be sent again: one lapsed 5 min 20 s after it with the socket up (M2 Task 19,
  [above](#the-native-clients-own-availability-m2-task-19)); the web client's interval isn't known, nor whether a
  manual status lapses too.
- Why a closed browser that had sent `user.activity` stayed online about 6 minutes, and one that hadn't only seconds
  ([Own presence](#own-presence-m2-task-18)); and why the web client sent no endpoint `PUT` at all in some runs.
