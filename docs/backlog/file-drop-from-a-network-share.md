# A file dropped from a network share in Nautilus

Not seen in a run. It is a conclusion from the code. Nautilus (GTK 4) offers a dragged file as a document portal key
(`application/vnd.portal.filetransfer`) and as a `text/uri-list`. The Wayland client reads the URI list when it is
offered (`file_drop_mime_type` in `build/patches/gpui-pre-linux/0006-file-drops-from-every-file-manager.patch`). A
file on a GVFS mount (sftp, smb) has an `sftp://` or `smb://` URI with no local path. The drop is thus refused. But
its portal key can give a local path under `/run/user/<uid>/gvfs/`.

To do: when an offer has both types and its URI list gives no local path, read the portal key next. Use the same
generation check. Refuse the offer only when the key also gives no files. First, make sure in the sandbox (or with a
real GVFS mount) that the portal key gives such a path. Add a source like that to the file managers of
`crates/teams-ui/tests/support/linux/attachment_window.rs`.
