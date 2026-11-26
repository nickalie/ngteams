# Server: a cancelled bot post keeps the pictures it stored

A bot's post or edit through the connector (`post` and `update` in `crates/ngteams-server/src/bots/connector.rs`)
stores each inline `data:` picture as a media object that the bot owns (`bots::pictures::take`). The objects count
against the bot's quota. `Taken::release` frees the objects that no message names only after the post or edit
returns. When the handler stops before that (the bot disconnects, or the node stops during the request), the
objects stay. No message names them, nothing sweeps them, and they stay in the bot's quota.

Keep the ids of the stored objects in a guard whose `Drop` starts the release in a task, and disarm the guard after
the explicit release. Or keep a record of new bot pictures and sweep the ones that no message names after a short
time. Add a test that drops a post after `take` and checks that the bot's usage goes back to zero.
