# Preserve bulk activity read intent across pending item writes

A pending item-unread write on the feed can be throttled while a later bulk read writes its bookmark immediately.
When the older item write eventually succeeds, its newer service version falls beyond that bookmark and the item
becomes unread again. This is a rare, recoverable minor issue: marking the affected item read repairs it.

Resolve this through the outbox, including explicit resends and denied or uncertain outcomes. Either supersede older
pending unread intent safely, or serialize bulk reads after pending item writes and establish the bookmark after they
settle. Merely sharing a lane is insufficient because the bookmark currently uses the bulk row's creation time.
Preserve the rule that an unknown outcome is checked and never blindly resent, and that previous runs' rows are never
resent automatically.

Add an engine/mock regression that throttles an item-unread write, queues bulk read during its retry delay, advances
the retry clock, and verifies the final synced feed is read. Cover a later explicit unread action as well, which must
still win. This needs a separate write-ordering change and its integration tests rather than a local pane adjustment.
