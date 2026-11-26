//! A short `WebM` video made elsewhere, and so untrusted, read as RGBA frames: a Telegram video sticker (VP9, its alpha
//! a second VP9 stream in the blocks' `BlockAdditional`). `GStreamer` reads it, with the video player's restrictions
//! (nothing that fetches or probes: [`start_gstreamer`]): `matroskademux`, then `vp9alphadecodebin` (gst-plugins-bad)
//! for a stream with alpha, or `vp9dec` (gst-plugins-good) for one without. Its plugins are the ones the app's packages
//! already install.
//!
//! Only a file that starts like a Matroska file reaches `GStreamer`. A stream with alpha (VP8 or VP9) is decoded only
//! when the alpha decoders are installed, and a stream is decoded only when the frame size it declares is in the
//! caller's limit; the appsink takes no larger frame, so a frame size the stream changes to is never converted either.
//! The decoder itself takes the size from the bitstream (`vp9parse` keeps the declared one), so a file that declares
//! a smaller size than it holds is decoded at its real size before it is refused. Each
//! frame goes to the caller as it is decoded, so a caller that keeps a smaller copy never holds the whole video; the
//! caller stops the read when it has enough. Past [`DEADLINE`] the read stops whatever it does, so a file made to
//! stall holds nothing up. A stream that is not video (a sound track) goes to a `fakesink`; a file with no video ends
//! when `decodebin` has no more pads.
//!
//! `vp9alphadecodebin` often gives no frame for a stream's last block (`GStreamer` 1.28, also through `decodebin3`),
//! so the read returns the length the file declares, and the caller lets the frame before it last to that end.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::decoding::{Decoding, Running, SetupError, caps_of, decoding, is_type, play, poll};
use crate::player::start_gstreamer;

/// The longest one file is worked on.
pub const DEADLINE: Duration = crate::decoding::DEADLINE;
/// The decoders that keep a stream's alpha (codecalpha, gst-plugins-bad); `vp8dec` and `vp9dec` alone drop it.
const ALPHA_DECODERS: [&str; 2] = ["vp8alphadecodebin", "vp9alphadecodebin"];

/// Why a video file gave no frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VideoFileError {
    #[error("not a WebM file")]
    NotWebm,
    #[error("no installed GStreamer plugin decodes it")]
    NoDecoder,
    #[error("a damaged file")]
    Damaged,
    #[error("it took too long to read")]
    TooSlow,
    #[error("its frames are too large")]
    TooBig,
}

/// One decoded frame: its pixels (RGBA, not premultiplied, rows `width * 4` bytes long) and its place in the video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// When it is shown, from the video's start.
    pub at: Duration,
    /// How long it is shown, if the file says.
    pub length: Option<Duration>,
}

/// Whether `bytes` start like a Matroska (`WebM`) file: its EBML header's ID.
#[must_use]
pub fn is_webm(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
}

/// What a stream must be for its frames to be read.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// The longest side a frame may have.
    max_side: u32,
    /// Whether [`ALPHA_DECODERS`] are installed.
    alpha_decoder: bool,
}

impl Limits {
    /// Why a stream of `caps` is not read, if it is not: a frame too large, or alpha no installed decoder keeps.
    fn refuses(self, caps: &gst::CapsRef) -> Option<VideoFileError> {
        let structure = caps.structure(0)?;
        let side = |field| {
            structure
                .get::<i32>(field)
                .ok()
                .and_then(|side| u32::try_from(side).ok())
        };
        if side("width").is_some_and(|side| side > self.max_side)
            || side("height").is_some_and(|side| side > self.max_side)
        {
            return Some(VideoFileError::TooBig);
        }
        let alpha = structure.get::<bool>("codec-alpha").unwrap_or(false);
        (alpha && !self.alpha_decoder).then_some(VideoFileError::NoDecoder)
    }

    /// The appsink's caps: RGBA frames no larger than the limit, so a larger frame is never converted.
    fn sink_caps(self) -> gst::Caps {
        let side = gst::IntRange::new(1, i32::try_from(self.max_side).unwrap_or(i32::MAX));
        gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", side)
            .field("height", side)
            .build()
    }
}

/// Why the pipeline's own signals stopped the read, the first reason told.
#[derive(Clone, Default)]
struct Verdict(Arc<Mutex<Option<VideoFileError>>>);

impl Verdict {
    fn tell(&self, why: VideoFileError) {
        if let Ok(mut verdict) = self.0.lock() {
            verdict.get_or_insert(why);
        }
    }

    fn get(&self) -> Option<VideoFileError> {
        self.0.lock().ok().and_then(|verdict| *verdict)
    }
}

/// A pad that is not the video's, linked to a new `fakesink`, so its stream does not stop the demuxer.
fn drain(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let Ok(sink) = gst::ElementFactory::make("fakesink").property("sync", false).build() else {
        return;
    };
    if pipeline.add(&sink).is_err() {
        return;
    }
    let _ = sink.sync_state_with_parent();
    if let Some(target) = sink.static_pad("sink") {
        // an unlinked pad ends as an error on the bus, which is told
        let _ = pad.link(&target);
    }
}

/// Stops `decode` before a stream `limits` refuses is decoded: a frame size a stream declares, or its alpha.
fn screen_streams(decode: &gst::Element, limits: Limits, verdict: Verdict) {
    decode.connect("autoplug-continue", false, move |values| {
        let refused = values
            .get(2)
            .and_then(|caps| caps.get::<gst::Caps>().ok())
            .and_then(|caps| limits.refuses(&caps));
        if let Some(why) = refused {
            verdict.tell(why);
        }
        Some(refused.is_none().to_value())
    });
}

/// Links `decode`'s raw video pad to `target`; drains every other pad. A video pad left encoded, or none at all,
/// is told to `verdict`. A frame size the stream changes to is checked at `target`.
fn link_video(decoding: &Decoding, limits: Limits, verdict: &Verdict) {
    let (target, told) = (decoding.target.clone(), verdict.clone());
    let weak = decoding.pipeline.downgrade();
    decoding.decode.connect_pad_added(move |_, pad| {
        let caps = caps_of(pad);
        let raw = is_type(&caps, "video/x-raw");
        if raw && told.get().is_none() && !target.is_linked() {
            // an unlinked pad ends as an error on the bus, which is told
            let _ = pad.link(&target);
            return;
        }
        if is_type(&caps, "video/") && !raw {
            told.tell(VideoFileError::NoDecoder);
        }
        if let Some(pipeline) = weak.upgrade() {
            drain(&pipeline, pad);
        }
    });
    let told = verdict.clone();
    decoding.decode.connect("unknown-type", false, move |values| {
        let video = values
            .get(2)
            .and_then(|caps| caps.get::<gst::Caps>().ok())
            .is_some_and(|caps| is_type(&caps, "video/"));
        if video {
            told.tell(VideoFileError::NoDecoder);
        }
        None
    });
    let (target, told) = (decoding.target.clone(), verdict.clone());
    decoding.decode.connect_no_more_pads(move |_| {
        if !target.is_linked() {
            told.tell(VideoFileError::Damaged);
        }
    });
    let told = verdict.clone();
    decoding
        .target
        .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gst::EventView::Caps(event)) = info.event().map(|event| event.view())
                && let Some(why) = limits.refuses(event.caps())
            {
                told.tell(why);
            }
            gst::PadProbeReturn::Ok
        });
}

/// Why a pipeline was not made, as a video file's error.
fn not_set_up(error: SetupError) -> VideoFileError {
    match error {
        SetupError::NotPlayed => VideoFileError::Damaged,
        SetupError::NoGstreamer | SetupError::Missing | SetupError::Setup => VideoFileError::NoDecoder,
    }
}

/// `bytes` through `decodebin` to an appsink of RGBA frames that `limits` allows.
fn pipeline(bytes: &[u8], limits: Limits, verdict: &Verdict) -> Result<(Running, gst_app::AppSink), VideoFileError> {
    let decoding = decoding(bytes, &["videoconvert"], || limits.sink_caps(), 4).map_err(not_set_up)?;
    screen_streams(&decoding.decode, limits, verdict.clone());
    link_video(&decoding, limits, verdict);
    Ok((play(decoding.pipeline).map_err(not_set_up)?, decoding.sink))
}

/// The frame a sample holds, if it is a whole RGBA frame.
fn frame_of(sample: &gst::Sample) -> Option<RgbaFrame> {
    let structure = sample.caps()?.structure(0)?;
    let width = u32::try_from(structure.get::<i32>("width").ok()?).ok()?;
    let height = u32::try_from(structure.get::<i32>("height").ok()?).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    let size = usize::try_from(u64::from(width) * u64::from(height) * 4).ok()?;
    if width == 0 || height == 0 || map.size() != size {
        return None;
    }
    Some(RgbaFrame {
        width,
        height,
        rgba: map.as_slice().to_vec(),
        at: Duration::from_nanos(buffer.pts().map_or(0, gst::ClockTime::nseconds)),
        length: buffer.duration().map(|length| Duration::from_nanos(length.nseconds())),
    })
}

/// Reads the `WebM` `bytes` (see the module docs), each frame to `frame` in order, until the video ends or `frame`
/// breaks. A frame larger than `max_side` on a side is never decoded to RGBA. Returns the video's length, if the
/// file declares one.
///
/// # Errors
/// It is not a `WebM` file, no installed plugin decodes it (with its alpha), it is damaged, its frames are larger
/// than `max_side`, or it takes longer than [`DEADLINE`].
pub fn decode_webm(
    bytes: &[u8],
    max_side: u32,
    frame: &mut dyn FnMut(RgbaFrame) -> ControlFlow<()>,
) -> Result<Option<Duration>, VideoFileError> {
    if !is_webm(bytes) {
        return Err(VideoFileError::NotWebm);
    }
    start_gstreamer().map_err(|_| VideoFileError::NoDecoder)?;
    let limits = Limits {
        max_side,
        alpha_decoder: ALPHA_DECODERS
            .iter()
            .all(|factory| gst::ElementFactory::find(factory).is_some()),
    };
    read(bytes, limits, frame)
}

/// [`decode_webm`] within `limits`.
fn read(
    bytes: &[u8],
    limits: Limits,
    frame: &mut dyn FnMut(RgbaFrame) -> ControlFlow<()>,
) -> Result<Option<Duration>, VideoFileError> {
    let verdict = Verdict::default();
    let (running, sink) = pipeline(bytes, limits, &verdict)?;
    let bus = running.0.bus().ok_or(VideoFileError::NoDecoder)?;
    let started = Instant::now();
    loop {
        if let Some(why) = verdict.get() {
            return Err(why);
        }
        if started.elapsed() > DEADLINE {
            return Err(VideoFileError::TooSlow);
        }
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            return Err(verdict.get().unwrap_or_else(|| failure(&message)));
        }
        let Some(sample) = sink.try_pull_sample(poll()) else {
            if sink.is_eos() {
                return Ok(length(&running.0));
            }
            continue;
        };
        let Some(decoded) = frame_of(&sample) else {
            return Err(VideoFileError::Damaged);
        };
        if frame(decoded).is_break() {
            return Ok(length(&running.0));
        }
    }
}

/// The length `pipeline`'s file declares.
fn length(pipeline: &gst::Pipeline) -> Option<Duration> {
    pipeline
        .query_duration::<gst::ClockTime>()
        .map(|length| Duration::from_nanos(length.nseconds()))
}

/// What an error on the bus says: a missing decoder, or a damaged file.
fn failure(message: &gst::Message) -> VideoFileError {
    let gst::MessageView::Error(error) = message.view() else {
        return VideoFileError::Damaged;
    };
    if error.error().matches(gst::CoreError::MissingPlugin) || error.error().matches(gst::StreamError::CodecNotFound) {
        VideoFileError::NoDecoder
    } else {
        VideoFileError::Damaged
    }
}

#[cfg(test)]
#[path = "video_file_tests.rs"]
mod tests;
