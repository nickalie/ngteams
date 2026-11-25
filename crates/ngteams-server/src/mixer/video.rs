//! A room's video: forwarded, never transcoded (`docs/protocol/calling.md`, "Group video").
//!
//! - **The lines** ([`video_lines`]): each leg has the main video line (its camera goes out on it), the receiving lines
//!   (each named by a stream id of its own) and the sharing line (its screen goes out on it, someone else's comes in
//!   on it). Which is which is read from the exchange the leg made: str0m's SDP and the dialect's, line for line.
//! - **What a receiving line shows** is what the leg's last source request for its stream id named ([`Slot`]): a
//!   participant's camera or screen by its source id, or nothing (`-1`). A line starts showing a source at the
//!   source's next keyframe, which the room asks the source for, and its RTP time goes on from where the line was, by
//!   the time since its last frame, so a line that changes source is one stream to the client's decoder. A line that
//!   went on by one frame after a pause (a second share on the same line) made the receiver take the pause as jitter
//!   and hold its frames for a second, past the share's end.
//! - **The largest picture a line takes** is its source request's `max-fs`: as Teams' mixer, this one never scales a
//!   stream down, so a source whose keyframes carry larger pictures ([`super::sps`]) shows nothing on that line (a
//!   1080p share reaches no line that asked for 720p).
//! - **Keyframes**: a receiver's PLI or FIR on a line, and a line that starts a source, ask that source's leg for one;
//!   a source is asked at most every [`KEYFRAME_EVERY`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::format::PayloadParams;
use str0m::media::Mid;
use teams_sdp::sdp::SHARING_LABEL;

/// A source is asked for a keyframe at most this often.
pub(crate) const KEYFRAME_EVERY: Duration = Duration::from_millis(300);
/// One frame's step in RTP time (90 kHz at 30 fps): the least a line goes on by with another source.
const FRAME_STEP: i64 = 3000;
/// RTP time's ticks a second (90 kHz).
const TICKS: i64 = 90_000;

/// What a video line of a leg carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// The leg's camera.
    Main,
    /// A receiving line, by its stream id.
    Slot(u32),
    /// The leg's screen going out, or someone's coming in.
    Sharing,
}

/// One video line of a leg: str0m's mid and what it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VideoLine {
    pub mid: Mid,
    pub kind: LineKind,
}

/// The m-sections of an SDP, each its m-line and the lines under it.
fn sections(sdp: &str) -> Vec<Vec<&str>> {
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for line in sdp.lines() {
        if line.starts_with("m=") {
            sections.push(vec![line]);
        } else if let Some(section) = sections.last_mut() {
            section.push(line);
        }
    }
    sections
}

fn value<'a>(section: &[&'a str], name: &str) -> Option<&'a str> {
    let prefix = format!("a={name}:");
    section.iter().find_map(|line| line.strip_prefix(prefix.as_str()))
}

/// The video lines in use of an exchange, from str0m's SDP (`standard`) and the dialect's (`written`), which have the
/// same m-lines in the same order: the first `main-video` line is the main one, each other the receiving line of the
/// stream id it names, and the `applicationsharing-video` line the sharing one.
pub(crate) fn video_lines(standard: &str, written: &str) -> Vec<VideoLine> {
    let mut lines = Vec::new();
    let mut main = false;
    for (ours, theirs) in sections(standard).iter().zip(sections(written).iter()) {
        let in_use = theirs
            .first()
            .is_some_and(|m| m.starts_with("m=video ") && !m.starts_with("m=video 0 "));
        let Some(mid) = value(ours, "mid").filter(|_| in_use) else {
            continue;
        };
        let kind = if value(theirs, "label") == Some(SHARING_LABEL) {
            LineKind::Sharing
        } else if !main {
            main = true;
            LineKind::Main
        } else {
            match value(theirs, "x-source-streamid").and_then(|id| id.parse().ok()) {
                Some(stream) => LineKind::Slot(stream),
                None => continue,
            }
        };
        lines.push(VideoLine { mid: mid.into(), kind });
    }
    lines
}

/// A frame a leg sent on its main or sharing line, as its participant's source `source`.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub source: u32,
    /// Its RTP time (90 kHz).
    pub time: u64,
    pub keyframe: bool,
    pub params: PayloadParams,
    pub data: Arc<[u8]>,
}

/// One receiving line of a leg: the source it shows (by the last source request for it), and where its stream is.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slot {
    source: Option<u32>,
    /// The largest picture it takes, in macroblocks (none: any).
    max_fs: Option<u32>,
    /// It shows nothing until the source's next keyframe.
    waiting: bool,
    /// What the source's RTP time is moved by on this line.
    offset: i64,
    /// The RTP time of the line's last frame, and when it went.
    last_out: Option<(u64, Instant)>,
}

impl Slot {
    /// The source the line shows from now on (none for `-1` or what isn't a source id); whether that changed.
    pub(crate) fn show(&mut self, source: i64) -> bool {
        let source = u32::try_from(source).ok();
        if source == self.source {
            return false;
        }
        self.source = source;
        self.waiting = source.is_some();
        true
    }

    pub(crate) fn source(&self) -> Option<u32> {
        self.source
    }

    /// The largest picture the line takes from now on, in macroblocks (none: any).
    pub(crate) fn limit(&mut self, max_fs: Option<u32>) {
        self.max_fs = max_fs;
    }

    /// Whether pictures of `size` macroblocks (none: not known) fit the line. One that doesn't makes the line wait for
    /// a keyframe again, so it shows its source again only from one that fits.
    pub(crate) fn fits(&mut self, size: Option<u32>) -> bool {
        let fits = size.zip(self.max_fs).is_none_or(|(size, max_fs)| size <= max_fs);
        if !fits {
            self.waiting = self.source.is_some();
        }
        fits
    }

    /// The RTP time a frame of the source at `time` goes out at on this line at `now`; none while the line waits for a
    /// keyframe and this isn't one.
    pub(crate) fn pass(&mut self, keyframe: bool, time: u64, now: Instant) -> Option<u64> {
        let time = i64::try_from(time).ok()?;
        if self.waiting {
            if !keyframe {
                return None;
            }
            self.waiting = false;
            self.offset = self.last_out.map_or(0, |(last, at)| {
                let since = i64::try_from(now.duration_since(at).as_millis()).unwrap_or(i64::MAX / TICKS);
                let last = i64::try_from(last).unwrap_or_default();
                last.saturating_add(since.saturating_mul(TICKS / 1000).max(FRAME_STEP)) - time
            });
        }
        let out = u64::try_from(time + self.offset).unwrap_or_default();
        self.last_out = Some((out, now));
        Some(out)
    }
}

/// When each source was last asked for a keyframe.
#[derive(Debug, Default)]
pub(crate) struct Asked(BTreeMap<u32, Instant>);

impl Asked {
    /// Whether `source` may be asked again at `now` (and it is taken as asked).
    pub(crate) fn due(&mut self, source: u32, now: Instant) -> bool {
        let due = self
            .0
            .get(&source)
            .is_none_or(|at| now.duration_since(*at) >= KEYFRAME_EVERY);
        if due {
            self.0.insert(source, now);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STANDARD: &str = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 107\r\na=mid:1\r\nm=video 9 UDP/TLS/RTP/SAVPF 107\r\na=mid:2\r\n\
        m=video 0 UDP/TLS/RTP/SAVPF 107\r\na=mid:3\r\nm=video 9 UDP/TLS/RTP/SAVPF 107\r\na=mid:4\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 107\r\na=mid:5\r\nm=video 9 UDP/TLS/RTP/SAVPF 107\r\na=mid:6\r\n\
        m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:7\r\n";
    const WRITTEN: &str = "v=0\r\nm=audio 3480 RTP/SAVP 111\r\na=label:main-audio\r\na=x-source-streamid:1000\r\n\
        m=video 3480 RTP/SAVP 107\r\na=label:main-video\r\na=x-source-streamid:1001\r\n\
        m=video 3480 RTP/SAVP 107\r\na=label:main-video\r\na=x-source-streamid:1004\r\n\
        m=video 0 RTP/SAVP 34\r\na=label:main-video\r\n\
        m=video 3480 RTP/SAVP 107\r\na=label:main-video\r\na=x-source-streamid:1005\r\n\
        m=video 3480 RTP/SAVP 107\r\na=label:main-video\r\n\
        m=video 3480 RTP/SAVP 107\r\na=label:applicationsharing-video\r\na=x-source-streamid:1002\r\n\
        m=x-data 3480 RTP/SAVP 127 126\r\na=label:data\r\n";

    #[test]
    fn the_lines_are_told_apart_by_their_place_and_label() {
        let lines = video_lines(STANDARD, WRITTEN);
        let kinds: Vec<(String, LineKind)> = lines.iter().map(|line| (line.mid.to_string(), line.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("1".to_owned(), LineKind::Main),
                ("2".to_owned(), LineKind::Slot(1004)),
                ("4".to_owned(), LineKind::Slot(1005)),
                ("6".to_owned(), LineKind::Sharing),
            ]
        );
    }

    #[test]
    fn a_line_shows_its_source_from_a_keyframe_on_and_goes_on_in_time() {
        let now = Instant::now();
        let mut slot = Slot::default();
        assert!(!slot.show(-1), "nothing was shown before");
        assert!(slot.show(1001));
        assert!(!slot.show(1001));
        assert_eq!(slot.source(), Some(1001));
        assert_eq!(slot.pass(false, 90_000, now), None);
        assert_eq!(slot.pass(true, 93_000, now), Some(93_000));
        assert_eq!(slot.pass(false, 96_000, now), Some(96_000));
        // another source, its own clock, at once: the line goes on one frame after where it was
        assert!(slot.show(1021));
        assert_eq!(slot.pass(false, 5_000, now), None);
        assert_eq!(slot.pass(true, 8_000, now), Some(99_000));
        assert_eq!(slot.pass(false, 11_000, now), Some(102_000));
        assert!(slot.show(-1));
        assert_eq!(slot.source(), None);
        assert_eq!(slot.pass(false, u64::MAX, now), None, "a time past i64 is no time");
    }

    #[test]
    fn a_line_that_shows_a_source_again_after_a_pause_goes_on_by_the_pause() {
        let now = Instant::now();
        let mut slot = Slot::default();
        slot.show(1022);
        assert_eq!(slot.pass(true, 90_000, now), Some(90_000));
        // the share ends, and starts again four seconds later: its RTP time went on meanwhile, and so does the line's
        slot.show(-1);
        slot.show(1022);
        let later = now + Duration::from_secs(4);
        assert_eq!(slot.pass(true, 500_000, later), Some(90_000 + 4 * 90_000));
        assert_eq!(slot.pass(false, 506_000, later), Some(456_000));
    }

    #[test]
    fn a_line_takes_only_pictures_that_fit_its_limit() {
        let now = Instant::now();
        let mut slot = Slot::default();
        slot.show(1022);
        assert!(slot.fits(Some(8160)), "no limit asked: any size");
        slot.limit(Some(3600));
        assert!(slot.fits(Some(3600)));
        assert!(slot.fits(None), "a size not known yet");
        assert_eq!(slot.pass(true, 90_000, now), Some(90_000));
        // a 1080p screen on a line that asked for 720p: nothing, and the line waits for a keyframe that fits
        assert!(!slot.fits(Some(8160)));
        assert_eq!(slot.pass(false, 93_000, now), None);
        slot.limit(Some(8160));
        assert!(slot.fits(Some(8160)));
        assert!(slot.pass(true, 96_000, now).is_some());
    }

    #[test]
    fn a_source_is_asked_for_a_keyframe_at_most_every_little_while() {
        let mut asked = Asked::default();
        let now = Instant::now();
        assert!(asked.due(1001, now));
        assert!(!asked.due(1001, now + Duration::from_millis(100)));
        assert!(asked.due(1021, now));
        assert!(asked.due(1001, now + KEYFRAME_EVERY));
    }
}
