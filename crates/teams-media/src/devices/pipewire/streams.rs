//! The device streams the loop thread makes: the microphone, the speaker and the camera, each linked by the session
//! manager to the device chosen for it ([`Target`]: `target.object`) or, with none chosen, to the system's default,
//! which it then follows; and a shared screen's, connected to the node the screen-cast portal named ([`screen`]).
//!
//! A shared screen is offered as a linear DMA-BUF first, then in plain memory ([`screen_formats`]): a compositor may
//! cast only as DMA-BUFs with a negotiated modifier (niri does), and a link where one side's mandatory property is
//! missing from the other's offer gets no format at all, so the stream would err before its first frame and end the
//! share. A DMA-BUF of ours is read on the CPU ([`super::dmabuf`]).
//!
//! A chosen device is pinned with `node.dont-move`: WirePlumber takes a stream whose `target.object` names the
//! current default for one following the default (the rule `PulseAudio` has: it writes `target.node = -1` into the
//! `default` metadata for it), so a device chosen while it is the default would be left for every later default.
//! With `node.dont-move` WirePlumber reads the stream's target only from its own properties, never from that
//! metadata, and still falls back to the default when the chosen node goes, so the call is never left without a
//! device. `node.dont-reconnect` would pin it too, but a stream with it is destroyed when its node goes.
//!
//! A stream hands its media to its device through a [`Feed`], gated by its [`Gate`]: a **live** stream feeds the
//! device and closes its queue when it ends; a stream made to replace it (the device was moved to another node) is
//! **pending** until the session manager has linked it, and feeds nothing (a pending speaker plays silence) while the
//! live one still carries the media; once the loop swaps them, the old one is **retired**: it feeds nothing and
//! closes nothing on its way out. So a device moved to another node never says it closed, and the media doesn't stop
//! while the new link is made.

use std::any::Any;
use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

use pipewire::core::CoreRc;
use pipewire::properties::{PropertiesBox, properties};
use pipewire::spa;
use pipewire::spa::buffer::ChunkFlags;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, ChoiceValue, Pod, PropertyFlags, Value};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};

use crate::devices::{AudioFrame, FRAME_SAMPLES, PixelFormat, SAMPLE_RATE, VideoFrame};
use crate::queue::Queue;

/// The node a stream asks to be linked to; `None` follows the system's default.
pub(super) type Target<'a> = Option<&'a str>;

/// The raw formats a camera may hand over, the first preferred.
const CAMERA_FORMATS: [(VideoFormat, PixelFormat); 9] = [
    (VideoFormat::I420, PixelFormat::I420),
    (VideoFormat::NV12, PixelFormat::Nv12),
    (VideoFormat::YUY2, PixelFormat::Yuy2),
    (VideoFormat::RGBx, PixelFormat::Rgbx),
    (VideoFormat::BGRx, PixelFormat::Bgrx),
    (VideoFormat::RGBA, PixelFormat::Rgba),
    (VideoFormat::BGRA, PixelFormat::Bgra),
    (VideoFormat::RGB, PixelFormat::Rgb),
    (VideoFormat::BGR, PixelFormat::Bgr),
];

/// A stream's part in its device (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Pending,
    Live,
    Retired,
}

/// A stream's role, and what its state has been, shared by the loop and the stream's callbacks.
#[derive(Debug)]
pub(super) struct Gate {
    role: Cell<Role>,
    streaming: Cell<bool>,
    ended: Cell<bool>,
}

impl Gate {
    fn new(role: Role) -> Rc<Self> {
        Rc::new(Self {
            role: Cell::new(role),
            streaming: Cell::new(false),
            ended: Cell::new(false),
        })
    }

    /// A stream that feeds its device from the start (a device opened).
    pub(super) fn live() -> Rc<Self> {
        Self::new(Role::Live)
    }

    /// A stream made to replace a live one, feeding nothing until [`Gate::go_live`].
    pub(super) fn pending() -> Rc<Self> {
        Self::new(Role::Pending)
    }

    pub(super) fn go_live(&self) {
        self.role.set(Role::Live);
    }

    /// The stream is being replaced: it feeds and closes nothing from now on.
    pub(super) fn retire(&self) {
        self.role.set(Role::Retired);
    }

    fn is_live(&self) -> bool {
        self.role.get() == Role::Live
    }

    /// The session manager linked the stream and media flows (it may have stopped since).
    pub(super) fn has_streamed(&self) -> bool {
        self.streaming.get()
    }

    /// The stream erred, or was unlinked or disconnected for good.
    pub(super) fn has_ended(&self) -> bool {
        self.ended.get()
    }
}

/// A stream's end of its device's queue: fed only while the stream is live, closed when a live stream ends or goes.
pub(super) struct Feed<T> {
    queue: Queue<T>,
    gate: Rc<Gate>,
}

impl<T> Feed<T> {
    pub(super) fn new(queue: Queue<T>, gate: Rc<Gate>) -> Self {
        Self { queue, gate }
    }

    /// The queue, while the stream is live.
    fn live(&self) -> Option<&Queue<T>> {
        self.gate.is_live().then_some(&self.queue)
    }

    /// Takes the stream's new state: it streamed, or it ended (closing a live stream's queue).
    fn state(&self, new: &StreamState) {
        match new {
            StreamState::Streaming => self.gate.streaming.set(true),
            StreamState::Error(_) | StreamState::Unconnected => {
                self.gate.ended.set(true);
                if self.gate.is_live() {
                    self.queue.close();
                }
            }
            StreamState::Connecting | StreamState::Paused => {}
        }
    }
}

impl<T> Drop for Feed<T> {
    fn drop(&mut self) {
        if self.gate.is_live() {
            self.queue.close();
        }
    }
}

/// A stream and its listener; the listener goes first (it unhooks itself from the stream).
struct Open<D: 'static> {
    _listener: StreamListener<D>,
    _stream: StreamRc,
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn serialize(object: pod::Object) -> Result<Vec<u8>, String> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("{error:?}"))
}

/// The one audio format every device stream takes: F32 mono at 48 kHz (PipeWire converts).
fn audio_format() -> Result<Vec<u8>, String> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    })
}

/// Connects `stream` offering `formats`, which the link tries in order.
fn connect(stream: &StreamRc, direction: Direction, formats: &[Vec<u8>]) -> Result<(), String> {
    connect_to(stream, direction, None, formats)
}

/// [`connect`], to the node `node` when one is named (a shared screen's).
fn connect_to(stream: &StreamRc, direction: Direction, node: Option<u32>, formats: &[Vec<u8>]) -> Result<(), String> {
    let mut pods = formats
        .iter()
        .map(|format| Pod::from_bytes(format).ok_or("a format PipeWire can't read"))
        .collect::<Result<Vec<_>, _>>()?;
    stream
        .connect(
            direction,
            node,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut pods,
        )
        .map_err(text)
}

/// Keeps a stream on the target its own properties name (see the module docs); the `pipewire` crate has no constant
/// for it.
const NODE_DONT_MOVE: &str = "node.dont-move";

/// A stream named `name` with `props`, asking for `target` when one is chosen, and pinned to it.
fn new_stream(core: &CoreRc, name: &str, mut props: PropertiesBox, target: Target<'_>) -> Result<StreamRc, String> {
    if let Some(target) = target {
        props.insert(*pipewire::keys::TARGET_OBJECT, target);
        props.insert(NODE_DONT_MOVE, "true");
    }
    StreamRc::new(core.clone(), name, props).map_err(text)
}

struct MicrophoneState {
    pending: Vec<f32>,
    frames: Feed<AudioFrame>,
}

pub(super) fn microphone(
    core: &CoreRc,
    frames: Queue<AudioFrame>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-microphone",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
        target,
    )?;
    let state = MicrophoneState {
        pending: Vec::with_capacity(4 * FRAME_SAMPLES),
        frames: Feed::new(frames, gate),
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(queue) = state.frames.live() else {
                return;
            };
            if let Some(data) = buffer.datas_mut().first_mut() {
                let (offset, size) = (to_usize(data.chunk().offset()), to_usize(data.chunk().size()));
                let bytes = data.data().unwrap_or_default();
                let end = offset.saturating_add(size).min(bytes.len());
                let (samples, _) = bytes.get(offset..end).unwrap_or_default().as_chunks::<4>();
                state
                    .pending
                    .extend(samples.iter().map(|sample| f32::from_le_bytes(*sample)));
            }
            while state.pending.len() >= FRAME_SAMPLES {
                if let Some(frame) = AudioFrame::from_samples(&state.pending[..FRAME_SAMPLES]) {
                    queue.push(frame);
                }
                state.pending.drain(..FRAME_SAMPLES);
            }
        })
        .state_changed(|_, state, _, new| state.frames.state(&new))
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Input, &[audio_format()?])?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

pub(super) fn speaker(
    core: &CoreRc,
    queue: Queue<f32>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-speaker",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Playback",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
        target,
    )?;
    let listener = stream
        .add_local_listener_with_user_data(Feed::new(queue, gate))
        .process(|stream, feed| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = to_usize_u64(buffer.requested());
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let written = data.data().map_or(0, |bytes| fill(bytes, requested, feed.live()));
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4;
            *chunk.size_mut() = u32::try_from(written * 4).unwrap_or(0);
        })
        .state_changed(|_, feed, _, new| feed.state(&new))
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Output, &[audio_format()?])?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// Fills a playback buffer with up to `requested` queued samples (all it holds when `requested` is 0), silence
/// where the queue runs out or when there is none to play from (a stream not live); returns how many samples it wrote.
fn fill(bytes: &mut [u8], requested: usize, queue: Option<&Queue<f32>>) -> usize {
    let capacity = bytes.len() / 4;
    let count = if requested == 0 {
        capacity
    } else {
        requested.min(capacity)
    };
    let slots = bytes.as_chunks_mut::<4>().0.iter_mut().take(count);
    match queue {
        Some(queue) => queue.with_items(|queue| {
            for slot in slots {
                slot.copy_from_slice(&queue.pop_front().unwrap_or(0.0).to_le_bytes());
            }
        }),
        None => {
            for slot in slots {
                slot.copy_from_slice(&0.0_f32.to_le_bytes());
            }
        }
    }
    count
}

struct CameraState {
    format: Option<(PixelFormat, u32, u32)>,
    /// Whether the negotiated format names a modifier: the buffers are DMA-BUFs of that (linear) layout.
    dmabuf: bool,
    frames: Feed<VideoFrame>,
    /// The buffers the stream takes ([`camera_buffers`]), answered to every format.
    buffers: Vec<u8>,
}

pub(super) fn camera(
    core: &CoreRc,
    frames: Queue<VideoFrame>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-camera",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Camera",
        },
        target,
    )?;
    video_capture(stream, frames, gate, (None, &camera_formats()?))
}

/// A shared screen's stream: the node `node` the screen-cast portal named, its picture taken at its own size (1080p
/// preferred) in the formats a camera's may come in.
pub(super) fn screen(
    core: &CoreRc,
    frames: Queue<VideoFrame>,
    node: u32,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-screen",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        },
        None,
    )?;
    video_capture(stream, frames, gate, (Some(node), &screen_formats()?))
}

/// A video stream's listeners and its connection (to `node` when one is named, offering `formats`): each buffer's
/// frame queued on `frames` while it is live.
fn video_capture(
    stream: StreamRc,
    frames: Queue<VideoFrame>,
    gate: Rc<Gate>,
    (node, formats): (Option<u32>, &[Vec<u8>]),
) -> Result<Box<dyn Any>, String> {
    let state = CameraState {
        format: None,
        dmabuf: false,
        frames: Feed::new(frames, gate),
        buffers: camera_buffers()?,
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|stream, state, id, param| {
            if id == ParamType::Format.as_raw() {
                state.format = param.and_then(video_format);
                state.dmabuf = param.is_some_and(names_a_modifier);
                if state.format.is_some()
                    && let Some(buffers) = Pod::from_bytes(&state.buffers)
                {
                    // a refused update leaves the link without buffers: the stream errs and the camera reports closed
                    let _ = stream.update_params(&mut [buffers]);
                }
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let dmabuf = state.dmabuf;
            if let (Some(queue), Some(format)) = (state.frames.live(), state.format)
                && let Some(frame) = buffer.datas_mut().first_mut().and_then(|data| {
                    if dmabuf {
                        dmabuf_frame(data, format)
                    } else {
                        frame_of(data, format)
                    }
                })
            {
                queue.push(frame);
            }
        })
        .state_changed(|_, state, _, new| state.frames.state(&new))
        .register()
        .map_err(text)?;
    connect_to(&stream, Direction::Input, node, formats)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// The frame one camera buffer's data holds, in the negotiated `format`; `None` for an empty one.
fn frame_of(data: &mut spa::buffer::Data, (format, width, height): (PixelFormat, u32, u32)) -> Option<VideoFrame> {
    let chunk = data.chunk();
    let (offset, size) = (to_usize(chunk.offset()), to_usize(chunk.size()));
    let stride = u32::try_from(chunk.stride()).unwrap_or(0);
    let bytes = data.data()?;
    let end = offset.saturating_add(size).min(bytes.len());
    let bytes = bytes.get(offset..end).filter(|bytes| !bytes.is_empty())?;
    Some(VideoFrame {
        width,
        height,
        format,
        stride: if stride == 0 { width } else { stride },
        data: bytes.to_vec(),
    })
}

/// The frame a DMA-BUF of the negotiated `format` holds: its first plane mapped and read here, `stride` bytes a row
/// (`width` pixels of 4 bytes when the chunk names none); `None` for a corrupted buffer or one that can't be read.
/// Its chunk's size is ignored: DMA-BUF producers may set it to 0 or 1, so the descriptor supplies the size check.
fn dmabuf_frame(data: &spa::buffer::Data, (format, width, height): (PixelFormat, u32, u32)) -> Option<VideoFrame> {
    let chunk = data.chunk();
    if chunk.flags().contains(ChunkFlags::CORRUPTED) || data.fd() < 0 {
        return None;
    }
    let packed = width.checked_mul(4)?;
    let stride = u32::try_from(chunk.stride())
        .ok()
        .filter(|stride| *stride >= packed)
        .unwrap_or(packed);
    let len = to_usize(stride).checked_mul(to_usize(height))?;
    // SAFETY: the buffer's descriptor stays open while the stream holds the buffer, which it does for this call.
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(data.fd()) };
    let bytes = super::dmabuf::read(fd, to_usize(chunk.offset()), len)?;
    Some(VideoFrame {
        width,
        height,
        format,
        stride,
        data: bytes,
    })
}

/// Whether a negotiated format names a modifier (its buffers are DMA-BUFs of that layout).
fn names_a_modifier(param: &Pod) -> bool {
    let Ok((_, Value::Object(object))) = pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes())
    else {
        return false;
    };
    object
        .properties
        .iter()
        .any(|property| property.key == FormatProperties::VideoModifier.as_raw())
}

/// A negotiated video format we take, as its pixel format and size.
fn video_format(param: &Pod) -> Option<(PixelFormat, u32, u32)> {
    let (media_type, media_subtype) = format_utils::parse_format(param).ok()?;
    if media_type != MediaType::Video {
        return None;
    }
    let format = if media_subtype == MediaSubtype::Mjpg {
        PixelFormat::Mjpeg
    } else if media_subtype == MediaSubtype::Raw {
        let mut info = VideoInfoRaw::new();
        info.parse(param).ok()?;
        CAMERA_FORMATS
            .iter()
            .find(|(video, _)| *video == info.format())
            .map(|(_, pixel)| *pixel)?
    } else {
        return None;
    };
    let Rectangle { width, height } = video_size(param)?;
    Some((format, width, height))
}

/// The size a fixed video format names: a plain rectangle, or one wrapped in a choice of none, as PipeWire 1.6 hands
/// a negotiated format over (each fixed value a `Choice` of kind `None`). Read only as a plain one, every format
/// negotiated there went unread and the stream dropped each of its buffers: no camera's or shared screen's frame came.
fn video_size(param: &Pod) -> Option<Rectangle> {
    let (_, value) = pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    object
        .properties
        .into_iter()
        .filter(|property| property.key == FormatProperties::VideoSize.as_raw())
        .find_map(|property| match property.value {
            Value::Rectangle(size) | Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::None(size)))) => {
                Some(size)
            }
            _ => None,
        })
}

/// The memory a camera's buffers may come in: a DMA-BUF or a memfd (shared with this process, mapped by the stream's
/// `MAP_BUFFERS`), or plain memory. Without it the link allows every type and a camera that allocates its own buffers
/// (a V4L2 node, `uvcvideo`) maps them as plain memory in the PipeWire daemon, which a client can't be handed: the
/// server refuses the buffers (`mix use buffers failed: -22`) and the stream errs before its first frame. With it,
/// such a camera exports its buffers as DMA-BUFs, as it does for the `pipewiresrc` element and libwebrtc's capturer.
fn camera_buffers() -> Result<Vec<u8>, String> {
    let types: u32 =
        (1 << spa::sys::SPA_DATA_DmaBuf) | (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr);
    let types = i32::try_from(types).map_err(text)?;
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![pod::Property::new(
            spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Flags {
                    default: types,
                    flags: Vec::new(),
                },
            ))),
        )],
    })
}

/// The 16:9 sizes a camera is asked for, the best first: the shape Teams sends and shows a camera in, the size the
/// call sends ([`crate::video::convert::MaxSize::HD`]) first, then those nearest it. A camera lists its own modes,
/// 4:3 often first (a UVC camera's default is 640x480), and PipeWire takes the first of the camera's modes a param
/// matches, so one param of many sizes would get the camera's order, not this one: each size is offered as a param
/// of its own, tried in turn ([`camera_formats`]).
const CAMERA_SIZES: [(u32, u32); 6] = [
    (1280, 720),
    (1024, 576),
    (960, 540),
    (1920, 1080),
    (848, 480),
    (640, 360),
];

/// The frame rate the call sends (`teams_core`'s video settings): asked for, and not exceeded where a camera allows.
const CAMERA_RATE: Fraction = Fraction { num: 30, denom: 1 };

/// The lowest frame rate the first round of offers takes: a camera that gives a size only slower is asked for it in
/// the second round, after every size it gives smoothly.
const SMOOTH_RATE: Fraction = Fraction { num: 24, denom: 1 };

/// How a camera's frames come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    /// Any of [`CAMERA_FORMATS`]: ready to convert, so taken over Motion JPEG at the same size and rate.
    Raw,
    /// Motion JPEG, decoded before it is sent: over USB, the only way most UVC cameras give 720p at 30 frames a
    /// second (raw, a USB 2 link carries it at 10 at most).
    Mjpeg,
}

/// The camera's offers, tried in order: each of [`CAMERA_SIZES`] raw and then as Motion JPEG, first at 24 to 30
/// frames a second, then at any rate; last, for a camera with none of them, any size up to 4K, 1280x720 preferred.
fn camera_formats() -> Result<Vec<Vec<u8>>, String> {
    let any_rate = (Fraction { num: 0, denom: 1 }, Fraction { num: 120, denom: 1 });
    let mut offers = Vec::new();
    for rates in [(SMOOTH_RATE, CAMERA_RATE), any_rate] {
        for (width, height) in CAMERA_SIZES {
            for encoding in [Encoding::Raw, Encoding::Mjpeg] {
                offers.push(camera_format(
                    encoding,
                    Value::Rectangle(Rectangle { width, height }),
                    rates,
                )?);
            }
        }
    }
    let any_size = Value::Choice(ChoiceValue::Rectangle(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Range {
            default: Rectangle {
                width: 1280,
                height: 720,
            },
            min: Rectangle { width: 1, height: 1 },
            max: Rectangle {
                width: 4096,
                height: 4096,
            },
        },
    )));
    for encoding in [Encoding::Raw, Encoding::Mjpeg] {
        offers.push(camera_format(encoding, any_size.clone(), any_rate)?);
    }
    Ok(offers)
}

/// An offer of frames as `encoding` at `size` and a rate in `(min, max)`, [`CAMERA_RATE`] preferred.
fn camera_format(encoding: Encoding, size: Value, (min, max): (Fraction, Fraction)) -> Result<Vec<u8>, String> {
    let subtype = match encoding {
        Encoding::Raw => MediaSubtype::Raw,
        Encoding::Mjpeg => MediaSubtype::Mjpg,
    };
    let mut properties = vec![
        pod::Property::new(
            FormatProperties::MediaType.as_raw(),
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        pod::Property::new(FormatProperties::MediaSubtype.as_raw(), Value::Id(Id(subtype.as_raw()))),
    ];
    if encoding == Encoding::Raw {
        let formats = CAMERA_FORMATS.iter().map(|(video, _)| Id(video.as_raw())).collect();
        properties.push(pod::Property::new(
            FormatProperties::VideoFormat.as_raw(),
            Value::Choice(ChoiceValue::Id(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: Id(CAMERA_FORMATS[0].0.as_raw()),
                    alternatives: formats,
                },
            ))),
        ));
    }
    properties.push(pod::Property::new(FormatProperties::VideoSize.as_raw(), size));
    properties.push(pod::Property::new(
        FormatProperties::VideoFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: CAMERA_RATE,
                min,
                max,
            },
        ))),
    ));
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
}

/// `DRM_FORMAT_MOD_LINEAR`: rows one after another, as the CPU reads them.
const LINEAR: i64 = 0;

/// The packed formats of 4 bytes a pixel a screen's DMA-BUF may come in (a compositor's own: niri draws `BGRx` or
/// `BGRA`), the first preferred.
const SCREEN_DMABUF_FORMATS: [VideoFormat; 4] = [
    VideoFormat::BGRx,
    VideoFormat::BGRA,
    VideoFormat::RGBx,
    VideoFormat::RGBA,
];

/// A shared screen's offers, tried in order: a linear DMA-BUF in one of [`SCREEN_DMABUF_FORMATS`], its modifier
/// mandatory (a compositor that casts only DMA-BUFs takes nothing else), then plain memory in any of
/// [`CAMERA_FORMATS`]; each at 1080p and 15 frames a second preferred, anything up to 8K (a wide or 5K screen's
/// whole picture: the call scales it down).
fn screen_formats() -> Result<Vec<Vec<u8>>, String> {
    let dmabuf = Value::Choice(ChoiceValue::Id(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Enum {
            default: Id(SCREEN_DMABUF_FORMATS[0].as_raw()),
            alternatives: SCREEN_DMABUF_FORMATS.iter().map(|format| Id(format.as_raw())).collect(),
        },
    )));
    let memory = Value::Choice(ChoiceValue::Id(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Enum {
            default: Id(CAMERA_FORMATS[0].0.as_raw()),
            alternatives: CAMERA_FORMATS.iter().map(|(video, _)| Id(video.as_raw())).collect(),
        },
    )));
    let modifier = pod::Property {
        key: FormatProperties::VideoModifier.as_raw(),
        flags: PropertyFlags::MANDATORY,
        value: Value::Choice(ChoiceValue::Long(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: LINEAR,
                alternatives: vec![LINEAR],
            },
        ))),
    };
    Ok(vec![
        screen_format(dmabuf, Some(modifier))?,
        screen_format(memory, None)?,
    ])
}

/// A screen's offer of `formats` (with `modifier` when one is named), at the sizes and rates [`screen_formats`] says.
fn screen_format(formats: Value, modifier: Option<pod::Property>) -> Result<Vec<u8>, String> {
    let mut properties = vec![
        pod::Property::new(
            FormatProperties::MediaType.as_raw(),
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        pod::Property::new(
            FormatProperties::MediaSubtype.as_raw(),
            Value::Id(Id(MediaSubtype::Raw.as_raw())),
        ),
        pod::Property::new(FormatProperties::VideoFormat.as_raw(), formats),
    ];
    properties.extend(modifier);
    properties.push(pod::Property::new(
        FormatProperties::VideoSize.as_raw(),
        Value::Choice(ChoiceValue::Rectangle(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Rectangle {
                    width: 1920,
                    height: 1080,
                },
                min: Rectangle { width: 1, height: 1 },
                max: Rectangle {
                    width: 8192,
                    height: 8192,
                },
            },
        ))),
    ));
    properties.push(pod::Property::new(
        FormatProperties::VideoFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction { num: 15, denom: 1 },
                min: Fraction { num: 0, denom: 1 },
                max: Fraction { num: 120, denom: 1 },
            },
        ))),
    ));
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn to_usize_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::time::Duration;

    use super::*;
    use crate::devices::DeviceError;

    fn dmabuf_test_frame(fd: i32, chunk: &mut spa::sys::spa_chunk) -> Option<VideoFrame> {
        let data = spa::sys::spa_data {
            type_: spa::buffer::DataType::DmaBuf.as_raw(),
            flags: spa::buffer::DataFlags::READABLE.bits(),
            fd: i64::from(fd),
            mapoffset: 0,
            maxsize: 0,
            data: std::ptr::null_mut(),
            chunk,
        };
        // SAFETY: Data is a transparent wrapper of spa_data; its descriptor and chunk live through this call.
        let data = unsafe { &*(&raw const data).cast::<spa::buffer::Data>() };
        dmabuf_frame(data, (PixelFormat::Bgrx, 2, 2))
    }

    #[test]
    fn dma_buf_frames_ignore_chunk_size_and_maxsize_but_keep_the_offset_and_stride() {
        let mut file = tempfile::tempfile().unwrap();
        let bytes: Vec<u8> = (0..32).collect();
        file.write_all(&bytes).unwrap();
        for size in [0, 1, 4096] {
            let mut chunk = spa::sys::spa_chunk {
                offset: 4,
                size,
                stride: 12,
                flags: 0,
            };
            let frame = dmabuf_test_frame(file.as_raw_fd(), &mut chunk).unwrap();
            assert_eq!((frame.width, frame.height, frame.stride), (2, 2, 12));
            assert_eq!(frame.format, PixelFormat::Bgrx);
            assert_eq!(frame.data, bytes[4..28]);
        }
    }

    #[test]
    fn dma_buf_frames_skip_corruption_invalid_descriptors_and_reads_past_the_buffer() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[42; 16]).unwrap();
        let mut chunk = spa::sys::spa_chunk {
            offset: 0,
            size: 0,
            stride: 0,
            flags: 0,
        };
        let frame = dmabuf_test_frame(file.as_raw_fd(), &mut chunk).unwrap();
        assert_eq!(frame.stride, 8, "packed rows when stride is absent");
        assert_eq!(frame.data, [42; 16]);
        chunk.flags = ChunkFlags::CORRUPTED.bits();
        assert!(dmabuf_test_frame(file.as_raw_fd(), &mut chunk).is_none());
        chunk.flags = 0;
        assert!(dmabuf_test_frame(-1, &mut chunk).is_none());
        chunk.offset = 1;
        assert!(dmabuf_test_frame(file.as_raw_fd(), &mut chunk).is_none());
        chunk.offset = 0;
        chunk.stride = 12;
        assert!(dmabuf_test_frame(file.as_raw_fd(), &mut chunk).is_none());
        chunk.stride = 0;
        file.set_len(15).unwrap();
        assert!(dmabuf_test_frame(file.as_raw_fd(), &mut chunk).is_none());
    }

    #[test]
    fn the_camera_takes_dma_bufs_and_memfds_so_a_v4l2_camera_s_buffers_reach_it() {
        let bytes = camera_buffers().expect("the buffers param");
        let (_, value) = pod::deserialize::PodDeserializer::deserialize_any_from(&bytes).expect("a pod");
        let Value::Object(object) = value else {
            panic!("not an object: {value:?}");
        };
        assert_eq!(object.type_, SpaTypes::ObjectParamBuffers.as_raw());
        assert_eq!(object.id, ParamType::Buffers.as_raw());
        let [property] = object.properties.as_slice() else {
            panic!("one property: {:?}", object.properties);
        };
        assert_eq!(property.key, spa::sys::SPA_PARAM_BUFFERS_dataType);
        let Value::Choice(ChoiceValue::Int(Choice(_, ChoiceEnum::Flags { default, .. }))) = property.value else {
            panic!("not a flags choice: {:?}", property.value);
        };
        for wanted in [
            spa::sys::SPA_DATA_DmaBuf,
            spa::sys::SPA_DATA_MemFd,
            spa::sys::SPA_DATA_MemPtr,
        ] {
            assert_ne!(default & (1 << wanted), 0, "data type {wanted}");
        }
    }

    #[test]
    fn a_live_feed_closes_its_queue_when_its_stream_ends_or_goes_and_a_retired_one_never_does() {
        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::live();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        assert!(feed.live().is_some());
        feed.state(&StreamState::Streaming);
        assert!(gate.has_streamed() && !gate.has_ended());
        drop(feed);
        assert!(matches!(queue.recv(Duration::ZERO), Err(DeviceError::Closed)));

        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::live();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        gate.retire();
        assert!(feed.live().is_none(), "a retired stream feeds nothing");
        feed.state(&StreamState::Unconnected);
        drop(feed);
        assert!(gate.has_ended());
        assert_eq!(queue.recv(Duration::ZERO).unwrap(), None, "still open");
    }

    #[test]
    fn a_pending_feed_feeds_nothing_until_it_goes_live() {
        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::pending();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        assert!(feed.live().is_none());
        feed.state(&StreamState::Error("gone".into()));
        assert_eq!(
            queue.recv(Duration::ZERO).unwrap(),
            None,
            "a pending stream closes nothing"
        );
        gate.go_live();
        assert!(feed.live().is_some());
    }

    #[test]
    fn a_speaker_that_isn_t_live_plays_silence_and_leaves_the_queue() {
        let queue = Queue::new(8);
        queue.extend([0.5_f32; 4]).unwrap();
        let mut bytes = [0xff_u8; 16];
        assert_eq!(fill(&mut bytes, 4, None), 4);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(queue.with_items(|items| items.len()), 4);
        assert_eq!(fill(&mut bytes, 0, Some(&queue)), 4);
        assert_eq!(queue.with_items(|items| items.len()), 0);
        assert!((f32::from_le_bytes(bytes[..4].try_into().unwrap()) - 0.5).abs() < f32::EPSILON);
    }

    fn properties(bytes: &[u8]) -> Vec<pod::Property> {
        let (_, value) = pod::deserialize::PodDeserializer::deserialize_any_from(bytes).expect("a pod");
        let Value::Object(object) = value else {
            panic!("not an object: {value:?}");
        };
        object.properties
    }

    fn property(properties: &[pod::Property], key: FormatProperties) -> &Value {
        &properties
            .iter()
            .find(|property| property.key == key.as_raw())
            .unwrap_or_else(|| panic!("no {key:?}"))
            .value
    }

    /// An offer's encoding, size (`None` for a range) and lowest frame rate.
    fn offer(bytes: &[u8]) -> (Encoding, Option<(u32, u32)>, u32) {
        let properties = properties(bytes);
        let encoding = match property(&properties, FormatProperties::MediaSubtype) {
            Value::Id(Id(raw)) if *raw == MediaSubtype::Raw.as_raw() => Encoding::Raw,
            Value::Id(Id(mjpg)) if *mjpg == MediaSubtype::Mjpg.as_raw() => Encoding::Mjpeg,
            other => panic!("another subtype: {other:?}"),
        };
        assert_eq!(
            properties
                .iter()
                .any(|property| property.key == FormatProperties::VideoFormat.as_raw()),
            encoding == Encoding::Raw,
            "only a raw offer names its pixel formats"
        );
        let size = match property(&properties, FormatProperties::VideoSize) {
            Value::Rectangle(size) => Some((size.width, size.height)),
            Value::Choice(ChoiceValue::Rectangle(_)) => None,
            other => panic!("not a size: {other:?}"),
        };
        let Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { default, min, max }))) =
            property(&properties, FormatProperties::VideoFramerate)
        else {
            panic!("not a rate range");
        };
        assert_eq!((default.num, default.denom), (30, 1));
        assert!(max.num >= 30, "{max:?}");
        (encoding, size, min.num)
    }

    #[test]
    fn the_camera_is_asked_for_16_by_9_at_720p_and_30_frames_first_raw_then_motion_jpeg() {
        let offers: Vec<_> = camera_formats().unwrap().iter().map(|bytes| offer(bytes)).collect();
        assert_eq!(offers.len(), CAMERA_SIZES.len() * 4 + 2);
        assert_eq!(offers[0], (Encoding::Raw, Some((1280, 720)), 24));
        assert_eq!(offers[1], (Encoding::Mjpeg, Some((1280, 720)), 24));
        let smooth = &offers[..CAMERA_SIZES.len() * 2];
        assert!(smooth.iter().all(|(_, size, min)| *min == 24 && size.is_some_and(|(w, h)| (w * 9).abs_diff(h * 16) * 100 <= h * 16)));
        let slow = &offers[CAMERA_SIZES.len() * 2..CAMERA_SIZES.len() * 4];
        assert!(slow.iter().all(|(_, size, min)| *min == 0 && size.is_some()));
        assert_eq!(
            offers[offers.len() - 2..],
            [(Encoding::Raw, None, 0), (Encoding::Mjpeg, None, 0)]
        );
    }

    #[test]
    fn a_negotiated_motion_jpeg_or_raw_format_is_read_with_its_size() {
        // PipeWire 1.6 wraps each fixed value of a negotiated format in a choice of none
        let id = |id: u32, wrapped: bool| {
            if wrapped {
                Value::Choice(ChoiceValue::Id(Choice(ChoiceFlags::empty(), ChoiceEnum::None(Id(id)))))
            } else {
                Value::Id(Id(id))
            }
        };
        let fixed_as = |subtype: MediaSubtype, format: Option<VideoFormat>, wrapped: bool| {
            let size = Rectangle {
                width: 1280,
                height: 720,
            };
            let mut properties = vec![
                pod::Property::new(
                    FormatProperties::MediaType.as_raw(),
                    id(MediaType::Video.as_raw(), wrapped),
                ),
                pod::Property::new(FormatProperties::MediaSubtype.as_raw(), id(subtype.as_raw(), wrapped)),
                pod::Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    if wrapped {
                        Value::Choice(ChoiceValue::Rectangle(Choice(
                            ChoiceFlags::empty(),
                            ChoiceEnum::None(size),
                        )))
                    } else {
                        Value::Rectangle(size)
                    },
                ),
            ];
            if let Some(format) = format {
                properties.push(pod::Property::new(
                    FormatProperties::VideoFormat.as_raw(),
                    id(format.as_raw(), wrapped),
                ));
            }
            serialize(pod::Object {
                type_: SpaTypes::ObjectParamFormat.as_raw(),
                id: ParamType::Format.as_raw(),
                properties,
            })
            .unwrap()
        };
        let read = |bytes: &[u8]| video_format(Pod::from_bytes(bytes).expect("a pod"));
        for wrapped in [false, true] {
            let fixed = |subtype, format| fixed_as(subtype, format, wrapped);
            assert_eq!(
                read(&fixed(MediaSubtype::Mjpg, None)),
                Some((PixelFormat::Mjpeg, 1280, 720))
            );
            assert_eq!(
                read(&fixed(MediaSubtype::Raw, Some(VideoFormat::YUY2))),
                Some((PixelFormat::Yuy2, 1280, 720)),
                "wrapped: {wrapped}"
            );
            assert_eq!(
                read(&fixed(MediaSubtype::Raw, Some(VideoFormat::I420))),
                Some((PixelFormat::I420, 1280, 720)),
                "wrapped: {wrapped}"
            );
            assert_eq!(read(&fixed(MediaSubtype::H264, None)), None);
        }
    }

    #[test]
    fn a_screen_is_asked_for_a_linear_dma_buf_first_its_modifier_mandatory_then_plain_memory() {
        let offers = screen_formats().unwrap();
        assert_eq!(offers.len(), 2);
        let dmabuf = properties(&offers[0]);
        let modifier = dmabuf
            .iter()
            .find(|property| property.key == FormatProperties::VideoModifier.as_raw())
            .expect("a modifier");
        assert!(modifier.flags.contains(PropertyFlags::MANDATORY));
        assert!(matches!(
            &modifier.value,
            Value::Choice(ChoiceValue::Long(Choice(_, ChoiceEnum::Enum { default: LINEAR, alternatives })))
                if *alternatives == [LINEAR]
        ));
        let Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::Enum { alternatives, .. }))) =
            property(&dmabuf, FormatProperties::VideoFormat)
        else {
            panic!("no formats");
        };
        assert_eq!(alternatives[0], Id(VideoFormat::BGRx.as_raw()), "niri's own first");
        let packed = [
            PixelFormat::Bgrx,
            PixelFormat::Bgra,
            PixelFormat::Rgbx,
            PixelFormat::Rgba,
        ];
        assert!(alternatives.iter().all(|format| {
            CAMERA_FORMATS
                .iter()
                .any(|(video, pixel)| video.as_raw() == format.0 && packed.contains(pixel))
        }));
        let memory = properties(&offers[1]);
        assert!(
            memory
                .iter()
                .all(|property| property.key != FormatProperties::VideoModifier.as_raw())
        );
        for offer in [&dmabuf, &memory] {
            let Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::Range { max, .. }))) =
                property(offer, FormatProperties::VideoSize)
            else {
                panic!("not a size range");
            };
            assert!(max.width >= 5120 && max.height >= 2880, "a 5K screen: {max:?}");
        }
        assert!(
            offers
                .iter()
                .all(|offer| Pod::from_bytes(offer).is_some_and(names_a_modifier) == (offer == &offers[0]))
        );
    }
}
