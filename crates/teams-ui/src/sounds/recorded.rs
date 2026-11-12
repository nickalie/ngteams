//! The one app pack that is recorded rather than played from notes: Big Beat Riot, a 90s big-beat set (breakbeat hits,
//! distorted guitar stabs, acid squelches, rave sirens) recorded for this app and kept in `assets/sounds/big-beat/`.
//!
//! Each file is mono Ogg Vorbis at the device's rate, a seventh of the size of the same as WAV, decoded as an imported
//! pack's sounds are ([`sound_file::decode`], `GStreamer`'s `ogg` and `vorbis` plugins, which the release ships), once
//! per sound, on the playback thread. A one-shot has no silence before it, so it is heard the moment it plays (a whoosh
//! still swells). The two ringing events are seamless loops a few bars long, played over and over as they are (their
//! length isn't a whole number of frames, so [`super::board`] wraps a loop mid-frame). Every sound is made to peak at
//! about 0.45; one that decodes louder than [`MAX_PEAK`] is brought down to it, as an imported sound is.

use teams_media::sound_file;

use super::SoundEvent;
use super::pack::{MAX_PEAK, MAX_SECONDS};

/// The pack's key.
pub const KEY: &str = "big-beat";

macro_rules! sound {
    ($name:literal) => {
        include_bytes!(concat!("../../assets/sounds/big-beat/", $name, ".ogg"))
    };
}

/// `event`'s file.
fn file(event: SoundEvent) -> &'static [u8] {
    use SoundEvent as E;
    match event {
        E::Message => sound!("message"),
        E::Mention => sound!("mention"),
        E::ChannelMessage => sound!("channel-message"),
        E::MissedCall => sound!("missed-call"),
        E::IncomingCall => sound!("incoming-call"),
        E::OutgoingCall => sound!("outgoing-call"),
        E::CallJoined => sound!("call-joined"),
        E::CallEnded => sound!("call-ended"),
        E::Reconnecting => sound!("reconnecting"),
        E::Reconnected => sound!("reconnected"),
        E::PersonJoined => sound!("person-joined"),
        E::PersonLeft => sound!("person-left"),
        E::LobbyRequest => sound!("lobby-request"),
        E::OnHold => sound!("on-hold"),
        E::Resumed => sound!("resumed"),
        E::RecordingStarted => sound!("recording-started"),
        E::RecordingStopped => sound!("recording-stopped"),
        E::ScreenShareStarted => sound!("screen-share-started"),
        E::ScreenShareEnded => sound!("screen-share-ended"),
        E::HandRaised => sound!("hand-raised"),
        E::LiveReaction => sound!("live-reaction"),
        E::MicMuted => sound!("mic-muted"),
        E::MicUnmuted => sound!("mic-unmuted"),
    }
}

/// `event`'s sound at the device's rate; `None` when its file doesn't read (said in the log).
#[must_use]
pub fn render(event: SoundEvent) -> Option<Vec<f32>> {
    decoded(event, file(event))
}

fn decoded(event: SoundEvent, bytes: &[u8]) -> Option<Vec<f32>> {
    let mut samples = match sound_file::decode(bytes, MAX_SECONDS) {
        Ok(decoded) => decoded.at_device_rate(),
        Err(error) => {
            log::warn!("the {KEY} pack's {} sound can't be read: {error}", event.key());
            return None;
        }
    };
    let peak = samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak > MAX_PEAK {
        for sample in &mut samples {
            *sample *= MAX_PEAK / peak;
        }
    }
    Some(samples)
}

#[cfg(test)]
mod tests {
    use teams_media::devices::SAMPLE_RATE;

    use super::*;

    const MS: usize = (SAMPLE_RATE / 1_000) as usize;

    #[test]
    fn every_event_has_its_own_recording() {
        let sounds: Vec<Vec<f32>> = SoundEvent::ALL
            .into_iter()
            .map(|event| render(event).unwrap_or_else(|| panic!("{event:?} doesn't read")))
            .collect();
        for (index, sound) in sounds.iter().enumerate() {
            assert!(
                !sounds[index + 1..].contains(sound),
                "{:?} shares its file",
                SoundEvent::ALL[index]
            );
        }
    }

    #[test]
    fn a_one_shot_starts_at_once_and_ends_quiet() {
        for event in SoundEvent::ALL.into_iter().filter(|event| !event.loops()) {
            let sound = render(event).unwrap_or_default();
            // above -50 dB, the level silence was trimmed to: a whoosh swells, but never after a gap
            assert!(
                sound[..50 * MS].iter().any(|sample| sample.abs() > 0.003),
                "{event:?} starts after a silence"
            );
            assert!(sound.len() <= 3_000 * MS, "{event:?} is an alert, not a phrase");
            assert!(sound[sound.len() - 1].abs() < 0.01, "{event:?} ends on a click");
        }
    }

    #[test]
    fn a_ring_loops_without_a_jump() {
        for event in SoundEvent::ALL.into_iter().filter(|event| event.loops()) {
            let sound = render(event).unwrap_or_default();
            let (first, last) = (sound[0], sound[sound.len() - 1]);
            assert!(
                (first - last).abs() < 0.1,
                "{event:?} jumps from {last} to {first} as it starts over"
            );
        }
    }

    #[test]
    fn a_file_that_doesnt_read_is_none() {
        assert_eq!(decoded(SoundEvent::Message, b"OggS but not a stream"), None);
    }
}
