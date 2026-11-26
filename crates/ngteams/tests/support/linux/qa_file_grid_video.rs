//! QA of task 793 (round 1), in the same walk: a video shared with a link for specific people opens in the app.
//!
//! - **A video alone**: c sends a message whose only file is a video. Its card is two columns wide with its location
//!   whole, shows the video's thumbnail, and a click on it plays the video; its frames move. Teams Light and Dark.
//! - **The keyboard**: Escape in the box, Enter into the list, then Tab reach the card, and Enter plays the video;
//!   Escape pauses it.
//! - **A refused link**: Graph refuses the file also with `Prefer: redeemSharingLink`. A click on its card says "This
//!   video can't be opened here" (the player's `Unavailable`), not a load that never ends.
//! - **The Recap tab**: a meeting whose recap names no drive or item of its recording, and the recording's link in the
//!   short form `/:v:/p/<owner>/<token>`. A click on Recap and on its player plays the recording, streamed by the item
//!   Graph names for the link. Teams Light and Dark.
//!
//! Every name, id, link and byte is made up.

use super::*;

/// A video c sends alone (task 793).
pub(super) const LONE_CLIP: MadeUpFile = MadeUpFile {
    name: "05-made-up-lone-rehearsal.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkLone0111",
    frame: Some(([120, 30, 140], [250, 200, 90])),
};

/// A video whose link Graph refuses, redeemed or not.
pub(super) const REFUSED: MadeUpFile = MadeUpFile {
    name: "06-made-up-refused.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkRefused0112",
    frame: None,
};

/// The meeting chat whose Recap tab plays a recording.
pub(super) const MEETING: &str = "19:meeting_MadeUpRecapQa793@thread.v2";

/// The account's MCPS base, as GTM names it for the US region.
pub(super) const MCPS_BASE: &str = "https://teams.microsoft.com/api/mcps/us/";

/// The recording's sharing link: the short form of a link for specific people.
const RECORDING: &str = "https://contoso-my.sharepoint.com/:v:/p/made_up_user_contoso_test/MadeUpRecapShare0201";

/// The drive and the item Graph names for [`RECORDING`]; the recap names neither.
const RECORDING_DRIVE: &str = "b!madeuprecap";
const RECORDING_ITEM: &str = "MADEUPRECAPITEM";

/// The recording's presentation, as the media transform service's MPD has it (the parts below, 3 s).
const MPD: &str = r#"<MPD type="static" mediaPresentationDuration="PT0H0M3.000S" xmlns="urn:mpeg:DASH:schema:MPD:2011">
  <BaseURL>https://madeupregion1-mediap.svc.ms/transform/</BaseURL>
  <Period>
    <AdaptationSet id="0" contentType="audio" mimeType="audio/mp4">
      <SegmentTemplate timescale="16000" initialization="videotranscode?part=initsegment&amp;track=audio&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;track=audio&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline><S t="0" d="14336" /><S d="16384" r="1" /><S d="896" /></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="audcopy" bandwidth="24000" />
    </AdaptationSet>
    <AdaptationSet id="1" contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="16384" initialization="videotranscode?part=initsegment&amp;track=video&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;track=video&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline><S t="0" d="16384" r="2" /></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="vcopy" bandwidth="120000" />
    </AdaptationSet>
  </Period>
</MPD>"#;

/// The made-up DASH parts of teams-media's tests: the init part, then the media parts in order.
const AUDIO_PARTS: [&[u8]; 5] = [
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/init-1.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-1-00001.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-1-00002.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-1-00003.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-1-00004.m4s"),
];
const VIDEO_PARTS: [&[u8]; 4] = [
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/init-0.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-0-00001.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-0-00002.m4s"),
    include_bytes!("../../../../teams-media/tests/fixtures/media/dash/chunk-0-00003.m4s"),
];
/// Each media part's `segmentTime`, as the MPD's timelines give it.
const AUDIO_TIMES: [&str; 4] = ["0", "14336", "30720", "47104"];
const VIDEO_TIMES: [&str; 3] = ["0", "16384", "32768"];

/// Where the conversation pane starts: right of the chat list.
const PANE_LEFT: u32 = 460;
/// Where the window's content starts: under its title bar.
const HEADER_TOP: u32 = 50;

/// The most Tab presses the keyboard check tries.
const TABS: u32 = 10;

// --- the service ---

/// Graph refuses [`REFUSED`]'s reads, redeemed or not.
pub(super) fn serve_refused(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let token = refused_token()?;
    for path in [
        format!("/v1.0/shares/{token}/driveItem"),
        format!("/v1.0/shares/{token}/driveItem/thumbnails"),
    ] {
        runtime.block_on(mock.serve_refused(&path));
    }
    Ok(())
}

fn refused_token() -> SandboxResult<String> {
    Ok(teams_proto::FileThumbnails::of(&share_url(&REFUSED))
        .ok_or("a made-up link no read names")?
        .share()
        .to_owned())
}

/// The meeting chat of c and a, its recap (MCPS) listing one recording with no drive or item, Graph's item of the
/// recording's link (redeemed only), its presentation and its parts.
pub(super) fn serve_recap(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    if !mock.add_meeting_chat(MEETING, "Made-up rehearsal review", &[LAB_C, LAB_A]) {
        return Err("no meeting chat made".into());
    }
    let contents = json!({
        "changeKey": "",
        "resources": [{
            "type": "Recording",
            "location": RECORDING,
            "metadata": {
                "callId": "made-up-call-793",
                "startTime": "639029448000000000",
                "endTime": "639029448300000000",
            },
        }],
        "invalidResources": [],
        "paginationInfo": {"hasNextPage": false},
    })
    .to_string();
    for path in ["/api/mcps/us/contents", "/api/mcps/us/contents/"] {
        runtime.block_on(mock.serve_bytes(path, "application/json", contents.clone().into_bytes()));
    }
    let share = teams_proto::FileDownload::of(RECORDING).ok_or("a made-up recording link no read names")?;
    // no download link: the recording plays only when it is streamed
    let item = json!({
        "id": RECORDING_ITEM,
        "name": "made-up-rehearsal-review.mp4",
        "size": 4096,
        "parentReference": {"driveId": RECORDING_DRIVE},
    });
    runtime.block_on(mock.serve_redeemed(
        &format!("/v1.0/shares/{}/driveItem", share.share()),
        "application/json",
        item.to_string().into_bytes(),
    ));
    let manifest = format!(
        "/personal/made_up_user_contoso_test/_api/v2.1/drives/{RECORDING_DRIVE}/items/{RECORDING_ITEM}/content"
    );
    runtime.block_on(mock.serve_bytes(&manifest, "application/dash+xml", MPD.as_bytes().to_vec()));
    runtime.block_on(mock.serve_bytes_by_query("/transform/videotranscode", "video/mp4", recording_part));
    Ok(())
}

/// The part of the recording a query of the transform service names.
fn recording_part(query: &str) -> Option<Vec<u8>> {
    let (parts, times): (&[&[u8]], &[&str]) = match field(query, "track")? {
        "video" => (&VIDEO_PARTS, &VIDEO_TIMES),
        "audio" => (&AUDIO_PARTS, &AUDIO_TIMES),
        _ => return None,
    };
    match field(query, "part")? {
        "initsegment" => Some(parts[0].to_vec()),
        "mediasegment" => {
            let time = field(query, "segmentTime")?;
            let at = times.iter().position(|known| *known == time)?;
            parts.get(at + 1).map(|part| part.to_vec())
        }
        _ => None,
    }
}

fn field<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

// --- the walk ---

impl Walk<'_> {
    pub(super) fn videos_open(&mut self) -> SandboxResult {
        self.lone_video()?;
        self.keyboard()?;
        self.refused()?;
        self.recap()?;
        self.run.theme(false)
    }

    /// The state of the player of message `key`'s first file, as `<id> <state>`.
    fn player_of(&mut self, key: &str) -> SandboxResult<Option<String>> {
        let id = format!("{}-0 ", self.posted(key)?.id);
        let states = self.run.players_until("the players", |_| true)?;
        Ok(states.into_iter().find(|state| state.starts_with(&id)))
    }

    /// Waits for the player of message `key` in a state `wanted` takes; `None` when it never came.
    fn player_until(&mut self, key: &str, wanted: impl Fn(&str) -> bool) -> SandboxResult<Option<String>> {
        let id = format!("{}-0 ", self.posted(key)?.id);
        let found = self.run.players_until(&format!("the player of {key}"), |states| {
            states.iter().any(|state| state.starts_with(&id) && wanted(state))
        });
        Ok(found
            .ok()
            .and_then(|states| states.into_iter().find(|state| state.starts_with(&id))))
    }

    fn lone_video(&mut self) -> SandboxResult {
        self.open_message("lone-video")?;
        let grid = Grid {
            cards: 1,
            rows: 1,
            across: 1,
        };
        let cards = self.both_themes("qa793-lone-video", grid)?;
        let [card] = cards.as_slice() else {
            note(format!("the lone video: not one card on the screen: {cards:?}"));
            return Ok(());
        };
        let card = *card;
        if card.width() + 4 < LONE_WIDTH {
            note(format!(
                "the lone video's card is {} px wide, not two columns ({LONE_WIDTH})",
                card.width()
            ));
        }
        self.thumbnail(card);
        self.location(card);
        self.play_lone(card, LIGHT)?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        let (_, dark) = self.shot_in("qa793-lone-video-again", DARK)?;
        if let Some(card) = dark.first().copied() {
            self.play_lone(card, DARK)?;
        } else {
            note("the lone video: no card on the Dark screen after it played".to_owned());
        }
        self.run.theme(false)?;
        step("a video alone: its card is two columns wide, shows its thumbnail and plays");
        Ok(())
    }

    /// The card's picture is the made-up thumbnail (its gradient: red over green), not an empty box.
    fn thumbnail(&mut self, card: Rect) {
        let picture = Rect {
            y0: card.y0 + HEADER + 4,
            ..card
        };
        let area = usize::try_from(picture.width() * picture.height()).unwrap_or(usize::MAX);
        let tinted = count_in(&self.run.last, picture, |[red, green, _]| {
            red > green.saturating_add(30)
        });
        if tinted * 3 < area {
            note(format!(
                "the lone video's card shows no thumbnail ({tinted} of {area} pixels are the made-up frame's)"
            ));
        }
        step(&format!("the lone video's thumbnail: {tinted} of {area} pixels"));
    }

    /// The location's line ends well before Share: it is whole, not cut.
    fn location(&mut self, card: Rect) {
        let shot = &self.run.last;
        let fill = at(shot, card.x0 + 6, card.y0 + 10);
        let line = Rect {
            x0: card.x0 + 50,
            y0: card.y0 + 36,
            x1: share_slot(card).x0 - 4,
            y1: card.y0 + 54,
        };
        let right = (line.x0..line.x1)
            .rev()
            .find(|&x| (line.y0..line.y1).any(|y| !near(at(shot, x, y), fill, 40)));
        match right {
            None => note("the lone video's card shows no location".to_owned()),
            Some(right) if line.x1 - right < 40 => note(format!(
                "the lone video's location runs to {right}, near Share at {}: cut?",
                line.x1
            )),
            Some(right) => step(&format!("the location ends at {right}, Share's place at {}", line.x1)),
        }
    }

    /// A click on the card's picture plays the video: the player says Playing and its frames move.
    fn play_lone(&mut self, card: Rect, look: Look) -> SandboxResult {
        let name = look.name;
        let before = self.run.shot(self.sandbox, &format!("qa793-lone-before-{name}.png"))?;
        let (x, y) = picture_at(card);
        self.run.click(x, y)?;
        let playing = self.player_until("lone-video", |state| {
            state.contains("Playing") || state.contains("Failed")
        })?;
        if !playing.as_deref().is_some_and(|state| state.contains("Playing")) {
            note(format!("{name}: a click on the lone video plays nothing: {playing:?}"));
            return Ok(());
        }
        let first = self.run.shot(self.sandbox, &format!("qa793-player-{name}.png"))?;
        let second = self.run.shot(self.sandbox, &format!("qa793-player-later-{name}.png"))?;
        let picture = Rect {
            y0: card.y0 + HEADER + 4,
            y1: card.y1 - 50,
            ..card
        };
        let shown = changed_in(&load(&before)?, &load(&first)?, picture);
        let moved = changed_in(&load(&first)?, &load(&second)?, picture);
        if shown < 500 {
            note(format!(
                "{name}: the playing video shows no frame of it ({shown} pixels)"
            ));
        }
        if moved < 200 {
            note(format!(
                "{name}: the video's frames do not move while it plays ({moved} pixels)"
            ));
        }
        step(&format!(
            "{name}: the lone video plays ({shown} pixels shown, {moved} moved)"
        ));
        let _ = self.player_until("lone-video", |state| !state.contains("Playing"));
        Ok(())
    }

    /// From the chat as it opens: Escape (the box gives the keys to the list), Enter (into the list), Tab `n` times,
    /// then Enter: the lone video plays at some `n`. Escape then pauses it and gives the keys back to its card.
    fn keyboard(&mut self) -> SandboxResult {
        let mut reached = None;
        for presses in 0..=TABS {
            if self.keys_play(presses)? {
                reached = Some(presses);
                break;
            }
        }
        let Some(presses) = reached else {
            self.run.shot(self.sandbox, "qa793-keyboard-unreached-light.png")?;
            note(format!(
                "the keyboard does not reach the lone video's card: Escape, Enter, 0 to {TABS} Tab presses, then Enter, \
                 play nothing"
            ));
            return Ok(());
        };
        self.run.shot(self.sandbox, "qa793-keyboard-playing-light.png")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let closed = self.player_of("lone-video")?;
        self.run.shot(self.sandbox, "qa793-keyboard-escape-light.png")?;
        if closed.as_deref().is_some_and(|state| state.contains("Playing")) {
            note(format!("Escape in the playing video does not pause it: {closed:?}"));
        }
        // the keys are back on the card: its ring, in both themes
        for look in [LIGHT, DARK] {
            self.run.theme(look.dark)?;
            thread::sleep(SETTLE);
            self.run
                .shot(self.sandbox, &format!("qa849-card-focused-{}.png", look.name))?;
        }
        self.run.theme(false)?;
        step(&format!(
            "the keyboard reaches the card: Escape, Enter, {presses} Tab, then Enter plays it; Escape: {closed:?}"
        ));
        Ok(())
    }

    /// Opens the lone video's chat afresh, presses Escape, Enter, Tab `presses` times, then Enter; whether the video
    /// started.
    fn keys_play(&mut self, presses: u32) -> SandboxResult<bool> {
        self.open_message("one")?;
        self.open_message("lone-video")?;
        let before = self.player_of("lone-video")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(Duration::from_millis(120));
        self.run.input.key(keys::ENTER)?;
        thread::sleep(Duration::from_millis(120));
        for _ in 0..presses {
            self.run.input.key(keys::TAB)?;
            thread::sleep(Duration::from_millis(120));
        }
        self.run.input.key(keys::ENTER)?;
        thread::sleep(QUIET);
        let after = self.player_of("lone-video")?;
        let started = after != before
            && after
                .as_deref()
                .is_some_and(|state| state.contains("Playing") || state.contains("Loading") || state.contains("Ended"));
        if !started {
            self.run.input.key(keys::ESC)?;
            self.run.input.key(keys::ESC)?;
            thread::sleep(Duration::from_millis(300));
        }
        Ok(started)
    }

    /// A link Graph refuses also redeemed: a click on the card says the video can't be opened here, soon.
    fn refused(&mut self) -> SandboxResult {
        self.open_message("refused")?;
        let (_, cards) = self.shot_in("qa793-refused", LIGHT)?;
        let Some(card) = cards.first().copied() else {
            note(format!("the refused video: no card on the screen: {cards:?}"));
            return Ok(());
        };
        let (x, y) = picture_at(card);
        self.run.click(x, y)?;
        let failed = self.player_until("refused", |state| state.contains("Failed"))?;
        if !failed.as_deref().is_some_and(|state| state.contains("Unavailable")) {
            let now = self.player_of("refused")?;
            note(format!(
                "a click on a video Graph refuses: the player says {now:?}, not Failed(Unavailable) (\"This video \
                 can't be opened here\")"
            ));
        }
        for look in [LIGHT, DARK] {
            self.shot_in("qa793-refused-open", look)?;
        }
        let token = refused_token()?;
        let reads = self.service.received(&token);
        let redeemed = reads.iter().filter(|redeemed| **redeemed).count();
        if redeemed == 0 {
            note(format!(
                "the refused link's reads never carried Prefer: redeemSharingLink ({} reads)",
                reads.len()
            ));
        }
        step(&format!(
            "a refused link: the player says {failed:?}; {redeemed} of {} reads redeemed it",
            reads.len()
        ));
        Ok(())
    }

    /// The Recap tab of a meeting whose recap names no drive or item: a click on Recap, then on its player, plays the
    /// recording.
    fn recap(&mut self) -> SandboxResult {
        self.run.theme(false)?;
        self.run.open(MEETING)?;
        thread::sleep(SETTLE * 2);
        self.run.shot(self.sandbox, "qa793-meeting-chat-light.png")?;
        let Some((x, y)) = recap_tab_at(&self.run.last, LIGHT) else {
            note("the meeting chat's Recap tab is not found on the screen".to_owned());
            return Ok(());
        };
        self.run.click(x, y)?;
        let shown = self.run.trace.next_where("the Recap tab shown", |line| {
            line["view"] == "chat-tab" && line["change"] == "shown" && line["kinds"][0] == "recap"
        });
        if shown.is_err() || self.run.question("tab")? != "\"Recap\"" {
            note("a click on the Recap tab does not show the recap".to_owned());
            return Ok(());
        }
        thread::sleep(SETTLE * 3);
        self.run.shot(self.sandbox, "qa793-recap-opened-light.png")?;
        let Some(screen) = black_box(&self.run.last) else {
            note("the Recap tab shows no player".to_owned());
            return Ok(());
        };
        for look in [DARK, LIGHT] {
            self.play_recap(look, screen)?;
        }
        step("the Recap tab plays a recording its recap names no item of, by its /p/ link");
        Ok(())
    }

    /// A click on the recap's player, found as `screen` while it was black, plays the recording.
    fn play_recap(&mut self, look: Look, screen: Rect) -> SandboxResult {
        let name = look.name;
        self.run.theme(look.dark)?;
        thread::sleep(SETTLE);
        let before = self.run.shot(self.sandbox, &format!("qa793-recap-{name}.png"))?;
        let (x, y) = (screen.x0 + screen.width() / 2, screen.y0 + screen.height() / 2);
        self.run.click(x, y)?;
        let playing = self.run.players_until("the recap's player", |states| {
            states
                .iter()
                .any(|state| state.starts_with("recap-") && (state.contains("Playing") || state.contains("Failed")))
        });
        let recap = playing
            .ok()
            .and_then(|states| states.into_iter().find(|state| state.starts_with("recap-")));
        if !recap.as_deref().is_some_and(|state| state.contains("Playing")) {
            note(format!(
                "{name}: a click on the Recap tab's player plays nothing: {recap:?}"
            ));
            self.run
                .shot(self.sandbox, &format!("qa793-recap-unplayed-{name}.png"))?;
            return Ok(());
        }
        let playing = self
            .run
            .shot(self.sandbox, &format!("qa793-recap-playing-{name}.png"))?;
        let shown = changed_in(&load(&before)?, &load(&playing)?, screen);
        if shown < 500 {
            note(format!("{name}: the recap's player shows no frame ({shown} pixels)"));
        }
        step(&format!("{name}: the recap's recording plays ({shown} pixels shown)"));
        let _ = self.run.players_until("the recap's player stopped", |states| {
            states
                .iter()
                .any(|state| state.starts_with("recap-") && !state.contains("Playing"))
        });
        Ok(())
    }
}

impl Service {
    /// Whether each Graph read whose path holds `token` carried `Prefer: redeemSharingLink`.
    fn received(&self, token: &str) -> Vec<bool> {
        let requests = self.runtime.block_on(self.mock.received_requests());
        requests
            .iter()
            .filter(|request| request.url.path().contains(token))
            .map(|request| {
                request
                    .headers
                    .get("prefer")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value == "redeemSharingLink")
            })
            .collect()
    }
}

/// Where the Recap tab is in a meeting chat's header: the second word right of the shown tab's underline (Chat's),
/// after Shared.
fn recap_tab_at(shot: &image::RgbImage, look: Look) -> Option<(u32, u32)> {
    // the underline runs from the brand colour into its gradient's second stop: blue over red and green
    let floor = if look.dark { 200 } else { 180 };
    let brand = |[red, green, blue]: [u8; 3]| blue >= floor && red < blue - 50 && green < blue - 50;
    // the conversation's header: right of the chat list, under the title bar (its purple is near the brand colour)
    let (underline_y, (_, chat_right)) = (HEADER_TOP..240).find_map(|y| {
        runs(shot, y, PANE_LEFT, shot.width(), brand)
            .into_iter()
            .find(|&(from, to)| {
                let mid = u32::midpoint(from, to);
                (16..120).contains(&(to - from)) && !brand(at(shot, mid, y - 3)) && !brand(at(shot, mid, y + 3))
            })
            .map(|run| (y, run))
    })?;
    let (top, bottom) = (underline_y.saturating_sub(30), underline_y.saturating_sub(4));
    let fill = at(shot, chat_right + 4, underline_y.saturating_sub(2));
    let inked = |x: u32| (top..bottom).any(|y| !near(at(shot, x, y), fill, 50));
    let mut words: Vec<(u32, u32)> = Vec::new();
    let mut x = chat_right + 2;
    while x < shot.width() && words.len() < 2 {
        if !inked(x) {
            x += 1;
            continue;
        }
        let start = x;
        let mut gap = 0;
        while x < shot.width() && gap < 12 {
            gap = if inked(x) { 0 } else { gap + 1 };
            x += 1;
        }
        words.push((start, x - gap));
    }
    let (from, to) = *words.get(1)?;
    Some((u32::midpoint(from, to), u32::midpoint(top, bottom)))
}

/// The largest black box on the screen (a player's), by its rows of black at least 300 px long.
fn black_box(shot: &image::RgbImage) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    for y in 0..shot.height() {
        for (from, to) in runs(shot, y, 0, shot.width(), |pixel| near(pixel, [0, 0, 0], 6)) {
            if to - from < 300 {
                continue;
            }
            found = Some(found.map_or(
                Rect {
                    x0: from,
                    y0: y,
                    x1: to,
                    y1: y + 1,
                },
                |known| Rect {
                    x0: known.x0.min(from),
                    y0: known.y0,
                    x1: known.x1.max(to),
                    y1: y + 1,
                },
            ));
        }
    }
    found.filter(|screen| screen.height() > 100)
}
