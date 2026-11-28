---
worth: later
where: crates/teams-app/src/signin_bound/classify.rs (SIGN_IN, QueryRule::Any)
added: 2026-10-21
---
# the sign-in window's rules let Entra's sign-in form out whatever its query

The sign-in window (`teams-signin`) keeps default-deny rules of its own (`teams_signin::classify`, AGENTS.md): reads,
the Entra token endpoint in its two captured parameter sets (the request id alone, or with the broker's three
parameters, each once and in its captured value forms), Teams' authz endpoints with no query, and Entra's sign-in form
(`/*/GetCredentialType`, `/*/login`, `/kmsi`, `/*/kmsi`, `OwnKind::EntraSignIn`). The form still takes any query
(`QueryRule::Any`), because no capture has shown its queries yet. `docs/personal-build.md` keeps these rules for the
personal sign-in window and asks, for both builds, that an allowance match the request's exact captured shape, query
included.

The lab browser no longer has the gap: under the lab rule Entra is no contacting service, so `proto-lab login` lets
every request to it through, logged `allowed` when it is mutating. Only the sign-in window's rules are left.

`login` records its run under `captures/login/<ts>/` (with no request body: the form's posts carry the password), so
the next time a lab session expires and `login` runs, its capture shows the form's queries.

Suggested fix: after a `login` run, read its capture's sign-in form requests (method, path, query keys and value
forms), give each `SIGN_IN` rule that query set (`QueryRule::OneOf`) in place of `QueryRule::Any`, with tests that any
other key set or a repeated key is `Unknown`. Not done blind: a rule narrowed without a capture could break the first
sign-in that needs it.

Found by the revmux review of M1 Task 14 (round 2, `docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`);
cut to the sign-in form by M2 Task 1; to the sign-in window by Task 7 of
`docs/plans/20261023-lab-fence-contacting-writes-only.md`.
