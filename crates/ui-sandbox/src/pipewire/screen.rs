//! The sandbox's screen (`sandbox-screen`): `teams-media`'s test pattern at 720p, 15 frames a second, cast the way
//! niri casts a screen, which is what the fake screen-cast portal shares. niri offers only `BGRx` (or `BGRA`) with a
//! mandatory modifier, takes only DMA-BUFs, allocates them itself, doesn't mark them mappable and sets each chunk's
//! size to 1; a client that offers no modifier gets no format at all. This screen does each of those, its DMA-BUFs
//! memfds (no GPU here) that a reader maps as it would a linear DMA-BUF.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::FileExt as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use pipewire::core::CoreRc;
use pipewire::properties::properties;
use pipewire::spa;
use pipewire::spa::buffer::{DataFlags, DataType};
use pipewire::spa::param::ParamType;
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::video::VideoFormat;
use pipewire::spa::pod::{self, ChoiceValue, Property, PropertyFlags, Value};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use teams_media::devices::file::SCREEN;
use teams_media::devices::pattern::TestPattern;

use super::SCREEN_NODE;
use super::media::{ScreenShown, as_pod, serialize, text, to_usize};

/// `DRM_FORMAT_MOD_LINEAR`, the one modifier offered.
const LINEAR: i64 = 0;
/// Bytes a pixel of `BGRx`.
const PIXEL: u32 = 4;
/// Luma's black and white in the pattern (video range), stretched to the full range of a screen's pixels.
const BLACK: i32 = 16;
const WHITE: i32 = 235;

/// The screen's listener unhooks before its stream drops; frames go out on each [`StreamRc::trigger_process`].
pub(super) struct Screen {
    _listener: StreamListener<Showing>,
    pub(super) stream: StreamRc,
}

struct Showing {
    pattern: TestPattern,
    index: Rc<Cell<u32>>,
    frames: Arc<AtomicU64>,
    /// Each buffer's memory, by the descriptor its data names.
    memory: HashMap<i64, File>,
}

/// Starts the screen (see the module docs): its node told on `shown` once the server made it.
pub(super) fn show(core: &CoreRc, shown: ScreenShown) -> Result<Screen, String> {
    let pattern = TestPattern::new(SCREEN.width, SCREEN.height);
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Video",
        *pipewire::keys::MEDIA_CATEGORY => "Source",
        *pipewire::keys::MEDIA_ROLE => "Screen",
        *pipewire::keys::NODE_NAME => SCREEN_NODE,
        *pipewire::keys::NODE_DESCRIPTION => "Sandbox screen",
    };
    let stream = StreamRc::new(core.clone(), SCREEN_NODE, props).map_err(text)?;
    let showing = Showing {
        pattern,
        index: Rc::default(),
        frames: shown.frames,
        memory: HashMap::new(),
    };
    let node: Arc<AtomicU32> = shown.node;
    let listener = stream
        .add_local_listener_with_user_data(showing)
        .param_changed(|stream, _, id, param| {
            if id == ParamType::Format.as_raw() && param.is_some() {
                let _ = buffers().and_then(|params| stream.update_params(&mut [as_pod(&params)?]).map_err(text));
            }
        })
        .state_changed(move |stream, _, _, _| {
            let id = stream.node_id();
            if id != u32::MAX {
                node.store(id, Ordering::Relaxed);
            }
        })
        .add_buffer(|_, showing, buffer| add_buffer(showing, buffer))
        .remove_buffer(|_, showing, buffer| remove_buffer(showing, buffer))
        .process(show_frame)
        .register()
        .map_err(text)?;
    let format = format(pattern)?;
    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::DRIVER | StreamFlags::ALLOC_BUFFERS,
            &mut [as_pod(&format)?],
        )
        .map_err(text)?;
    Ok(Screen {
        _listener: listener,
        stream,
    })
}

/// The first data of `buffer`, if it has one; valid for the callback it was handed to.
fn first_data<'a>(buffer: *mut pipewire::sys::pw_buffer) -> Option<&'a mut spa::sys::spa_data> {
    // SAFETY: PipeWire hands the buffer callbacks a valid `pw_buffer` (or null), alive for the callback.
    let buffer = unsafe { buffer.as_ref() }?;
    // SAFETY: its `spa_buffer`, null or valid while the `pw_buffer` is.
    let spa_buffer = unsafe { buffer.buffer.as_ref() }?;
    if spa_buffer.n_datas == 0 {
        return None;
    }
    // SAFETY: the array of `n_datas` datas (at least one) PipeWire made for the buffer, or null.
    unsafe { spa_buffer.datas.as_mut() }
}

/// A new buffer's memory, as niri's DMA-BUFs: its own descriptor, not mappable, a size PipeWire is told nothing of.
fn add_buffer(showing: &mut Showing, buffer: *mut pipewire::sys::pw_buffer) {
    let Some(data) = first_data(buffer) else {
        return;
    };
    let size = u64::from(showing.pattern.width * PIXEL * showing.pattern.height);
    let Some(memory) = memfd().filter(|memory| memory.set_len(size).is_ok()) else {
        return;
    };
    let fd = i64::from(memory.as_raw_fd());
    // the descriptor stays open in `memory` until the buffer is removed
    data.type_ = DataType::DmaBuf.as_raw();
    data.flags = DataFlags::READWRITE.bits();
    data.fd = fd;
    data.mapoffset = 0;
    data.maxsize = 1;
    data.data = std::ptr::null_mut();
    showing.memory.insert(fd, memory);
}

fn remove_buffer(showing: &mut Showing, buffer: *mut pipewire::sys::pw_buffer) {
    if let Some(data) = first_data(buffer) {
        showing.memory.remove(&data.fd);
    }
}

/// An empty memfd, closed on exec.
fn memfd() -> Option<File> {
    // SAFETY: a fresh descriptor the call returns (or -1), owned from here on.
    let fd = unsafe { libc::memfd_create(c"sandbox-screen".as_ptr(), libc::MFD_CLOEXEC) };
    // SAFETY: `fd` is the new descriptor nothing else owns.
    (fd >= 0).then(|| File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Writes the next frame of the pattern into the stream's next buffer, as `BGRx`, its chunk's size 1 as niri's.
fn show_frame(stream: &pipewire::stream::Stream, showing: &mut Showing) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let Some(memory) = showing.memory.get(&i64::from(data.fd())) else {
        return;
    };
    let frame = to_bgrx(&showing.pattern.frame(showing.index.get()).data, showing.pattern);
    if memory.write_all_at(&frame, 0).is_err() {
        return;
    }
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = i32::try_from(showing.pattern.width * PIXEL).unwrap_or(0);
    *chunk.size_mut() = 1;
    showing.index.set(showing.index.get().wrapping_add(1));
    showing.frames.fetch_add(1, Ordering::Relaxed);
}

/// The pattern's I420 picture as `BGRx` (its chroma is neutral: each pixel the grey of its luma).
fn to_bgrx(i420: &[u8], pattern: TestPattern) -> Vec<u8> {
    let pixels = to_usize(pattern.width) * to_usize(pattern.height);
    let mut bgrx = Vec::with_capacity(pixels * 4);
    for &luma in i420.iter().take(pixels) {
        let grey = ((i32::from(luma) - BLACK) * 255 / (WHITE - BLACK)).clamp(0, 255);
        let grey = u8::try_from(grey).unwrap_or(u8::MAX);
        bgrx.extend_from_slice(&[grey, grey, grey, u8::MAX]);
    }
    bgrx
}

/// The screen's one format, as niri offers it: `BGRx` with the linear modifier, mandatory, at the pattern's size.
fn format(pattern: TestPattern) -> Result<Vec<u8>, String> {
    let modifier = Property {
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
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Id(Id(VideoFormat::BGRx.as_raw())),
            ),
            modifier,
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Rectangle(Rectangle {
                    width: pattern.width,
                    height: pattern.height,
                }),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Fraction(Fraction {
                    num: SCREEN.fps,
                    denom: 1,
                }),
            ),
        ],
    })
}

/// The buffers niri takes: DMA-BUFs only, one block each.
fn buffers() -> Result<Vec<u8>, String> {
    let dmabuf = 1_i32 << DataType::DmaBuf.as_raw();
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, Value::Int(4)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(
                spa::sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: dmabuf,
                        flags: vec![dmabuf],
                    },
                ))),
            ),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_s_greys_span_a_screen_s_full_range() {
        let pattern = TestPattern::new(2, 2);
        let pixels = to_bgrx(&[16, 235, 64, 255, 128, 128], pattern);
        assert_eq!(pixels.len(), 16);
        let greys: Vec<u8> = pixels.chunks(4).map(|pixel| pixel[1]).collect();
        assert_eq!(greys, [0, 255, 55, 255]);
        assert!(
            pixels
                .chunks(4)
                .all(|pixel| pixel[0] == pixel[1] && pixel[2] == pixel[1] && pixel[3] == 255)
        );
    }
}
