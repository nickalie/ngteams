//! The part the readers of untrusted files ([`crate::sound_file`], [`crate::video_file`]) share: the file's bytes
//! from an appsrc through `decodebin` to a chain that ends in an appsink, with the video player's restrictions
//! ([`start_gstreamer`]), and a pipeline that stops on every way out. Each reader links `decodebin`'s pads itself.

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::player::start_gstreamer;

/// The longest one file is worked on.
pub const DEADLINE: Duration = Duration::from_secs(20);
/// How long the appsink is waited on before the bus and the clock are looked at again.
const POLL: Duration = Duration::from_millis(50);

/// Why a pipeline was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupError {
    /// `GStreamer` did not start.
    NoGstreamer,
    /// An element of the pipeline is not installed.
    Missing,
    /// The elements did not link, or the bytes did not go in.
    Setup,
    /// The pipeline did not start to play.
    NotPlayed,
}

/// A pipeline set to nothing when dropped, on every way out.
pub(crate) struct Running(pub(crate) gst::Pipeline);

impl Drop for Running {
    fn drop(&mut self) {
        // a pipeline that won't stop has nothing more to say
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// A pipeline that holds the bytes, not yet playing: `decode` is its `decodebin`, `target` the sink pad of the chain's
/// first element.
pub(crate) struct Decoding {
    pub(crate) pipeline: gst::Pipeline,
    pub(crate) decode: gst::Element,
    pub(crate) target: gst::Pad,
    pub(crate) sink: gst_app::AppSink,
}

pub(crate) fn element(factory: &str) -> Result<gst::Element, SetupError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| SetupError::Missing)
}

/// `bytes` through `decodebin`, then `chain` (element factories), then an appsink of `caps` (made once `GStreamer`
/// runs) that holds at most `max_buffers`.
pub(crate) fn decoding(
    bytes: &[u8],
    chain: &[&str],
    caps: impl FnOnce() -> gst::Caps,
    max_buffers: u32,
) -> Result<Decoding, SetupError> {
    start_gstreamer().map_err(|_| SetupError::NoGstreamer)?;
    let pipeline = gst::Pipeline::new();
    let source = gst_app::AppSrc::builder().format(gst::Format::Bytes).build();
    let decode = element("decodebin")?;
    let mut elements = chain
        .iter()
        .map(|factory| element(factory))
        .collect::<Result<Vec<_>, _>>()?;
    let sink = gst_app::AppSink::builder()
        .caps(&caps())
        .sync(false)
        .max_buffers(max_buffers)
        .build();
    elements.push(sink.clone().upcast());
    let linked = (|| {
        pipeline.add_many([source.upcast_ref(), &decode])?;
        pipeline.add_many(&elements)?;
        source.link(&decode)?;
        gst::Element::link_many(&elements)
    })();
    linked.map_err(|_| SetupError::Setup)?;
    let target = elements
        .first()
        .and_then(|first| first.static_pad("sink"))
        .ok_or(SetupError::Setup)?;
    source
        .push_buffer(gst::Buffer::from_slice(bytes.to_vec()))
        .map_err(|_| SetupError::Setup)?;
    source.end_of_stream().map_err(|_| SetupError::Setup)?;
    Ok(Decoding {
        pipeline,
        decode,
        target,
        sink,
    })
}

/// Sets `pipeline` playing, to be stopped when the result is dropped.
pub(crate) fn play(pipeline: gst::Pipeline) -> Result<Running, SetupError> {
    let running = Running(pipeline);
    running
        .0
        .set_state(gst::State::Playing)
        .map_err(|_| SetupError::NotPlayed)?;
    Ok(running)
}

/// How long one pull from the appsink waits.
pub(crate) fn poll() -> gst::ClockTime {
    gst::ClockTime::from_mseconds(u64::try_from(POLL.as_millis()).unwrap_or(50))
}

/// What `pad` gives: its caps, or what it can give when it has none yet.
pub(crate) fn caps_of(pad: &gst::Pad) -> gst::Caps {
    pad.current_caps().unwrap_or_else(|| pad.query_caps(None))
}

/// Whether `caps`' media type starts with `prefix`.
pub(crate) fn is_type(caps: &gst::CapsRef, prefix: &str) -> bool {
    caps.structure(0).is_some_and(|s| s.name().starts_with(prefix))
}
