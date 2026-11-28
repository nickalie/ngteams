# The calendar

Status: 2026-10-30. Written from the web client's captures of its calendar read (`ab-chat`, `load`, 2026-10-20 to
2026-10-24: method, path, query names and value forms, header names, statuses and the answer's key paths only) and from
the field names and value lists the web client's code reads an event by (the desktop client's bundle the lab keeps,
read locally; nothing of it copied). The lab accounts' calendars are empty, so every captured answer held no event: an
event's value forms below are the bundle's, not a capture's, and the native reader takes them leniently. The native
client's read is in `crates/teams-proto/src/calendar.rs` (`CalendarView`), its use in
`crates/teams-core/src/calendar.rs`, and the view in `crates/teams-ui/src/{calendar.rs,calendar_view.rs}`.

## Where the web client reads it

The web client reads the account's calendar from the middle tier (`Service::MiddleTier`, the region's GTM
`middleTier`, `https://<AFD host>/api/mt/part/<partition>`), with the account's token for
`https://api.spaces.skype.com`:

- `GET v2.0/me/calendars/default/calendarView?StartDate=<ISO>&EndDate=<ISO>&shouldDecryptData=true`, the query in that
  order, the window's ends in UTC with milliseconds and `Z` (`2026-10-24T21:00:00.000Z`: local midnight of the first
  and of the day after the last day shown). Its captured headers besides the token: `x-ms-client-caller`
  (`calling-cal-adapter`), `x-ms-client-requesttype`, `x-ms-client-type` (`web`), `x-ms-client-version`,
  `x-ms-migration: True`. The captures show windows of three and six days.
- It answered `200` with `{"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": []}`: every event
  of the window in `value`, no paging key.

The bundle names more of the same service's calendar paths; the event by id, the scheduling form's reads and an
event's writes are described below. The rest (an event by its iCal uid, by its join link, a group's or a connected
account's calendar view, forward and import) were never captured, and the native client uses none of them.

## An event

The fields the web client selects for an event, with what the native client reads (the bundle's value lists for the
enums):

| Field | What it is | Read as |
|---|---|---|
| `objectId` | the event's id | its id; an event without one is skipped |
| `subject` | its title (a cancelled meeting's usually renamed `Canceled: …` by the service) | text, empty when absent |
| `startTime`, `endTime` | its start and end, an ISO date and time | with an offset: an instant; without one: a local time, as a JavaScript `Date` parses it; an event whose times don't read is skipped |
| `isAllDayEvent` | all day | a flag |
| `eventType` | `Single`, `Occurrence`, `Exception`, `RecurringMaster` | an occurrence or exception is drawn with the recurring mark; a master is left out (its occurrences come on their own) |
| `isCancelled` | cancelled | a flag: the event is drawn outlined |
| `isOnlineMeeting`, `skypeTeamsMeetingUrl`, `joinOnlineMeetingUrl`, `shortOnlineMeetingJoinUrl` | a Teams meeting and its join link | the first `https` link of the two first fields |
| `skypeTeamsData` | a JSON object as a string; its `cid` is the meeting's chat (`19:meeting_…@thread.v2`) | the meeting's chat, else the thread id the join link's path names (`/l/meetup-join/<thread, percent-encoded>/…`) |
| `organizerName`, `organizerAddress`, `isOrganizer` | the organizer | text and a flag |
| `myResponseType` | `None`, `Organizer`, `Accepted`, `Tentative`, `Declined`, `NotResponded` | a declined meeting isn't shown (as in Teams); tentative and not answered are drawn with a striped edge |
| `showAs` | `Free`, `Tentative`, `Busy`, `OOF`, `WorkingElsewhere`, `Unknown` | free and tentative drawn striped |
| `location` | where | text (or an object's `displayName`) |
| `attendees` | who is invited | each one's name and address, when readable; `type` `Optional` an optional one; `status.response` (`Accepted`, `TentativelyAccepted`, `Declined`, `None`, …) their answer. The calendar view's events carried no `attendees` at all on 2026-11-06 (a meeting a→c read by both); the event read whole carries them with each one's `status` (`{"response", "time"}`) |
| `isPrivate`, `bodyPreview`, `onlineMeetingConferenceId` | private, the start of its text, the dial-in id | read; the text is shown in the event's card |
| `seriesMasterId` | an occurrence's or exception's series (its master's id) | the event the scheduling form reads and writes to change the whole series |

Other selected fields (`categories`, `cleanGlobalObjectId`, `iCalUid`, `lastModifiedTime`,
`schedulingServiceUpdateUrl`, `meetingTemplateId`, the dial-in numbers, …) aren't read by the calendar view;
`eventTimeZone` is read by the scheduling form (under "Time zones" below).

## What the native client does with it

- **The calendar view's read**: one read per window a view shows, when it changes, on Refresh and every five minutes
  while the calendar is shown (the writes are under "Writing an event" below). A window wider than 62 days is never
  asked for.
- **Kept between runs**: each answer replaces, in the account's store, the events it held that overlap the window
  (each event kept as its entry came, read again by the same reader), so a window not read yet in this run is drawn
  from the store at once and the answer then applies on top: events added, changed, moved, cancelled or gone. A
  failed read keeps what was stored. A write (a change, a cancellation, a delete, an answer) is settled in the store
  before the view hears it went through: every kept copy of the event it names is forgotten, in whatever window it
  was kept, a series' occurrences too when it names the master (by `seriesMasterId`), a channel's meeting by its
  `iCalUID`; a new single event is kept as its creation's answer gave it. A window read that began before the write
  was over isn't kept, as it may hold the event as it was. The web client keeps a calendar cache of its own too: the lab profiles' local
  storage holds its calendar sync state (a `CalendarScenario` and a `CalendarSyncScenario` with a version and a stale
  flag, and an "up to date" marker), which the lab's empty calendars never filled with events, so its exact rules
  (how much it keeps, when it marks it stale) weren't seen.
- **Joining**: a meeting's Join is there at any time, started or not ([calling.md](calling.md#joining-a-meeting)): its
  chat comes from `skypeTeamsData`'s `cid` or the join link's path, its organizer and tenant from the link's `context`
  (`Oid`, `Tid`), a series' occurrences all naming the series' one chat. It opens the app's pre-join; only its Join now
  goes out. An event whose link doesn't name them (a short `/meet/<code>` one) is joined by its chat: the chat's thread
  names its organizer ([calling.md](calling.md#joining-a-meeting)); only an event with neither opens its link in the
  browser. "Join with an ID" finds the meeting by the ID and passcode its short link (`shortOnlineMeetingJoinUrl`, or
  the join link) carries, among the events kept from the last two months and the next two, and joins it as its card
  does. Writing an event is below.
- **Join's menu**, as the calendar Teams embeds (Outlook's) has it: Join and Copy join link. Read from Outlook's public
  scripts (`JoinMeetingSplitButton`'s two items), since the lab's calendar app didn't start under the fence (its
  `POST /owa/startupdata.ashx` was on a contacting service with no row; a carve-out since 2026-11-03). It copies the event's join link.

## Reminders

An event's reminder is its `isReminderSet` and `reminderMinutesBeforeStart` (an event read whole carries both; the
lab's calendar view answers, 2026-11-06, carried neither, though the web client's reader copies them when present).
Outlook gives an invitee's copy the invitee's own default reminder, 15 minutes; the scheduling form's default is 15
minutes too.

The web client's own reminder (read 2026-11-19 from its bundle) is behind a remote switch that is off by default
(`enableMeetingReminder`): when on, it shows one toast 10 minutes before an accepted or organized online meeting
(not the event's reminder time), only when the app runs at that moment, with a button that opens the meeting's chat;
no snooze. The desktop Outlook-style reminder the user knows from Teams on Windows and Mac is not in the web bundle.

The native client reminds as Outlook does, by the event's own reminder (`teams_ui::meeting_reminder`): an online
meeting, neither cancelled nor declined nor all day, whose reminder is set. The view's event lacking the fields is
read whole, and again every 5 minutes while it starts within 2 hours (or within its reminder, when longer) so that a
reminder changed during the run is seen; a meeting that ended is not read. Until the first read answers nothing is
shown, a failed first read leaves the default 15 minutes, and a failed read again keeps the last one. From the
reminder time until the start the notice says "Starts in N minutes", and at the start "Starting now" (in the same
notice's place), until the meeting ends; a run that starts after the reminder time reminds at once, one that starts
more than 10 minutes after the start doesn't. Join (the calendar's Join), Snooze (the desktop notice's for 5 minutes;
the window card's 1, 5, 10 or 15 minutes, or until it starts) and Dismiss; a meeting that moves is reminded anew, its
old notice closed, and one cancelled, declined or deleted closes its notice. The calendar is read from the service
every 5 minutes (and the store every 15 s), so the reminders don't need the calendar view. Settings → Notifications →
Meetings → "Meeting reminders" turns them off; notifications off and Do not disturb show none.

## Writing an event

Written 2026-10-31 from the web client's code (the bundle the lab keeps, read locally) and the `ab-meeting` captures
of that day (method, path, query, header names, body keys and value forms, statuses and the answers' key paths). The
runs captured a's Teams meeting with b made from the scheduling form, and what the form sends around it; the change
and the cancellation the scenario goes on to make (from the meeting's chat, "Details") weren't reached in the runs of
that day (the web client stopped answering on a loaded machine), so their shapes below are the bundle's. The lab's calendar
app is Outlook's, embedded (its writes go to Outlook's own service and were held in those captures); the
scheduling form the web client opens on its new-meeting deep link (`/l/meeting/new?subject&attendees&startTime&endTime`)
and on a meeting's chat ("Details") is Teams' own, and writes through the middle tier. The native client writes as
that form does.

Every write goes to the middle tier with the account's `api.spaces.skype.com` token, `Content-Type:
application/json`, and the web client's `x-ms-client-*` headers beside it (`x-ms-client-caller` naming the form's
operation, `x-ms-client-type: cdlworker`, `x-ms-migration: True`, `x-client-ui-language`).

### Before the save

- `GET beta/me/calendarEvents/timeZoneSettingsWithOffset?timezone=<IANA zone>` (a read): the desktop's zone as
  Exchange names it, `{"timeZone", "utcOffsetInMinutes", "local": {"timeZone", "utcOffsetInMinutes"}, "remote": {…}}`.
  An event carries `local.timeZone` (`TurkeySt` for `Europe/Istanbul`) as its `eventTimeZone`.
- `PUT v2.0/me/calendars/findMeetingTimes` and `PUT v2.0/me/calendars/schedules` (reads sent as a `PUT`, the people
  asked about and the window in the body): the scheduling assistant's free and busy times, sent as attendees are
  added. They reach nobody: the lab rule lets them through. Their shapes are under "The Scheduler" below.
- `GET v2.0/me/calendars/masterCategories` (a read): the account's categories, `[{"id", "displayName", "color"}]` (a
  new mailbox has Outlook's six, red to purple).

### A Teams meeting's online meeting

`POST beta/me/calendarEvents/privateMeeting/schedulingService/create` with `{"isStreamEnabled": false,
"meetingType": "Scheduled"}` (no query): the meeting's join link and chat, with only the organizer in it, made when
the form opens on a Teams meeting. It answered `201` with `{"value": {"expiryTime", "groupContext": {"threadId":
"19:meeting_…@thread.v2"}, "etag", "meetingUrl", "links": {"join", "update", "shortJoinUrl"}, "views": {"html": <the
invitation's join block as a data URL>}, "onlineMeetingConfLink", "onlineMeetingConferenceId", …}}`. It reaches nobody
(no attendee is named).

On closing, the form deleted the online meeting it had made, even the one its saved event had just taken: `PUT
beta/me/calendarEvents/schedulingService/delete` with `{"links": {"update": <its update link>}}`. That delete is not implemented by the native client.

### Creating

`POST v2.0/me/calendars/default/events?isOnlineMeeting=<true|false>&shouldDecryptData=true` with:

- `calendarEvent`: `subject`; `startTime` and `endTime` in local time with their offset, no fraction
  (`2026-10-31T01:30:00+03:00`); `eventTimeZone` and `utcOffset` (minutes); `isAllDayEvent`; `attendees`, the organizer
  first (`{"address", "mri", "name", "role": "Admin", "type": "Organizer"}`; the form's first state had it with no
  address), then each invitee (`{"address", "mri", "name", "role": "Admin", "type": "Required"|"Optional"}`);
  `location` (text); `showAs`; `isReminderSet` and `reminderMinutesBeforeStart`; `isPrivate`; `isResponseRequested`;
  `isOrganizer`; `isAppointment` (`true` while it invites nobody); `eventType: "Single"`; `doNotForward`;
  `organizerAddress` and `organizerName`; `primarySpokenLanguage`; `clientOptimisticEventId` and `TransactionId` (one
  id, five random 32-bit numbers joined by `-`); for a Teams meeting `joinMeetingLabel`, `meetingTemplateId:
  "default"` and `skypeTeamsData` (`{"uid": <organizer MRI>, "type": 0, "private": true}` as a string); the text as
  `bodyContentType` and `bodyContent` when it has one (the form left both out of an empty one, with
  `emptyBodyPlaceholder`);
- `schedulingServiceEvent`: for a Teams meeting, the online meeting made before, as its answer gave it.

It answered `201` with `{"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": [<the event>]}`:
the event as the calendar view reads it (`objectId`, times in UTC, `skypeTeamsData` naming the meeting's chat,
`shortOnlineMeetingJoinUrl`, `bodyPreview` holding the invitation's join block, `location: "Microsoft Teams
Meeting"`).

### Changing, cancelling, deleting

- `GET v2.0/me/calendars/events/<id>?shouldDecryptData=true` (a read): one event whole, as the form opens on it.
  `ngteamsctl calendar show` uses the same read. For an occurrence, it also reads the series master, because only
  the master carries the recurrence.
- `PUT v2.0/me/calendars/events/<id>?isOnlineMeeting=<bool>` with `{"updatedCalendarEvent": <the event>}`: the
  event as read with the form's fields over it; its update goes to every attendee (and a cancellation to one taken
  off). The bundle expects `204`.
- `PUT v2.0/me/calendars/events/<id>/cancel` with `{"conversationId"?: <the meeting's chat>, "cancellationMessage"?:
  <text>}`: the organizer's meeting cancelled, each attendee told. `204`.
- `DELETE v2.0/me/calendars/events/<id>` (no body): the event taken off the account's calendar (an appointment, a
  meeting its organizer cancelled). `204`.

- `PUT v2.0/me/calendars/events/<id>/respond` with `{"responseType": "Accept"|"TentativelyAccept"|"Decline",
  "sendResponse": <bool>, "comment"?: <text>}`: the account's answer to an invitation (the bundle's
  `updateMeetingResponse`, which also may send `attendance`; read from the web client's code kept with the lab's
  references). The id is an occurrence's for that occurrence, its series master's for the whole series. `204`.
  Checked live on 2026-11-06 by the native client: c accepted a's weekly series (by its master, with a note), and a's
  event read whole then gave c's `status.response` `Accepted` on each occurrence.

Checked live on 2026-11-06 by the native client (lab accounts a and c only): the change (`204`), the organizer's
cancellation of one occurrence and of the whole series by its master's id, each with `cancellationMessage` (`204`; the
organizer's calendar then holds neither, c's holds each as `isCancelled` and `Canceled: …`), and c's delete of a
cancelled occurrence and of the cancelled series' master (`204`).

The bundle also names an event's forward, a channel meeting's writes and a webinar's; none was captured, and the native
client sends none.

### A series

Written from the bundle (its scheduling form, its recurrence model and the other callers that build an event), and
checked live on 2026-10-31 by the native client (a retired native series probe: an appointment series of lab account a's
own, weekly, three weeks): the creation in the form below was answered with a 2xx and the master (`eventType`
`RecurringMaster`); the calendar view then gave its four occurrences (`Occurrence`, each with `seriesMasterId`); the
master read whole gave the recurrence back by names (`{"patternType": "Weekly", "weekly": {"daysOfTheWeek":
["Sunday"], "interval": 1}}`) and the range as written (`{"startDate": "2026-11-01T00:00:00+00:00", "endDate":
"2026-11-22T00:00:00+00:00"}`), beside `recurrencePattern` and `recurrenceEnd`. An appointment (nobody invited) reads
back with no `attendees` key at all. A repeating event is one more pair of fields on the event the creation and the
change send:

- `eventRecurrencePattern`: `{"patternType": <n>, "<kind>": {…}}`, the pattern types numbered as the model numbers
  them (1 daily, 2 weekly, 3 monthly, 4 relative monthly, 5 yearly, 6 relative yearly; 0 none) and the kind's object
  beside it: `daily: {interval}`, `weekly: {interval, daysOfTheWeek: [<day>…]}` (0 Sunday to 6 Saturday),
  `monthly: {interval, dayOfMonth}`, `relativeMonthly: {interval, dayOfTheWeek, dayOfTheWeekIndex}` (0 first to 4
  last), `yearly: {month, dayOfTheMonth}` (month 1 to 12), `relativeYearly: {month, dayOfTheWeek,
  dayOfTheWeekIndex}`. A read may give the same values as names or as numbers in strings (the web client turns them
  into numbers as it reads them).
- `eventRecurrenceRange`: `{"startDate", "endDate"?}`, the series' first day as its local midnight in ISO with its
  offset (`2026-10-27T00:00:00+03:00`, date-fns' `formatISO` of the start's day), and its last day the same way when
  it ends (none: it never ends).
- `eventType`: the form names a new event `RecurringMaster` when it has both fields, else `Single`; a change keeps the
  event's own.

The form's repeat drop-down offers Does not repeat, Every weekday (Mon - Fri) (weekly on Monday to Friday), Daily,
Weekly (on the start's weekday), Monthly (relative: the start's weekday of its week of the month, the last when the
start is in the month's last seven days), Yearly (the start's month and day) and Custom (its own dialog: repeat every
n days, weeks on chosen days, months on a day or a weekday, years; an end date). A preset's range starts on the
start's day and never ends.

The calendar view gives a series as its occurrences (`eventType` `Occurrence`, or `Exception` for one changed on its
own), each naming its series by `seriesMasterId`. A change, a cancellation or a delete of the occurrence's own id acts
on that occurrence; of the master's id, on the whole series (the master read whole first, `GET
v2.0/me/calendars/events/<master id>`, which carries the recurrence). The update addresses the event or series master named by its path.

### The Scheduler

Captured in `ab-meeting` (2026-10-30, the web client's form as attendees were added): the two free/busy reads, sent
as a `PUT` on the middle tier with the same body, `{"attendees": [<attendee>…, <organizer>], "minimumSuggestionQuality":
"Good", "numberOfDays", "requestType", "startDate", "startTime", "startDateTimeZone": "Utc",
"meetingDurationInMinutes", "endTime"}`: each attendee as the event lists it (`{"type": "Required"|"Optional",
"address", "name", "mri", "role": "Admin"}`), the organizer last (`"type": "Organizer"`); `startDate` and `startTime`
the local midnight of the form's day in UTC with milliseconds, `endTime` that and `numberOfDays` days; the meeting's
length in minutes.

- `PUT v2.0/me/calendars/findMeetingTimes` with `requestType: "SuggestedTimesOnly"` and two days: `200` with
  `{"timeSuggestions": [{"meetingTime": "2026-10-30T05:00:00+00:00", "confidence": 100.0}…]}`, the starts every
  attendee is free at for the meeting's length, every half hour.
- `PUT v2.0/me/calendars/schedules` with `requestType: "AttendeeAvailabilityOnly"` and five days: `200` with
  `[{"userPrincipalName", "timeZone"?, "timeZoneOffset"?, "timeZoneName"?, "freeBusyEvents": [{"freeBusyStatus":
  "Busy"|"Tentative"|…, "startTime", "endTime", "details": {"subject"?, "location"?, "isPrivate"?}}]}]`, one entry
  per person asked about; the details (and the time zone) only for the account's own calendar.

### Time zones

Behind the globe beside its times, the web client's form shows the start's date, time and time zone on one line and
the end's under them ("(UTC+03:00) Istanbul"), seen 2026-11-06 (`team-manage`, the hosted Outlook form). The middle
tier's event carries one zone, `eventTimeZone` (with `utcOffset`): the start's, by its Exchange name, which
`timeZoneSettingsWithOffset?timezone=<IANA zone>` gives (`Pacific/Honolulu`: `local.timeZone` `HawaiianSt`). The
service reads `startTime` and `endTime` as that zone's clock whatever offset they carry: a native save of 2026-11-06
with the start in Honolulu (`-10:00`) and the end written with Istanbul's offset (`13:00+03:00`) came back ending at
13:00 Honolulu time. So both are written in the start's zone; an end picked in another zone is converted to it.

An event read whole names its zone the same way, `eventTimeZone` with the start's `utcOffset` (`"HawaiianSt"`,
`-600.0` on 2026-11-06), and nothing more: no `originalStartTimeZone` or end zone, the master and its occurrences
alike. The names are the Windows zones in the service's short form (`<Name>St` for "<Name> Standard Time",
`UtcMinus11`, `RussiaTimeZone3`, `GTBSt`, and the odd `VenezulaSt`, `IndianSt`, `NAsiaSt`), the web client listing 140
of them; the native client reads each as the IANA zone of its Windows zone's main place.

A zone field is a combobox: focused, it empties to "Search for a city" and typing lists matching cities with their
zone ("hono": Honolulu, Hawaii (HST), Honokaa, Hawaii, …), seen 2026-11-06. Picking a zone there keeps the date and
time its line shows and reads them in the new zone (Outlook's form, 2026-11-06, `team-manage`, the save held unsent):

- the start's zone: 10:00 to 10:30 in Istanbul, the start zone made Hawaii, was 10:00 to 10:30 in Hawaii, the end's
  zone changed with it; with the end already in a zone of its own (London), the start's 4:00 PM became Tokyo's and
  the end stayed 4:30 PM in London;
- the end's zone: 4:00 to 4:30 PM in Istanbul, the end zone made London, was 4:00 PM Istanbul to 4:30 PM London; an
  end that came before the start that way (4:30 PM at Kiritimati, UTC+14, against a 4:00 PM Tokyo start) was made the
  start ("4:00 PM - 4:00 PM", shown 9:00 PM at Kiritimati). The held `CreateCalendarEvent` carried `Start`
  `2026-11-06T16:00:00.000` with `StartTimeZoneId` `Tokyo Standard Time` and `End` `2026-11-06T21:00:00.000` with
  `EndTimeZoneId` `Line Islands Standard Time`.

Outlook's form opens an event in its own zone; the native form opens a change in the zone its read names, on both
lines (the read gives no end zone), so its times show there and a series keeps its weekday.

A series repeats in the start's zone. Outlook's form (2026-11-06, start zone Hawaii, the save held unsent) said
"Occurs every Friday from 10:00 AM to 10:30 AM effective Fri 11/6/2026 until Fri 4/30/2027 (UTC-10)" and its
`CreateCalendarEvent` carried `StartTimeZoneId` `Hawaiian Standard Time`, the weekly pattern's `DaysOfWeek` `Friday`
and the range's `StartDate` `2026-11-06`: the weekday and the days are the start zone's, whatever day the desktop is on.
The native client writes `eventRecurrenceRange`'s days as that zone's midnights with its offset, and takes a preset's
weekday, week and date from the start's day there. Checked live 2026-11-06 with a series of lab account a's own: a
weekly series starting Monday 21:00 in Honolulu (Tuesday 10:00 on an Istanbul desktop), sent with `eventTimeZone`
`HawaiianSt`, `daysOfTheWeek` `[1]` and `startDate` `2026-11-09T00:00:00-10:00`, was answered `201`, and the calendar
view gave its occurrences at `07:00:00+00:00` on the Tuesdays, Monday 21:00 there.

### Meet now

The calendar's toolbar has a split **Meet now** (its menu: Join with an ID). Its face opens "Start a meeting now" with
the meeting's name ("Meeting with <first name>") and two buttons; both make the meeting with `POST
beta/me/calendarEvents/privateMeeting/schedulingService/create` and `{"meetingType": "MeetNow", "isStreamEnabled":
false, "subject": <name>, "unhideChatThread": true}` (captured 2026-11-06, `meeting-steps`, answered `201` with the
meeting: `subject`, `groupContext.threadId`, `links.join`, `links.shortJoinUrl`). **Start meeting** then opens the
meeting's pre-join; **Get a link to share** shows the meeting's name over its short link
(`https://teams.microsoft.com/meet/<id>?p=<passcode>`) with Copy, "Configure meeting options", Start meeting and
"Share via email". No event is written to the calendar, and nobody is invited.

### Other people's calendars

The left pane's **Add calendar** opens a dialog of sections (Recommended, Edit my calendars, Create blank calendar, Add
from directory, Subscribe from web, Upload from file, Holidays, Birthdays). **Add from directory** finds a person by
name or address and adds their calendar under "People's calendars" (Outlook's `service.svc?action=CreateCalendar`,
seen 2026-11-06), each with a tick and a "…" (Show this only, Remove, Color, Move). Split view (in the view menu)
shows each calendar ticked in columns of its own under its name and its close. What a person's calendar shows by
default is their free and busy times: the native client reads them with the Scheduler's `schedules` read (five days a
read, as the web client asks) and keeps the list of added calendars in its own settings, not in the mailbox.

### Rooms

The web client's code names the room finder's reads, `GET v2.0/places/roomLists`, `v2.0/places/rooms` and
`v2.0/places/<list address>/roomList/rooms` (each `?$count=true&$top=<n>&$skip=0`), answered `{"roomLists": [<place>]}`
and `{"rooms": [<place>]}`, a place `{"displayName", "emailAddress", "address"?, …}`. The lab tenant has no rooms (the
form's "Browse all rooms" said "We didn't find any conference rooms", 2026-11-06), so no answer with one was seen and the
native form shows no room finder; `teams-proto`'s `RoomLists` and `Rooms` read them.

### Write path and lab reach

Calendar changes use the typed event requests and the outbox; there is no membership classifier or permit. Updates
retain the event identity and preserve fields the form did not edit. For a lab run, the agent includes the organizer,
all attendees and any team/channel invitees in its reach check before acting. Free/busy and online-meeting setup use
their typed protocol requests with the service's own credential.

### What the native client does

- **New, Edit, Cancel meeting, Delete, the answer** (`crates/teams-ui/src/event_form.rs`, the calendar's card): each a
  sealed write (`teams-proto` `CreateEvent`, `UpdateEvent`, `CancelEvent`, `DeleteEvent`, `RespondEvent`) through the outbox's calendar kind
  (`Outbox::try_calendar`): one attempt, with another only after a 401 or a short throttle,
  never after an unknown outcome. A Teams meeting's creation first makes its online meeting (`CreateOnlineMeeting`, a
  plain request) and carries it. The engine fills in the organizer (the account's MRI, its profile's address, its
  name) and the time zone (`TimeZoneSettings`, from `TZ` or the OS's IANA zone through `iana-time-zone`, on Windows
  and Unix), each kept once read (one that didn't read is asked again at the next write; a change without the zone
  keeps the event's own).
- A change sends the event as `EventRead` gave it with the form's fields over it, so what the form doesn't show goes
  back as it came; a Teams meeting stays one, an occurrence one and a series' master one.
- **A series** (`crates/teams-ui/src/event_form/repeat.rs`, `teams-proto`'s `Recurrence`): the form's Series makes a
  new event repeat; an occurrence's change edits that occurrence, or the whole series once Series is picked (its
  master read and changed by its id). The form's text is the app's rich-text editor, sent as the HTML it writes for a
  message; an event's own text goes back as it came while it isn't edited.
- **The Scheduler** (`crates/teams-ui/src/event_form/scheduler.rs`): both free/busy reads (`teams-proto`'s
  `MeetingTimes` and `Schedules`, plain requests) for the form's day, sent together when it shows the Scheduler or
  names attendees, and again as the day, the attendees or the length change.
- **The card** reads its event whole when it opens (`EventRead`) for whom it invites and their answers, which also
  tells the organizer's meeting (Cancel meeting) from an appointment (Delete). An invitation's card offers Accept,
  Decline and Tentative with "Email organizer" and an optional message, as the web client's peek does, for the
  occurrence or (by its master) the whole series; an occurrence's Cancel and Delete offer this event or the whole
  series, a cancellation with a message for the attendees.
- It never sends the online meeting's delete.
- **Meet now** (`crates/teams-ui/src/calendar_view/meet_now.rs`): the meeting made through the outbox (`MeetNow`, one
  attempt), then its pre-join (Start meeting) or its short link (Get a link to share).
- **Time zones** (`crates/teams-ui/src/event_form/zones.rs`): the form resolves the desktop's IANA zone from a valid
  `TZ` override or the OS on Windows and Unix, and its slot stays in the desktop's time; a start zone
  other than the desktop's is sent as the draft's `zone`, whose Exchange name the engine reads for `eventTimeZone`, and
  both times are written in it. A series' days are the start zone's (its weekday, its first and last days, the
  summary), written as that zone's midnights. Each zone picker searches its zones by city, region or offset, and
  picking one keeps the line's date and time, as Outlook's does. A change opens in the event's own `eventTimeZone`
  (`zones/exchange.rs`) when its clock isn't the desktop's, so saving it unchanged writes the same moments, zone and
  series again; a zone name the list doesn't know opens in the desktop's.
- **People's calendars** (`crates/teams-ui/src/calendar_view/people.rs`): their busy times read for the days shown
  (`Schedules`, a read per five days), drawn in each calendar's colour, opening no card.
