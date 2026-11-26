//! A short `WebM` video made elsewhere, and so untrusted, read as RGBA frames: a Telegram video sticker (VP9, its alpha
//! a second VP9 stream in the blocks' `BlockAdditional`). `GStreamer` reads it, with the video player's restrictions
//! (nothing that fetches or probes: [`start_gstreamer`]): `matroskademux`, then `vp9alphadecodebin` (gst-plugins-bad)
//! for a stream with alpha, or `vp9dec` (gst-plugins-good) for one without. Its plugins are the ones the app's packages
//! already install.
//!
//! Only a file that starts like a Matroska file reaches `GStreamer`. Each frame goes to the caller as it is decoded,
//! so a caller that keeps a smaller copy never holds the whole video; the caller stops the read when it has enough.
//! Past [`DEADLINE`] the read stops whatever it does, so a file made to stall holds nothing up. A stream that is not
//! video (a sound track) goes to a `fakesink`.
//!
//! `vp9alphadecodebin` often gives no frame for a stream's last block (`GStreamer` 1.28, also through `decodebin3`),
//! so the read returns the length the file declares, and the caller lets the frame before it last to that end.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::player::start_gstreamer;

/// The longest one file is worked on.
pub const DEADLINE: Duration = Duration::from_secs(20);
/// How long the appsink is waited on before the bus and the clock are looked at again.
const POLL: Duration = Duration::from_millis(50);

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

/// A pipeline set to nothing when dropped, on every way out.
struct Running(gst::Pipeline);

impl Drop for Running {
    fn drop(&mut self) {
        // a pipeline that won't stop has nothing more to say
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn element(factory: &str) -> Result<gst::Element, VideoFileError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| VideoFileError::NoDecoder)
}

/// Whether `pad` gives raw video.
fn is_video(pad: &gst::Pad) -> bool {
    pad.current_caps()
        .or_else(|| Some(pad.query_caps(None)))
        .and_then(|caps| caps.structure(0).map(|s| s.name().starts_with("video/")))
        .unwrap_or(false)
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

/// `bytes` through `decodebin` to an appsink of RGBA frames.
fn pipeline(bytes: &[u8]) -> Result<(Running, gst_app::AppSink), VideoFileError> {
    start_gstreamer().map_err(|_| VideoFileError::NoDecoder)?;
    let pipeline = gst::Pipeline::new();
    let source = gst_app::AppSrc::builder().format(gst::Format::Bytes).build();
    let decode = element("decodebin")?;
    let convert = element("videoconvert")?;
    let caps = gst::Caps::builder("video/x-raw").field("format", "RGBA").build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
        .max_buffers(4)
        .build();
    let linked = (|| {
        pipeline.add_many([source.upcast_ref(), &decode, &convert, sink.upcast_ref()])?;
        source.link(&decode)?;
        convert.link(&sink)
    })();
    linked.map_err(|_| VideoFileError::NoDecoder)?;
    let target = convert.static_pad("sink").ok_or(VideoFileError::NoDecoder)?;
    let weak = pipeline.downgrade();
    decode.connect_pad_added(move |_, pad| {
        if is_video(pad) && !target.is_linked() {
            // an unlinked pad ends as an error on the bus, which is told
            let _ = pad.link(&target);
        } else if let Some(pipeline) = weak.upgrade() {
            drain(&pipeline, pad);
        }
    });
    source
        .push_buffer(gst::Buffer::from_slice(bytes.to_vec()))
        .map_err(|_| VideoFileError::NoDecoder)?;
    source.end_of_stream().map_err(|_| VideoFileError::NoDecoder)?;
    let running = Running(pipeline);
    running
        .0
        .set_state(gst::State::Playing)
        .map_err(|_| VideoFileError::Damaged)?;
    Ok((running, sink))
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
/// breaks. Returns the video's length, if the file declares one.
///
/// # Errors
/// It is not a `WebM` file, no installed plugin decodes it, it is damaged, or it takes longer than [`DEADLINE`].
pub fn decode_webm(
    bytes: &[u8],
    frame: &mut dyn FnMut(RgbaFrame) -> ControlFlow<()>,
) -> Result<Option<Duration>, VideoFileError> {
    if !is_webm(bytes) {
        return Err(VideoFileError::NotWebm);
    }
    let (running, sink) = pipeline(bytes)?;
    let bus = running.0.bus().ok_or(VideoFileError::NoDecoder)?;
    let started = Instant::now();
    let timeout = gst::ClockTime::from_mseconds(u64::try_from(POLL.as_millis()).unwrap_or(50));
    loop {
        if started.elapsed() > DEADLINE {
            return Err(VideoFileError::TooSlow);
        }
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            return Err(failure(&message));
        }
        let Some(sample) = sink.try_pull_sample(timeout) else {
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
