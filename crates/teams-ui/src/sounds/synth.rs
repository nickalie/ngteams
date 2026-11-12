//! The app's own sound packs: all but one made here, by code, from notes on a timbre, and that one recorded for the app
//! ([`super::recorded`]). Nothing of anyone else's: Teams' sounds are never copied.
//!
//! A played sound is a motif ([`motif`]: when each note starts, how long it lasts, and how far above the pack's root it
//! is); a played pack ([`PACKS`], [`Voice::Played`]) plays the motifs on its own timbre, root and tempo, its ringtone
//! one of [`RINGS`]. A recorded pack ([`Voice::Recorded`]) plays its recordings instead. Every pack has every event, so
//! the default pack, a played one, can always answer. The default pack's ringtone is the one the app always had.

use std::f32::consts::TAU;

use teams_media::devices::SAMPLE_RATE;

use super::{SoundEvent, recorded};
use crate::ringtone::{RING_FRAMES, RingtoneFrames};

/// The default pack's key.
pub const DEFAULT_PACK: &str = "default";

/// How a note sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Timbre {
    Sine,
    Bell,
    Square,
    Mallet,
    Reed,
    Soft,
}

/// One of the app's packs.
#[derive(Clone, Copy, Debug)]
pub struct Pack {
    pub key: &'static str,
    pub name: &'static str,
    voice: Voice,
}

/// How a pack's sounds are made.
#[derive(Clone, Copy, Debug)]
enum Voice {
    Played(Instrument),
    /// From its recordings ([`recorded`]).
    Recorded,
}

/// What a played pack plays its motifs on.
#[derive(Clone, Copy, Debug)]
struct Instrument {
    timbre: Timbre,
    /// Its root note, in Hz.
    root: f32,
    /// How much longer than the motif it plays, as a percentage.
    tempo: u32,
    /// Which of [`RINGS`] it rings with; `None`: the app's first ringtone.
    ring: Option<usize>,
}

const fn pack(key: &'static str, name: &'static str, timbre: Timbre, root: f32, tempo: u32, ring: usize) -> Pack {
    Pack {
        key,
        name,
        voice: Voice::Played(Instrument {
            timbre,
            root,
            tempo,
            ring: Some(ring),
        }),
    }
}

/// The app's packs.
pub const PACKS: [Pack; 9] = [
    Pack {
        key: DEFAULT_PACK,
        name: "Default",
        voice: Voice::Played(Instrument {
            timbre: Timbre::Sine,
            root: 523.25,
            tempo: 100,
            ring: None,
        }),
    },
    pack("chimes", "Chimes", Timbre::Bell, 659.25, 180, 0),
    pack("retro", "Retro", Timbre::Square, 392.0, 70, 1),
    pack("marimba", "Marimba", Timbre::Mallet, 440.0, 110, 2),
    pack("glass", "Glass", Timbre::Bell, 1_046.5, 130, 3),
    pack("organ", "Organ", Timbre::Reed, 349.23, 160, 4),
    pack("soft", "Soft", Timbre::Soft, 293.66, 150, 5),
    pack("arcade", "Arcade", Timbre::Square, 783.99, 55, 6),
    Pack {
        key: recorded::KEY,
        name: "Big Beat Riot",
        voice: Voice::Recorded,
    },
];

/// One note: when it starts and how long it lasts, in milliseconds, and how many semitones above the root it is.
#[derive(Clone, Copy, Debug)]
struct Note {
    at: u32,
    ms: u32,
    semitones: i32,
}

const fn n(at: u32, ms: u32, semitones: i32) -> Note {
    Note { at, ms, semitones }
}

/// A note's peak; the sounds are as loud as the ringtone always was.
const LEVEL: f32 = 0.2;
/// How long a note takes to rise, in milliseconds: no click as it starts.
const ATTACK_MS: f32 = 4.0;
/// A looping event's sound, its pause included: 2 s, as the ringtone's.
const CYCLE_MS: u32 = 2_000;

/// The ringtones the packs choose among.
const RINGS: [&[Note]; 7] = [
    &[n(0, 400, 0), n(140, 400, 4), n(280, 600, 7)],
    &[
        n(0, 90, 7),
        n(100, 90, 12),
        n(200, 90, 16),
        n(300, 90, 12),
        n(400, 90, 7),
        n(500, 90, 12),
        n(600, 90, 16),
        n(700, 90, 12),
    ],
    &[n(0, 150, 0), n(160, 150, 7), n(320, 150, 12), n(480, 300, 7)],
    &[n(0, 500, 12), n(500, 500, 7)],
    &[n(0, 700, 0), n(0, 700, 7), n(700, 500, 5), n(700, 500, 9)],
    &[n(0, 600, 0), n(650, 600, 3)],
    &[
        n(0, 60, 0),
        n(70, 60, 4),
        n(140, 60, 7),
        n(210, 60, 12),
        n(350, 60, 0),
        n(420, 60, 4),
        n(490, 60, 7),
        n(560, 60, 12),
    ],
];

/// `event`'s notes, the ringtone's aside.
fn motif(event: SoundEvent) -> &'static [Note] {
    use SoundEvent as E;
    match event {
        E::IncomingCall => const { &[n(0, 400, 0), n(400, 400, 7)] },
        E::OutgoingCall => const { &[n(0, 900, -3)] },
        E::Message => const { &[n(0, 90, 9), n(100, 140, 14)] },
        E::Mention => const { &[n(0, 80, 12), n(90, 80, 16), n(180, 140, 19)] },
        E::ChannelMessage => const { &[n(0, 120, 7)] },
        E::MissedCall => const { &[n(0, 160, 7), n(170, 160, 4), n(340, 260, 0)] },
        E::CallJoined => const { &[n(0, 110, 0), n(120, 180, 7)] },
        E::CallEnded => const { &[n(0, 110, 7), n(120, 200, 0)] },
        E::Reconnecting => const { &[n(0, 130, 3), n(200, 130, 3)] },
        E::Reconnected => const { &[n(0, 90, 3), n(100, 90, 7), n(200, 140, 10)] },
        E::PersonJoined => const { &[n(0, 90, 4), n(100, 110, 9)] },
        E::PersonLeft => const { &[n(0, 90, 9), n(100, 110, 4)] },
        E::LobbyRequest => const { &[n(0, 120, 5), n(160, 120, 5)] },
        E::OnHold => const { &[n(0, 220, 0), n(240, 220, -5)] },
        E::Resumed => const { &[n(0, 130, -5), n(150, 180, 0)] },
        E::RecordingStarted => const { &[n(0, 90, 12), n(150, 90, 12), n(300, 90, 12)] },
        E::RecordingStopped => const { &[n(0, 200, 12), n(210, 200, 7)] },
        E::ScreenShareStarted => const { &[n(0, 70, 0), n(80, 70, 5), n(160, 100, 9)] },
        E::ScreenShareEnded => const { &[n(0, 70, 9), n(80, 70, 5), n(160, 100, 0)] },
        E::HandRaised => const { &[n(0, 110, 9), n(120, 70, 12)] },
        E::LiveReaction => const { &[n(0, 60, 16)] },
        E::MicMuted => const { &[n(0, 60, -5)] },
        E::MicUnmuted => const { &[n(0, 60, 2)] },
    }
}

/// The app's pack of this key.
#[must_use]
pub fn find(key: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|pack| pack.key == key)
}

/// The app's pack of this key, by the static key it is kept under.
#[must_use]
pub fn pack_key(key: &str) -> Option<&'static str> {
    find(key).map(|pack| pack.key)
}

/// `seconds` into a note of `hz`, on `timbre`, before its envelope.
fn wave(timbre: Timbre, hz: f32, seconds: f32) -> f32 {
    let phase = TAU * hz * seconds;
    match timbre {
        Timbre::Sine => phase.sin(),
        // a bell's partials, the higher ones dying away faster
        Timbre::Bell => {
            0.75 * phase.sin()
                + 0.2 * (phase * 2.76).sin() * (-seconds * 6.0).exp()
                + 0.05 * (phase * 5.4).sin() * (-seconds * 12.0).exp()
        }
        // a square wave's first odd harmonics: no harsher than it needs to be
        Timbre::Square => {
            (1..=7)
                .step_by(2)
                .map(|harmonic| {
                    let harmonic = small(harmonic);
                    (phase * harmonic).sin() / harmonic
                })
                .sum::<f32>()
                * 0.8
        }
        // a struck bar: the fundamental and its fourth harmonic, gone at once
        Timbre::Mallet => 0.85 * phase.sin() + 0.15 * (phase * 4.0).sin() * (-seconds * 30.0).exp(),
        // a reed's even and odd harmonics, held
        Timbre::Reed => 0.6 * phase.sin() + 0.25 * (phase * 2.0).sin() + 0.15 * (phase * 3.0).sin(),
        // a sine with a little warmth
        Timbre::Soft => 0.92 * phase.sin() + 0.08 * (phase * 2.0).sin(),
    }
}

/// How loud a note is `at` milliseconds into its `ms`, from 0 to 1.
fn envelope(timbre: Timbre, at: f32, ms: f32) -> f32 {
    let rise = (at / ATTACK_MS).min(1.0);
    let fall = match timbre {
        Timbre::Bell | Timbre::Mallet => (-3.5 * at / ms).exp() * (1.0 - at / ms).max(0.0).sqrt(),
        Timbre::Soft => (1.0 - at / ms).max(0.0),
        Timbre::Sine | Timbre::Square | Timbre::Reed => ((ms - at) / (ATTACK_MS * 4.0)).clamp(0.0, 1.0),
    };
    rise * fall
}

#[expect(clippy::cast_precision_loss, reason = "small counts, within an f32's exact range")]
fn small(value: u32) -> f32 {
    value as f32
}

/// Mixes `note`, of `hz`, into `out`.
fn mix(out: &mut [f32], timbre: Timbre, note: Note, hz: f32) {
    let rate = SAMPLE_RATE / 1_000;
    let start = (note.at * rate) as usize;
    let count = (note.ms * rate) as usize;
    let ms = small(note.ms);
    for (index, sample) in out.iter_mut().skip(start).take(count).enumerate() {
        let seconds = small(u32::try_from(index).unwrap_or(u32::MAX)) / small(SAMPLE_RATE);
        *sample =
            (*sample + LEVEL * envelope(timbre, seconds * 1_000.0, ms) * wave(timbre, hz, seconds)).clamp(-0.5, 0.5);
    }
}

/// `event`'s sound in the app's pack `key` (the default pack's for a key that isn't one), at the device's rate: a played
/// pack's looping event is one whole cycle, its pause included; a recorded pack's is its loop as recorded.
#[must_use]
pub fn render(key: &str, event: SoundEvent) -> Vec<f32> {
    match find(key).unwrap_or(&PACKS[0]).voice {
        Voice::Played(instrument) => play(instrument, event),
        Voice::Recorded => recorded::render(event).unwrap_or_else(|| render(DEFAULT_PACK, event)),
    }
}

/// `event`'s sound on `instrument` (see [`render`]).
fn play(instrument: Instrument, event: SoundEvent) -> Vec<f32> {
    let Instrument {
        timbre,
        root,
        tempo,
        ring,
    } = instrument;
    let notes = match (event, ring) {
        (SoundEvent::IncomingCall, None) => {
            return RingtoneFrames::default()
                .take(RING_FRAMES)
                .flat_map(|frame| frame.samples().to_vec())
                .collect();
        }
        (SoundEvent::IncomingCall, Some(ring)) => RINGS[ring],
        _ => motif(event),
    };
    let scale = |ms: u32| ms * tempo / 100;
    let end = notes.iter().map(|note| scale(note.at + note.ms)).max().unwrap_or(0);
    let length = if event.loops() { end.max(CYCLE_MS) } else { end };
    let mut out = vec![0.0; (length * (SAMPLE_RATE / 1_000)) as usize];
    for note in notes {
        let stretched = Note {
            at: scale(note.at),
            ms: scale(note.ms),
            semitones: note.semitones,
        };
        let hz = root * 2.0_f32.powf(f32_of(note.semitones) / 12.0);
        mix(&mut out, timbre, stretched, hz);
    }
    out
}

#[expect(clippy::cast_precision_loss, reason = "a few semitones")]
fn f32_of(value: i32) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use teams_media::devices::FRAME_SAMPLES;
    use teams_media::tone::amplitude_at;

    use super::*;
    use crate::ringtone::RINGTONE;

    #[test]
    fn every_pack_has_every_event_and_stays_in_range() {
        for Pack { key: pack, .. } in PACKS {
            for event in SoundEvent::ALL {
                let sound = render(pack, event);
                assert!(!sound.is_empty(), "{pack} {event:?}");
                assert!(sound.iter().any(|sample| sample.abs() > 0.05), "{pack} {event:?}");
                assert!(sound.iter().all(|sample| sample.abs() <= 0.5), "{pack} {event:?}");
                if event.loops() {
                    assert!(sound.len() >= 96_000, "a whole cycle: {pack} {event:?}");
                }
            }
        }
    }

    #[test]
    fn the_default_ringtone_is_the_one_the_app_always_had() {
        let ring = render(DEFAULT_PACK, SoundEvent::IncomingCall);
        assert_eq!(ring.len(), RING_FRAMES * FRAME_SAMPLES);
        assert!(amplitude_at(&ring[..19_200], RINGTONE[0].hz) > 0.15);
    }

    #[test]
    fn packs_differ_and_an_unknown_key_is_the_default() {
        let event = SoundEvent::Message;
        assert_ne!(render("chimes", event), render("retro", event));
        assert_eq!(render("nope", event), render(DEFAULT_PACK, event));
        assert_eq!(pack_key("chimes"), Some("chimes"));
        assert_eq!(pack_key("nope"), None);
        let rings: Vec<Vec<f32>> = PACKS
            .iter()
            .map(|pack| render(pack.key, SoundEvent::IncomingCall))
            .collect();
        for (index, ring) in rings.iter().enumerate() {
            assert!(!rings[index + 1..].contains(ring), "every pack rings its own way");
        }
    }

    #[test]
    fn a_note_starts_and_ends_at_silence() {
        let sound = render("retro", SoundEvent::MicMuted);
        assert!(sound[0].abs() < 0.01);
        assert!(sound[sound.len() - 1].abs() < 0.05);
    }
}
