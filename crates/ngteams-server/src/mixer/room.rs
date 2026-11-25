//! A call's room: its legs and its 20 ms tick, on the node's monotonic clock. Each tick takes a frame from every leg,
//! mixes for each listener every active source but its own, and sends the mix; the dominant speakers are told when
//! they change (at most every 300 ms) and to a leg once its control plane comes up. Video is forwarded as it comes:
//! each frame of a camera or a screen goes onto every receiving line whose source request named it and whose largest
//! picture its pictures fit, as its last keyframe tells their size ([`super::video`]).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::audio::{self, Activity, FRAME, Heard, Speakers};
use super::control::SourceRequest;
use super::leg::{Driven, Leg};
use super::sps;
use super::video::{Asked, Frame, Slot};

/// The tick.
pub(crate) const TICK: Duration = Duration::from_millis(20);
/// The dominant speakers are told at most this often.
const SPEAKERS_EVERY: Duration = Duration::from_millis(300);

/// One call's room.
#[derive(Debug)]
pub(crate) struct Room {
    pub legs: BTreeMap<String, Leg>,
    activity: BTreeMap<String, Activity>,
    speakers: Speakers,
    /// The history last told, and when.
    told: Vec<u32>,
    told_at: Option<Instant>,
    /// When the next tick is due.
    pub next_tick: Instant,
    /// What each receiving line shows, by its leg and its stream id.
    slots: BTreeMap<(String, u32), Slot>,
    /// When each source was last asked for a keyframe.
    asked: Asked,
    /// The macroblocks of each source's pictures, as its last keyframe told them.
    sizes: BTreeMap<u32, u32>,
}

impl Room {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            legs: BTreeMap::new(),
            activity: BTreeMap::new(),
            speakers: Speakers::default(),
            told: Vec::new(),
            told_at: None,
            next_tick: now + TICK,
            slots: BTreeMap::new(),
            asked: Asked::default(),
            sizes: BTreeMap::new(),
        }
    }

    /// Takes the leg `id` out.
    pub(crate) fn remove(&mut self, id: &str) {
        if let Some(mut leg) = self.legs.remove(id) {
            leg.close();
            self.speakers.forget(leg.sources.audio);
            self.sizes.remove(&leg.sources.video);
            self.sizes.remove(&leg.sources.sharing);
        }
        self.activity.remove(id);
        self.slots.retain(|(leg, _), _| leg != id);
    }

    /// What the leg `id`'s `Rtc` gave: its control plane up, its source requests, its decoders' wish for a keyframe,
    /// and its camera's and screen's frames, forwarded.
    pub(crate) fn driven(&mut self, id: &str, driven: Driven, now: Instant) {
        if driven.control_up {
            self.control_up(id);
        }
        for request in driven.requests {
            self.request(id, request, now);
        }
        for stream in driven.keyframes {
            let source = self.slots.get(&(id.to_owned(), stream)).and_then(Slot::source);
            if let Some(source) = source {
                self.ask_keyframe(source, now);
            }
        }
        for frame in &driven.frames {
            self.forward(id, frame, now);
        }
    }

    /// The leg `id`'s line `request.stream` is to show `request.source`: from that source's next keyframe, asked for.
    /// A request naming no line the leg receives on is nothing.
    fn request(&mut self, id: &str, request: SourceRequest, now: Instant) {
        if !self.legs.get(id).is_some_and(|leg| leg.receives_on(request.stream)) {
            return;
        }
        let slot = self.slots.entry((id.to_owned(), request.stream)).or_default();
        slot.limit(request.max_fs);
        if slot.show(request.source)
            && let Some(source) = slot.source()
        {
            self.ask_keyframe(source, now);
        }
    }

    /// `frame` of the leg `from` onto every other leg's line that shows its source; a line still waiting for a keyframe
    /// has the source asked for one.
    fn forward(&mut self, from: &str, frame: &Frame, now: Instant) {
        if frame.keyframe
            && let Some(size) = sps::macroblocks(&frame.data)
        {
            self.sizes.insert(frame.source, size);
        }
        let size = self.sizes.get(&frame.source).copied();
        let mut wanted = false;
        for ((id, stream), slot) in &mut self.slots {
            if id == from || slot.source() != Some(frame.source) || !slot.fits(size) {
                continue;
            }
            let Some(leg) = self.legs.get_mut(id).filter(|leg| leg.seed.is_some()) else {
                continue;
            };
            match slot.pass(frame.keyframe, frame.time, now) {
                Some(time) => {
                    leg.write_video(*stream, time, frame, now);
                }
                None => wanted = true,
            }
        }
        if wanted {
            self.ask_keyframe(frame.source, now);
        }
    }

    /// Asks the leg whose camera or screen `source` is for a keyframe, unless it was asked a moment ago.
    fn ask_keyframe(&mut self, source: u32, now: Instant) {
        let owner = self
            .legs
            .values_mut()
            .find(|leg| leg.sources.video == source || leg.sources.sharing == source);
        if let Some(leg) = owner
            && self.asked.due(source, now)
        {
            leg.request_keyframe(source);
        }
    }

    /// A leg's control plane came up: it hears who spoke so far.
    fn control_up(&mut self, id: &str) {
        let history = self.speakers.history().to_vec();
        if let Some(leg) = self.legs.get_mut(id)
            && !history.is_empty()
        {
            leg.tell_speakers(&history);
        }
    }

    /// The ticks due by `now`, each a frame of the mix; a room that fell behind more than a few ticks skips ahead.
    pub(crate) fn run_ticks(&mut self, now: Instant) {
        if now.duration_since(self.next_tick) > 5 * TICK {
            self.next_tick = now;
        }
        while self.next_tick <= now {
            self.tick(now);
            self.next_tick += TICK;
        }
    }

    fn tick(&mut self, now: Instant) {
        let mut heard = Vec::with_capacity(self.legs.len());
        for (id, leg) in &mut self.legs {
            let mut frame = [0.0; FRAME];
            leg.take(&mut frame);
            let level = audio::dbfs(&frame);
            let active = self.activity.entry(id.clone()).or_default().hear(level);
            self.speakers.hear(leg.sources.audio, level);
            heard.push(Heard { frame, active });
        }
        for (listener, leg) in self.legs.values_mut().enumerate() {
            leg.send(audio::mix_for(listener, &heard).as_ref(), now);
        }
        self.tell_speakers(now);
    }

    fn tell_speakers(&mut self, now: Instant) {
        let history = self.speakers.history();
        let due = self.told_at.is_none_or(|at| now.duration_since(at) >= SPEAKERS_EVERY);
        if history == self.told.as_slice() || !due {
            return;
        }
        self.told = history.to_vec();
        self.told_at = Some(now);
        for leg in self.legs.values_mut() {
            leg.tell_speakers(&self.told);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::calling::Sources;
    use crate::mixer::leg::tests::{SOURCES, group_client, making};

    const B: Sources = Sources {
        audio: 1020,
        video: 1021,
        sharing: 1022,
        data: 1023,
    };

    /// A room of a and b, each from a client's group offer.
    fn room(now: Instant) -> Room {
        let mut room = Room::new(now);
        for (id, sources) in [("a", SOURCES), ("b", B)] {
            let leg = Leg::new(&making(), sources, Some(&group_client()), now).unwrap();
            room.legs.insert(id.to_owned(), leg);
        }
        room
    }

    fn requested(stream: u32, source: i64) -> Driven {
        Driven {
            requests: vec![SourceRequest {
                stream,
                source,
                max_fs: None,
            }],
            ..Driven::default()
        }
    }

    fn shows(room: &Room, leg: &str, stream: u32) -> Option<u32> {
        room.slots.get(&(leg.to_owned(), stream)).and_then(Slot::source)
    }

    #[test]
    fn a_line_shows_what_its_leg_asked_and_its_decoders_wish_reaches_the_source() {
        let now = Instant::now();
        let mut room = room(now);
        // b asks its first receiving line for a's camera; a line of nobody's, or of a's, is nothing
        room.driven("b", requested(1024, 1001), now);
        room.driven("b", requested(99_999, 1001), now);
        room.driven("b", requested(1004, 1001), now);
        room.driven("nobody", requested(1024, 1001), now);
        assert_eq!(shows(&room, "b", 1024), Some(1001));
        assert_eq!(room.slots.len(), 1);
        assert!(!room.asked.due(1001, now), "a was asked for a keyframe");
        // b's decoder wants a keyframe on that line: a is asked again, once the last ask is old enough
        let later = now + super::super::video::KEYFRAME_EVERY;
        let wish = Driven {
            keyframes: vec![1024, 1025],
            ..Driven::default()
        };
        room.driven("b", wish, later);
        assert!(!room.asked.due(1001, later));
        // b's sharing line shows a's screen; a wish on that line reaches a's screen
        room.driven("b", requested(B.sharing, 1002), now);
        assert_eq!(shows(&room, "b", B.sharing), Some(1002));
        let wish = Driven {
            keyframes: vec![B.sharing],
            ..Driven::default()
        };
        room.driven("b", wish, later);
        assert!(!room.asked.due(1002, later), "a was asked for its screen's keyframe");
    }

    #[test]
    fn a_frame_reaches_a_seeded_line_from_a_keyframe_on() {
        let later = Instant::now();
        let mut room = room(later);
        room.driven("b", requested(1024, 1001), later);
        // a's frames: none to b before it seeded, then from a keyframe on
        let params = room.legs.get_mut("a").unwrap().video_params().unwrap();
        let frame = |keyframe| Frame {
            source: 1001,
            time: 90_000,
            keyframe,
            params,
            data: Arc::from(&[0_u8, 0, 0, 1, 0x65][..]),
        };
        let sent = |keyframe| Driven {
            frames: vec![frame(keyframe)],
            ..Driven::default()
        };
        room.driven("a", sent(true), later);
        assert!(
            room.slots
                .values()
                .all(|slot| slot.clone().pass(false, 1, later).is_none()),
            "still waiting"
        );
        room.legs.get_mut("b").unwrap().seed = Some(4_000_000);
        room.driven("a", sent(false), later);
        room.driven("a", sent(true), later);
        assert!(
            room.slots
                .values()
                .all(|slot| slot.clone().pass(false, 1, later).is_some()),
            "showing"
        );
        room.remove("b");
        assert!(room.slots.is_empty());
    }

    #[test]
    fn a_screen_reaches_only_the_lines_whose_limit_its_pictures_fit() {
        let now = Instant::now();
        let mut room = room(now);
        room.legs.get_mut("b").unwrap().seed = Some(4_000_000);
        let asked = |max_fs| Driven {
            requests: vec![SourceRequest {
                stream: B.sharing,
                source: 1002,
                max_fs: Some(max_fs),
            }],
            ..Driven::default()
        };
        let params = room.legs.get_mut("a").unwrap().video_params().unwrap();
        let screen = |keyframe: bool| Driven {
            frames: vec![Frame {
                source: 1002,
                time: 90_000,
                keyframe,
                params,
                data: Arc::from(crate::mixer::sps::tests::keyframe(66, (1920, 1080), 2)),
            }],
            ..Driven::default()
        };
        let showing = |room: &Room| {
            room.slots
                .get(&("b".to_owned(), B.sharing))
                .is_some_and(|slot| slot.clone().pass(false, 1, now).is_some())
        };
        // b asks for a's screen at 720p: a's 1080p keyframe doesn't fit, nothing goes
        room.driven("b", asked(3600), now);
        room.driven("a", screen(true), now);
        assert_eq!(room.sizes.get(&1002), Some(&8160));
        assert!(!showing(&room));
        // asked at 1080p, as the web client asks for a screen: from the next keyframe on
        room.driven("b", asked(8160), now);
        room.driven("a", screen(false), now);
        assert!(!showing(&room), "still waiting for a keyframe");
        room.driven("a", screen(true), now);
        assert!(showing(&room));
        room.remove("a");
        assert!(room.sizes.is_empty());
    }
}
