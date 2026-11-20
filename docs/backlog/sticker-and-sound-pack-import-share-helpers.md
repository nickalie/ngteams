---
worth: later
status: open
where: crates/teams-ui/src/stickers/import.rs, crates/teams-ui/src/sounds/pack.rs
added: 2026-11-20
---
# The sticker import and the sound-pack import have copies of the same helpers

The sticker store and import (`crates/teams-ui/src/stickers/`) copy parts of the sound-pack store
(`crates/teams-ui/src/sounds/pack.rs`): the id check (`store::is_id`), the hidden build directory and its `Drop`
(`Building`), the `IMPORTS` counter and the stage, rename and mark-done steps, `read_limited`, `free_id`, `ImportKind`
and `MAX_ID`. The copies are already different: `free_id` cuts the slug by bytes in one and by chars in the other, and
`remove` of a path that is not a directory gives `NoPack` in one and `Ok(())` in the other. A fix in one copy does not
go into the other.

To do: move the shared parts into one teams-ui module that both stores call, with the differences of each domain as
parameters. Keep each store's error texts. The review of task 653 (round 1) found this; it was too large for that
review.
