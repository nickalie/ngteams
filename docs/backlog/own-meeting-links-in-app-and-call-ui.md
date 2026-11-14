---
worth: yes
where: crates/ngteams/src/ctl/links.rs, crates/teams-ui/src/calls.rs
added: 2026-11-14
---
# Use the account cloud for app meeting links and the meeting chat handoff

Typed calling and meeting readers support Own origins, but the app's `Link::of` still filters for Microsoft links
and calls `join_url_chat` and `join_url_meeting`. The call UI's `Calls::resolve_meeting` also requires a Microsoft
meetup-join prefix and uses those Microsoft readers. An Own join link therefore cannot open a meeting pre-join
through `open-link`, or resolve a code-based call into its meeting chat. A Microsoft link may open a pre-join under
an Own account instead of being refused for that account.

No production account selects Own on this branch. This needs the account-cloud integration and sign-in work,
including a cloud exposed to the app's account target and the call UI, or validated meeting details supplied by
the engine. Neither UI model currently carries a cloud. This is deferred from task 192's first review because it
crosses the app/account/UI construction boundary and needs their existing pre-join and handoff tests, beyond a
typed calling review fix. Complete it before enabling Own accounts in the app.

Pass the selected account's cloud to the app's link reader and use the `_in` meeting readers with the filter in
`teams-proto/src/meeting.rs`. Resolve a call's meeting reference using the same account cloud or engine-validated
chat and organizer, retaining the current matching-chat check.

Acceptance: an Own join link on the exact origin, including its port, opens that account's pre-join and resolves
its code-based call's matching meeting chat. Microsoft, another host or port, and insecure links are refused for
Own accounts. Microsoft and `msteams:` handling retain their existing behavior for Microsoft accounts. Extend
the existing `ctl_links` and meeting-resolution cases, then verify the pre-join and meeting-chat handoff in the
UI sandbox.
