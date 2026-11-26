//! Who is in a call as the account sees it (M3 Task 19): the participants its roster names, whose video each of a
//! group call's receiving lines shows, and who is speaking.
//!
//! - **The participants** are the roster's (`teams_call::Participant`, the account left out), in the order they came.
//! - **Whose video goes where** (a group call's): each receiving line ([`super::GROUP_SLOTS`] of them, numbered from
//!   1 as the media numbers its video lines) shows one participant's video. A participant keeps its line while it is
//!   in the call and its video source stays the same; a free line goes to the next participant without one, those
//!   whose camera is on first. What the lines are to show goes to the mixer as source requests
//!   ([`teams_media::session::SourceRequest`]: the line's stream id, the participant's video source id while its
//!   camera is on, `-1` while it is off), each line only when it changed (the session keeps what went).
//! - **Whose screen the sharing line shows** (a group call's): the participant whose share started last (the
//!   roster's `applicationsharing-video` stream going out), asked for on the call's sharing line by its stream id as a
//!   receiving line's video is (`-1` once nobody shares), as the web client's viewer asks the mixer for it: at a
//!   screen's size (1080p), as the mixer forwards a share unscaled. As in Teams, a new share takes over: the sharing
//!   line follows it while the old sharer's client stops its own.
//! - **Who is speaking**: in a 1:1 call, the far end while what the speaker plays is louder than [`SPEAKING_LEVEL`];
//!   in a group call, the newest dominant speaker the mixer told (by audio source id) while what the speaker plays is
//!   (the mixer's word alone would keep the last one speaking through a silence), and nobody in a group call without
//!   a control plane (the mixed playout says nothing of who in it speaks). The account itself is speaking while what
//!   its microphone hears is. A level that falls quiet keeps its speaker for [`SPEAKING_HOLD`], so a pause between
//!   words doesn't flicker.
//! - **Whether the microphone hears anything** (before its processing, muted or not; held as speaking is): what the
//!   speaking mark can't tell when the processing takes everything out, as it does an echo it cancels.

use std::time::Duration;

#[cfg(feature = "calls")]
use {
    std::time::Instant,
    teams_call::Participant,
    teams_media::session::{AudioLevels, SourceRequest},
};

/// The RMS level (0 to 1) above which a side is speaking: about -34 dBFS, above a quiet room's noise.
pub const SPEAKING_LEVEL: f32 = 0.02;
/// The RMS level (0 to 1) of what the microphone delivers, before its processing, above which it hears something:
/// -80 dBFS, under any real microphone's own noise, over a silent source's.
pub const HEARING_LEVEL: f32 = 1e-4;
/// How long a side stays speaking after its level fell below [`SPEAKING_LEVEL`].
pub const SPEAKING_HOLD: Duration = Duration::from_millis(400);

/// One participant as the account is shown it ([`crate::Event::CallParticipants`]).
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a sign of its own the call gives of the participant, shown by its own mark"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallParticipant {
    pub mri: String,
    /// As the call's roster gives it.
    pub name: Option<String>,
    /// Whether its camera is on.
    pub video_on: bool,
    /// The video line showing it (a group call's receiving line, from 1; a 1:1 call's far end is line 0), if any.
    pub line: Option<usize>,
    /// Whether it is speaking now.
    pub speaking: bool,
    /// Whether its microphone is muted, as its endpoint reports it.
    pub muted: bool,
    /// Whether its hand is raised.
    pub hand_raised: bool,
    /// Whether it organizes the meeting: Teams' People pane says "Organizer" under its name.
    pub organizer: bool,
    /// When it joined the call, as its roster entry says (`endpointJoinTime`, RFC 3339): the attendance list's time.
    pub joined: Option<String>,
    /// Whether it shares its screen now (a group call's roster lists its sharing stream going out): the share's name.
    pub sharing: bool,
}

/// What a call's view is now: its participants, whether the account itself is speaking, and whether its microphone
/// hears anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallView {
    pub participants: Vec<CallParticipant>,
    pub speaking: bool,
    /// What the microphone delivers is above [`HEARING_LEVEL`] before its processing (or was within
    /// [`SPEAKING_HOLD`]).
    pub hearing: bool,
}

/// A running call's participants and lines (see the module docs).
#[derive(Debug, Default)]
#[cfg(feature = "calls")]
pub(super) struct Gallery {
    /// A group call's: who speaks in it is the mixer's to say.
    group: bool,
    /// A group call's that takes video: its receiving lines are assigned.
    lines: bool,
    participants: Vec<Participant>,
    /// Who shares a screen, by MRI, in the order their shares started.
    sharers: Vec<String>,
    /// The receiving lines' stream ids, in line order (line `index + 1`); `None` where the far end named none.
    slots: Vec<Option<u32>>,
    /// Who each receiving line shows, by MRI and video source.
    shown: Vec<Option<(String, i64)>>,
    /// A group call's sharing line's stream id, once the far end named one.
    sharing_slot: Option<u32>,
    /// Who the mixer last said spoke (a group call's), by audio source id.
    dominant: Option<i64>,
    /// Until when the far end (a 1:1 call's) and the account are speaking.
    far_until: Option<Instant>,
    near_until: Option<Instant>,
    /// Until when the microphone hears something.
    captured_until: Option<Instant>,
    /// What was last told.
    told: Option<CallView>,
}

#[cfg(feature = "calls")]
impl Gallery {
    /// A call's gallery; `group` for a group call's, `lines` for one whose video started (its receiving lines
    /// assigned).
    pub(super) fn new(group: bool, lines: bool) -> Self {
        Self {
            group,
            lines: group && lines,
            ..Self::default()
        }
    }

    /// It is a group call's whose receiving lines are assigned.
    pub(super) fn has_lines(&self) -> bool {
        self.lines
    }

    /// The participants now, as the roster told them; whether a participant's share started.
    pub(super) fn set_participants(&mut self, participants: Vec<Participant>) -> bool {
        let shares = |mri: &str| {
            participants
                .iter()
                .any(|p| p.mri.as_str() == mri && p.sharing_source.is_some())
        };
        self.sharers.retain(|mri| shares(mri));
        let mut started = false;
        for p in participants.iter().filter(|p| p.sharing_source.is_some()) {
            if !self.sharers.iter().any(|mri| mri == p.mri.as_str()) {
                self.sharers.push(p.mri.as_str().to_owned());
                started = true;
            }
        }
        self.participants = participants;
        started
    }

    /// The participants the roster told last.
    pub(super) fn participants(&self) -> &[Participant] {
        &self.participants
    }

    /// The receiving lines' stream ids, as the media's dialect names them now.
    pub(super) fn set_slots(&mut self, slots: Vec<Option<u32>>) {
        self.shown.resize(slots.len(), None);
        self.slots = slots;
    }

    /// The group call's sharing line's stream id (`None` while it has none).
    pub(super) fn set_sharing_slot(&mut self, slot: Option<u32>) {
        self.sharing_slot = slot;
    }

    /// Whose screen is shared now, in a group call whose sharing line can show it: the participant whose share
    /// started last.
    pub(super) fn sharer(&self) -> Option<&Participant> {
        if !self.lines || self.sharing_slot.is_none() {
            return None;
        }
        let newest = self.sharers.last()?;
        self.participants.iter().find(|p| p.mri.as_str() == newest)
    }

    /// The source requests that show what the lines are to show now (a 1:1 call's: none): each line that has a stream
    /// id, with its participant's video source (`-1` for none).
    pub(super) fn assign(&mut self) -> Vec<SourceRequest> {
        if !self.lines {
            return Vec::new();
        }
        let present = |shown: &(String, i64), participants: &[Participant]| {
            participants
                .iter()
                .any(|p| p.mri.as_str() == shown.0 && p.video_source == Some(shown.1))
        };
        for line in &mut self.shown {
            if line.as_ref().is_some_and(|shown| !present(shown, &self.participants)) {
                *line = None;
            }
        }
        let mut waiting: Vec<&Participant> = self
            .participants
            .iter()
            .filter(|p| p.video_source.is_some())
            .filter(|p| !self.shown.iter().flatten().any(|(mri, _)| p.mri.as_str() == mri))
            .collect();
        // those whose camera is on first, in the order they came
        waiting.sort_by_key(|p| !p.video_on);
        let mut waiting = waiting.into_iter();
        for line in self.shown.iter_mut().filter(|line| line.is_none()) {
            let Some(next) = waiting.next() else { break };
            if let Some(source) = next.video_source {
                *line = Some((next.mri.as_str().to_owned(), source));
            }
        }
        // a line asks for its participant's video only while its camera is on, so its camera coming on asks again: a
        // caller's requests made before its callees' cameras were on were answered `ok` and brought no video (seen
        // live, M3 Task 19; `docs/protocol/calling.md`, "Group video")
        let source = |shown: &(String, i64)| {
            if self
                .participants
                .iter()
                .any(|p| p.mri.as_str() == shown.0 && p.video_on)
            {
                shown.1
            } else {
                -1
            }
        };
        let screen = self.sharing_slot.map(|stream| {
            let sharer = self.sharer().and_then(|p| p.sharing_source);
            SourceRequest::screen(stream, sharer.unwrap_or(-1))
        });
        self.slots
            .iter()
            .zip(&self.shown)
            .filter_map(|(stream, shown)| {
                Some(SourceRequest::camera(
                    stream.as_ref().copied()?,
                    shown.as_ref().map_or(-1, source),
                ))
            })
            .chain(screen)
            .collect()
    }

    /// Who the mixer says spoke, newest first (a group call's).
    pub(super) fn speakers(&mut self, history: &[i64]) {
        self.dominant = history.first().copied();
    }

    /// What the media hears now (a 1:1 call's far end, and the account).
    pub(super) fn levels(&mut self, levels: AudioLevels, now: Instant) {
        if levels.far > SPEAKING_LEVEL {
            self.far_until = Some(now + SPEAKING_HOLD);
        }
        if levels.near > SPEAKING_LEVEL {
            self.near_until = Some(now + SPEAKING_HOLD);
        }
        if levels.captured > HEARING_LEVEL {
            self.captured_until = Some(now + SPEAKING_HOLD);
        }
    }

    /// The view at `now`, when it changed since last told.
    pub(super) fn changed(&mut self, now: Instant) -> Option<CallView> {
        let view = self.view(now);
        if self.told.as_ref() == Some(&view) {
            return None;
        }
        self.told = Some(view.clone());
        Some(view)
    }

    fn view(&self, now: Instant) -> CallView {
        let far_speaking = self.far_until.is_some_and(|until| now < until);
        let participants = self
            .participants
            .iter()
            .map(|participant| {
                let mri = participant.mri.as_str().to_owned();
                let line = if self.lines {
                    self.shown
                        .iter()
                        .position(|shown| shown.as_ref().is_some_and(|(shown, _)| *shown == mri))
                        .map(|index| index + 1)
                } else if self.group {
                    None
                } else {
                    Some(0)
                };
                let speaking = far_speaking
                    && (!self.group || (self.dominant.is_some() && participant.audio_source == self.dominant));
                CallParticipant {
                    mri,
                    name: participant.name.clone(),
                    video_on: participant.video_on,
                    line,
                    speaking,
                    muted: participant.muted,
                    hand_raised: participant.hand_raised,
                    organizer: participant.organizer,
                    joined: participant.joined.clone(),
                    sharing: participant.sharing_source.is_some(),
                }
            })
            .collect();
        CallView {
            participants,
            speaking: self.near_until.is_some_and(|until| now < until),
            hearing: self.captured_until.is_some_and(|until| now < until),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_proto::Mri;

    const LOUD: f32 = SPEAKING_LEVEL * 4.0;

    fn participant(mri: &str, audio: i64) -> Participant {
        Participant {
            mri: Mri::new(mri),
            name: None,
            audio_source: Some(audio),
            video_source: Some(audio + 1),
            video_on: true,
            sharing_source: None,
            muted: false,
            hand_raised: false,
            organizer: false,
            joined: None,
        }
    }

    fn levels(near: f32, far: f32) -> AudioLevels {
        AudioLevels {
            near,
            far,
            captured: near,
        }
    }

    /// Who the view marks speaking: the account first, then each participant.
    fn speaking(view: &CallView) -> (bool, Vec<bool>) {
        (view.speaking, view.participants.iter().map(|p| p.speaking).collect())
    }

    #[test]
    fn a_one_to_one_far_end_speaks_while_the_speaker_plays_and_for_the_hold_after() {
        let mut gallery = Gallery::new(false, false);
        gallery.set_participants(vec![participant("8:orgid:far", 10)]);
        let now = Instant::now();
        gallery.levels(levels(0.0, LOUD), now);
        let view = gallery.changed(now).unwrap();
        assert_eq!(speaking(&view), (false, vec![true]));
        assert_eq!(view.participants[0].line, Some(0));
        // quiet: still speaking within the hold, not after it
        gallery.levels(levels(0.0, 0.0), now + SPEAKING_HOLD / 2);
        assert_eq!(gallery.changed(now + SPEAKING_HOLD / 2), None);
        let view = gallery.changed(now + SPEAKING_HOLD).unwrap();
        assert_eq!(speaking(&view), (false, vec![false]));
    }

    #[test]
    fn the_account_speaks_while_its_microphone_hears_it_never_while_silent() {
        let mut gallery = Gallery::new(false, false);
        gallery.set_participants(vec![participant("8:orgid:far", 10)]);
        let now = Instant::now();
        // a muted microphone hears nothing (its level is 0): never speaking
        gallery.levels(levels(0.0, 0.0), now);
        assert_eq!(speaking(&gallery.changed(now).unwrap()), (false, vec![false]));
        gallery.levels(levels(LOUD, 0.0), now);
        assert_eq!(speaking(&gallery.changed(now).unwrap()), (true, vec![false]));
        assert_eq!(
            speaking(&gallery.changed(now + SPEAKING_HOLD).unwrap()),
            (false, vec![false])
        );
    }

    #[test]
    fn the_microphone_hears_what_its_processing_took_out_and_nothing_from_a_silent_source() {
        let mut gallery = Gallery::new(false, false);
        gallery.set_participants(vec![participant("8:orgid:far", 10)]);
        let now = Instant::now();
        let heard = |captured| AudioLevels {
            near: 0.0,
            far: LOUD,
            captured,
        };
        // a silent source
        gallery.levels(heard(0.0), now);
        let view = gallery.changed(now).unwrap();
        assert!(!view.hearing && !view.speaking);
        // the far end's echo, cancelled: heard, not spoken
        gallery.levels(heard(HEARING_LEVEL * 50.0), now);
        let view = gallery.changed(now).unwrap();
        assert!(view.hearing && !view.speaking);
        // held as speaking is, then gone
        gallery.levels(heard(0.0), now + SPEAKING_HOLD / 2);
        assert_eq!(gallery.changed(now + SPEAKING_HOLD / 2), None);
        assert!(!gallery.changed(now + SPEAKING_HOLD).unwrap().hearing);
    }

    #[test]
    fn a_group_call_s_speaker_is_the_mixer_s_while_the_mix_is_audible() {
        let mut gallery = Gallery::new(true, true);
        gallery.set_participants(vec![participant("8:orgid:b", 10), participant("8:orgid:c", 20)]);
        let now = Instant::now();
        // the mix alone names nobody
        gallery.levels(levels(0.0, LOUD), now);
        assert_eq!(speaking(&gallery.changed(now).unwrap()), (false, vec![false, false]));
        gallery.speakers(&[20, 10]);
        assert_eq!(speaking(&gallery.changed(now).unwrap()), (false, vec![false, true]));
        // the mix falls silent: the last dominant speaker isn't speaking any more
        let later = now + SPEAKING_HOLD;
        gallery.levels(levels(0.0, 0.0), later);
        assert_eq!(speaking(&gallery.changed(later).unwrap()), (false, vec![false, false]));
    }

    #[test]
    fn a_group_call_without_video_shows_nobody_speaking_and_no_lines() {
        let mut gallery = Gallery::new(true, false);
        assert!(!gallery.has_lines());
        gallery.set_participants(vec![participant("8:orgid:b", 10), participant("8:orgid:c", 20)]);
        gallery.set_slots(vec![Some(5), Some(6)]);
        assert!(gallery.assign().is_empty(), "no source requests without video");
        let now = Instant::now();
        gallery.levels(levels(0.0, LOUD), now);
        let view = gallery.changed(now).unwrap();
        assert_eq!(speaking(&view), (false, vec![false, false]));
        assert!(view.participants.iter().all(|p| p.line.is_none()), "{view:?}");
    }

    #[test]
    fn a_group_call_s_shared_screen_is_asked_for_on_its_sharing_line_and_let_go_once_it_stops() {
        let mut gallery = Gallery::new(true, true);
        gallery.set_participants(vec![participant("8:orgid:b", 10), participant("8:orgid:c", 20)]);
        gallery.set_slots(vec![Some(5)]);
        assert!(gallery.sharer().is_none(), "no sharing line yet");
        gallery.set_sharing_slot(Some(425));
        assert!(gallery.sharer().is_none(), "nobody shares");
        assert_eq!(
            gallery.assign(),
            vec![SourceRequest::camera(5, 11), SourceRequest::screen(425, -1)]
        );
        let mut sharing = participant("8:orgid:c", 20);
        sharing.sharing_source = Some(212);
        gallery.set_participants(vec![participant("8:orgid:b", 10), sharing]);
        assert_eq!(gallery.sharer().map(|p| p.mri.as_str()), Some("8:orgid:c"));
        assert_eq!(
            gallery.assign(),
            vec![SourceRequest::camera(5, 11), SourceRequest::screen(425, 212)]
        );
        gallery.set_participants(vec![participant("8:orgid:b", 10), participant("8:orgid:c", 20)]);
        assert!(gallery.sharer().is_none());
        assert_eq!(
            gallery.assign(),
            vec![SourceRequest::camera(5, 11), SourceRequest::screen(425, -1)]
        );
    }

    #[test]
    fn a_share_that_starts_while_another_goes_takes_the_sharing_line() {
        let mut gallery = Gallery::new(true, true);
        gallery.set_sharing_slot(Some(425));
        let sharing = |mri: &str, audio: i64, source: i64| {
            let mut sharing = participant(mri, audio);
            sharing.sharing_source = Some(source);
            sharing
        };
        assert!(!gallery.set_participants(vec![participant("8:orgid:b", 10), participant("8:orgid:c", 20)]));
        assert!(gallery.set_participants(vec![sharing("8:orgid:b", 10, 12), participant("8:orgid:c", 20)]));
        assert!(!gallery.set_participants(vec![sharing("8:orgid:b", 10, 12), participant("8:orgid:c", 20)]));
        // c's share starts while b's goes: c's is shown, as long as the roster lists both
        assert!(gallery.set_participants(vec![sharing("8:orgid:b", 10, 12), sharing("8:orgid:c", 20, 22)]));
        assert_eq!(gallery.sharer().map(|p| p.mri.as_str()), Some("8:orgid:c"));
        assert!(!gallery.set_participants(vec![participant("8:orgid:b", 10), sharing("8:orgid:c", 20, 22)]));
        assert_eq!(gallery.sharer().map(|p| p.mri.as_str()), Some("8:orgid:c"));
        // c stops while b's is still listed: b's shows again
        assert!(gallery.set_participants(vec![sharing("8:orgid:b", 10, 12), participant("8:orgid:c", 20)]));
        assert_eq!(gallery.sharer().map(|p| p.mri.as_str()), Some("8:orgid:b"));
    }
}
