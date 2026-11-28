---
worth: later
status: open
where: crates/ngteams/tests/ctl_calendar_cli.rs, crates/ngteams/tests/ctl_card_text_cli.rs, crates/ngteams/tests/ctl_download_cli.rs, crates/ngteams/tests/ctl_support
added: 2026-11-28
---
# The ngteamsctl child-process harness is in three test files

Each `*_cli.rs` test has its own copy of the child test `ngteamsctl_child`, the `Run` struct, the `ngteamsctl()`
launcher, the `Credentials` stub and `started()`. A fix to the child runner must go into each copy, and the copies can
become different.

To do: move `Run`, the launcher (with the names of its environment variables as arguments) and the body of the child
test into `ctl_support`. Each `*_cli.rs` then keeps only its `#[test] #[ignore] fn ngteamsctl_child()`, which calls
the shared body.
