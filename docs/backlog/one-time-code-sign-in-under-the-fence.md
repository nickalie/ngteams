---
worth: later
where: crates/teams-app/src/signin_bound/classify.rs (SIGN_IN), crates/proto-lab/src/commands/login.rs
added: 2026-10-20
---
# Can a sign-in enter a one-time code?

Found by the review of M0 Task 8 (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`). The plan asked `login` to
write `need-code` to `status-<alias>.json` and wait for the code in `inbox/<alias>.code` when Microsoft asks for one.
Entering the code needs Entra's multi-factor posts on `login.microsoftonline.com` (`SAS/BeginAuth`, `SAS/EndAuth`,
`SAS/ProcessAuth`).

**The lab browser** lets them through now: under the lab rule Entra is no contacting service, and a multi-factor post
reaches only the account's own registered device. What still stops `proto-lab login` is `login` itself: it stops at a
one-time-code prompt (`SignInStop::MfaCode`) like at every other multi-factor prompt, and has no step that types a
code in. Adding one is an ordinary change now; whether it is wanted depends on a lab account ever being asked for a
code (none has been, as of 2026-10-23).

**The sign-in window** (`teams-signin`) still denies them: its rules (`teams_signin::classify`, `Classifier::sign_in`)
take the token endpoint, authz and Entra's sign-in form only, and nothing of multi-factor sign-in. The personal build
plans to add the multi-factor and federation return paths to its own sign-in window (`docs/personal-build.md`); the
lab's window has no need for them while lab accounts sign in through `proto-lab auth`. Before a rule for them is
written, these were the open questions (decided with Codex, transcript under the M0 run's `discussions/fixer-task8-otc/`):

- what each request does: BeginAuth starts the challenge as soon as the page shows the prompt (a text, a voice call or
  an Authenticator push for the default method); who receives it (the account's registered device, and whose that is);
- how the method is chosen (the request bodies), since an own-state rule matches method, origin, path and query only;
- that the same paths never carry registration or security-info changes.

The unknown that settles them: a capture of an Entra one-time-code sign-in for a lab account (`proto-lab login`
records its runs under `captures/login/`).
