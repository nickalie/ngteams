# A channel header can keep Join while CSA still names an ended meeting

When a channel's post tells that its meeting ended, the shell reads the channel's meeting again at once
(`Shell::reread_channel_meetup`). That read (`find_channel_meetup` in `crates/teams-core/src/calendar.rs`) uses CSA's
`activeMeetups` first and reads the posts only when CSA names no meeting. If CSA removes an ended meeting later than
the `callEnded` post comes, the header keeps Join until the next timed read (`CHANNEL_MEETUP_REFRESH`, 30 s).

No capture shows how soon CSA removes an ended meeting. CI and the own server use only the posts path. Before a fix,
capture CSA's teams read just after a channel meeting ends, with a lab account that CSA tells the meeting to.

If CSA is late, let a `callEnded` post win: `ChatViewEvent::MeetingCallsChanged` names the running posts, the shell
removes `channel_meetup` at once when its `message_id` is not one of them, and it ignores a later read that names a
post that the rows show as ended.
