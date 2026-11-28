---
worth: later
where: crates/teams-app/src/signin.rs, crates/ngteams/src/signin.rs, crates/teams-auth/src/broker.rs, crates/teams-auth/src/secrets.rs
added: 2026-11-14
---
# A locked keyring at sign-in is said in plain words

When the Login keyring is locked and its unlock prompt is dismissed (or can't show), a sign-in ends with the raw
chain on the card: "the sign-in stored nothing: the credential broker's store failed: Secret Service: DBus error
Prompt was dismissed". The user isn't told what to do.

Tell a locked or refused credential store apart from other store failures (a typed error from the Secret Service
store, carried through the broker) and say it for the user in `signin::failure` and `signin::own_failure`: that the
keyring is locked, to unlock it, then sign in again. Cover the Windows and macOS stores' own "locked" or "denied"
answers where they have one. Seen with a lab alias on a host whose Login keyring was locked; the personal app
stores through the same path.
