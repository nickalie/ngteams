---
worth: later
status: open
where: crates/teams-ui/src/recording_download.rs (save), crates/ngteams/src/ctl/download.rs (save_all, write_picture)
added: 2026-11-27
---
# A download without --force can still replace a file that came during its own save

Not seen in a run. It is a conclusion from the code. `ngteamsctl download` checks each target (and its `.part`
file) before each file, and refuses a file that is there without `--force`. But the file lands by a rename
(`recording_download::save`, `write_picture`), and a rename replaces a file at the target. A file that another program
or another download puts at the target while a big file downloads is replaced, and the download answers `saved`.

To do: when `force` is off, land the file with a rename that does not replace (`renameat2` with `RENAME_NOREPLACE`
on Linux, `renamex_np` with `RENAME_EXCL` on macOS, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows),
and answer `file_exists` when the target is there. `recording_download` needs an option for it, because the file
cards' Download replaces the file on purpose.
