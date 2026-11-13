//! The sandbox's camera and the speaker's recording: two streams on a thread of the process that owns the sandbox.
//!
//! - **The camera** (`sandbox-camera`, a video source): `teams-media`'s test pattern ([`TestPattern`]), I420 at the
//!   size and rate asked, a frame on each tick of its own timer (it drives its graph, as a camera does), each frame
//!   carrying its index.
//! - **The screen** (`sandbox-screen`, [`super::screen`]): the same pattern at 720p, 15 frames a second, cast as niri
//!   casts a screen (or in plain memory, [`super::ScreenCast::Memory`]; a stream of the kind a compositor's screen
//!   cast is, `Stream/Output/Video`, so no camera list names it); its node is what the sandbox's fake screen-cast portal hands out ([`MediaHelper::screen_node`]).
//! - **The recording**: a capture of the speaker's monitor, written to a WAV file whose header is brought up to date
//!   every second, each buffer marked on its [`Timeline`] (when its first sample was heard).
//! - **Devices added and removed while it runs** ([`super::nodes`]): another microphone playing a tone of its own, or
//!   another speaker recorded to a file of its own, as a device plugged in and out.
//!
//! The thread connects to the server's socket itself and hands libpipewire the descriptor, with the sandbox's own
//! client config named outright, so nothing of this process's session (its runtime dir, its config dirs) is read.
//! The thread runs in this process, with its environment: libpipewire reads `PIPEWIRE_CONFIG_NAME` and
//! `PIPEWIRE_CONFIG_PREFIX` ahead of the config a context names, so a process that sets either is refused
//! ([`check_environment`]) before anything starts.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Cursor;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::properties;
use pipewire::spa;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::video::VideoFormat;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, Pod, Property, Value};
use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use teams_media::devices::file::{PatternCamera, SCREEN};
use teams_media::devices::pattern::TestPattern;
use teams_media::devices::pipewire::CONFIG_OVERRIDES;
use teams_media::devices::wav::WavWriter;

use super::timeline::{Timeline, now_us, samples_to_us};
use super::{CAMERA_NODE, SPEAKER_NODE, ScreenCast};
use crate::{SOCKET_WAIT, SandboxResult};

/// The camera and the recording, stopped (and the recording's header brought up to date) on drop.
pub struct MediaHelper {
    commands: pipewire::channel::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    frames: Arc<AtomicU64>,
    screen: ScreenShown,
    recorded: Timeline,
}

/// The sandbox's screen as it is shown: its node once the server made it (`0` before), and its frames so far.
#[derive(Clone, Default)]
pub struct ScreenShown {
    pub node: Arc<AtomicU32>,
    pub frames: Arc<AtomicU64>,
}

/// What the helper's thread is asked.
pub(super) enum Command {
    /// Adds a node (`super::nodes`); answers once the node is there.
    Add(super::nodes::Added, mpsc::Sender<Result<(), String>>),
    /// Removes the node of this name; answers once it is gone.
    Remove(String, mpsc::Sender<()>),
    Quit,
}

impl Drop for MediaHelper {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl MediaHelper {
    /// Connects to `socket` with the client config `client_conf`, shows the camera and records the speaker to
    /// `recording`; returns once the session manager has linked the recording.
    ///
    /// # Errors
    /// This process sets a config override ([`check_environment`]), the socket can't be reached, a stream can't be
    /// made, or the recording isn't linked within [`SOCKET_WAIT`].
    pub fn start(
        socket: &Path,
        client_conf: &Path,
        recording: &Path,
        camera: PatternCamera,
        cast: ScreenCast,
    ) -> SandboxResult<Self> {
        check_environment()?;
        let stream = UnixStream::connect(socket)?;
        let (commands, commands_rx) = pipewire::channel::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let frames = Arc::new(AtomicU64::new(0));
        let screen = ScreenShown::default();
        let recorded = Timeline::new();
        let setup = Setup {
            recorded: recorded.clone(),
            stream,
            client_conf: client_conf.to_owned(),
            recording: recording.to_owned(),
            camera,
            frames: Arc::clone(&frames),
            screen: screen.clone(),
            cast,
        };
        let thread = thread::Builder::new()
            .name("ui-sandbox-media".to_owned())
            .spawn(move || {
                if let Err(error) = setup.run(commands_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        let helper = Self {
            commands,
            thread: Some(thread),
            frames,
            screen,
            recorded,
        };
        match ready_rx.recv_timeout(SOCKET_WAIT) {
            Ok(Ok(())) => Ok(helper),
            Ok(Err(error)) => Err(format!("the sandbox's camera and recording: {error}").into()),
            Err(_) => Err("the session manager didn't link the speaker's recording in time".into()),
        }
    }

    /// Frames the camera has shown so far.
    #[must_use]
    pub fn frames_shown(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The sandbox's screen: its node (once made) and its frames, shared with the fake screen-cast portal.
    #[must_use]
    pub fn screen(&self) -> ScreenShown {
        self.screen.clone()
    }

    /// The sandbox's screen's node, once the server made it.
    #[must_use]
    pub fn screen_node(&self) -> Option<u32> {
        Some(self.screen.node.load(Ordering::Relaxed)).filter(|node| *node != 0)
    }

    /// When the recording's samples were heard.
    #[must_use]
    pub fn recording_timeline(&self) -> Timeline {
        self.recorded.clone()
    }

    /// The way to the helper's thread, for the nodes it adds.
    pub(super) fn commands(&self) -> pipewire::channel::Sender<Command> {
        self.commands.clone()
    }
}

/// Refuses this process when its environment sets one of libpipewire's config overrides ([`CONFIG_OVERRIDES`]): the
/// helper's thread would load that config (with its modules and D-Bus support) instead of the sandbox's.
///
/// # Errors
/// The override set, by name.
pub fn check_environment() -> SandboxResult {
    config_override(|name| std::env::var_os(name)).map_or(Ok(()), |name| {
        Err(format!(
            "{name} is set in this process: the sandbox's PipeWire clients in it would load that config instead of the \
             sandbox's; unset it"
        )
        .into())
    })
}

/// The first of [`CONFIG_OVERRIDES`] `var` has at all (even empty: libpipewire reads it as it is).
fn config_override(var: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    CONFIG_OVERRIDES.into_iter().find(|name| var(name).is_some())
}

struct Setup {
    recorded: Timeline,
    stream: UnixStream,
    client_conf: PathBuf,
    recording: PathBuf,
    camera: PatternCamera,
    frames: Arc<AtomicU64>,
    screen: ScreenShown,
    cast: ScreenCast,
}

pub(super) fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Setup {
    fn run(
        self,
        commands: pipewire::channel::Receiver<Command>,
        ready: &mpsc::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        // libwebrtc's stubs of libpipewire's functions stand in for its own in this binary: loaded first
        if !teams_media::rtc::load_pipewire() {
            log::warn!("libwebrtc couldn't load libpipewire into its stubs");
        }
        pipewire::init();
        let main_loop = MainLoopRc::new(None).map_err(text)?;
        let conf = self
            .client_conf
            .to_str()
            .ok_or("the client config's path isn't UTF-8")?;
        let context =
            ContextRc::new(&main_loop, Some(properties! { *pipewire::keys::CONFIG_NAME => conf })).map_err(text)?;
        let core = context.connect_fd_rc(self.stream.into(), None).map_err(text)?;
        let _recorder = recorder(&core, &self.recording, self.recorded, ready.clone())?;
        let camera = Camera::show(&core, self.camera, self.frames)?;
        let ticks = camera.stream.clone();
        let timer = main_loop.loop_().add_timer(move |_| {
            let _ = ticks.trigger_process();
        });
        let period = Duration::from_secs(1) / self.camera.fps.max(1);
        timer
            .update_timer(Some(period), Some(period))
            .into_sync_result()
            .map_err(text)?;
        let screen = super::screen::show(&core, self.screen, self.cast)?;
        let screen_ticks = screen.stream.clone();
        let screen_timer = main_loop.loop_().add_timer(move |_| {
            let _ = screen_ticks.trigger_process();
        });
        let period = Duration::from_secs(1) / SCREEN.fps;
        screen_timer
            .update_timer(Some(period), Some(period))
            .into_sync_result()
            .map_err(text)?;
        let stop = main_loop.downgrade();
        let added: Rc<RefCell<HashMap<String, Box<dyn Any>>>> = Rc::default();
        let adding = Rc::clone(&added);
        let _commands = commands.attach(main_loop.loop_(), move |command| match command {
            Command::Add(node, done) => {
                let name = node.name().to_owned();
                let _ = done.send(super::nodes::add(&core, node).map(|open| {
                    adding.borrow_mut().insert(name, open);
                }));
            }
            Command::Remove(name, done) => {
                let removed = adding.borrow_mut().remove(&name);
                drop(removed);
                let _ = done.send(());
            }
            Command::Quit => {
                if let Some(main_loop) = stop.upgrade() {
                    main_loop.quit();
                }
            }
        });
        main_loop.run();
        added.borrow_mut().clear();
        Ok(())
    }
}

pub(super) fn serialize(object: pod::Object) -> Result<Vec<u8>, String> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("{error:?}"))
}

pub(super) fn as_pod(bytes: &[u8]) -> Result<&Pod, String> {
    Pod::from_bytes(bytes).ok_or_else(|| "a parameter PipeWire can't read".to_owned())
}

/// A stream and its listener; the listener goes first (it unhooks itself from the stream).
pub(super) struct Open<D: 'static> {
    pub(super) _listener: StreamListener<D>,
    pub(super) _stream: StreamRc,
}

/// F32 mono at 48 kHz: what the recording and the nodes added take and give.
pub(super) fn audio_format() -> Result<Vec<u8>, String> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(teams_media::devices::SAMPLE_RATE);
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

struct Recording {
    writer: Option<WavWriter>,
    ready: Option<mpsc::Sender<Result<(), String>>>,
    /// Each buffer written, marked when its first sample was heard; `written` samples so far.
    timeline: Timeline,
    written: u64,
}

fn recorder(
    core: &CoreRc,
    path: &Path,
    timeline: Timeline,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<Open<Recording>, String> {
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Production",
        *pipewire::keys::NODE_NAME => "sandbox-recorder",
        *pipewire::keys::TARGET_OBJECT => SPEAKER_NODE,
        *pipewire::keys::STREAM_CAPTURE_SINK => "true",
        // the session manager moves a stream linked to the default it named to every new default (PulseAudio's
        // rule): the recording stays on the sandbox's speaker when a speaker is plugged in above it
        *pipewire::keys::NODE_DONT_RECONNECT => "true",
    };
    let stream = StreamRc::new(core.clone(), "sandbox-recorder", props).map_err(text)?;
    let state = Recording {
        writer: Some(WavWriter::create(path).map_err(text)?),
        ready: Some(ready),
        timeline,
        written: 0,
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(|_, state, _, new| {
            if matches!(new, StreamState::Streaming)
                && let Some(ready) = state.ready.take()
            {
                let _ = ready.send(Ok(()));
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (to_usize(data.chunk().offset()), to_usize(data.chunk().size()));
            let bytes = data.data().unwrap_or_default();
            let end = offset.saturating_add(size).min(bytes.len());
            let (samples, _) = bytes.get(offset..end).unwrap_or_default().as_chunks::<4>();
            let samples: Vec<f32> = samples.iter().map(|sample| f32::from_le_bytes(*sample)).collect();
            let count = u64::try_from(samples.len()).unwrap_or(0);
            if let Some(writer) = &mut state.writer {
                if writer.write(&samples).is_err() {
                    state.writer = None;
                    return;
                }
                state.timeline.mark(state.written, now_us() - samples_to_us(count));
                state.written += count;
            }
        })
        .register()
        .map_err(text)?;
    let format = audio_format()?;
    stream
        .connect(
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [as_pod(&format)?],
        )
        .map_err(text)?;
    Ok(Open {
        _listener: listener,
        _stream: stream,
    })
}

struct Camera {
    stream: StreamRc,
    _open: Open<Showing>,
}

struct Showing {
    pattern: TestPattern,
    index: Rc<Cell<u32>>,
    frames: Arc<AtomicU64>,
}

impl Camera {
    fn show(core: &CoreRc, camera: PatternCamera, frames: Arc<AtomicU64>) -> Result<Self, String> {
        let pattern = TestPattern::new(camera.width, camera.height);
        let props = properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Source",
            *pipewire::keys::MEDIA_ROLE => "Camera",
            *pipewire::keys::MEDIA_CLASS => "Video/Source",
            *pipewire::keys::NODE_NAME => CAMERA_NODE,
            *pipewire::keys::NODE_DESCRIPTION => "Sandbox camera",
            *pipewire::keys::PRIORITY_SESSION => super::DEVICE_PRIORITY.to_string().as_str(),
        };
        let stream = StreamRc::new(core.clone(), CAMERA_NODE, props).map_err(text)?;
        let showing = Showing {
            pattern,
            index: Rc::default(),
            frames,
        };
        let listener = stream
            .add_local_listener_with_user_data(showing)
            .param_changed(|stream, showing, id, param| {
                if id == ParamType::Format.as_raw() && param.is_some() {
                    let _ = buffers(showing.pattern)
                        .and_then(|params| stream.update_params(&mut [as_pod(&params)?]).map_err(text));
                }
            })
            .process(show_frame)
            .register()
            .map_err(text)?;
        let format = camera_format(pattern, camera.fps)?;
        stream
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
                &mut [as_pod(&format)?],
            )
            .map_err(text)?;
        Ok(Self {
            stream: stream.clone(),
            _open: Open {
                _listener: listener,
                _stream: stream,
            },
        })
    }
}

/// Writes the next frame of the pattern into the stream's next buffer.
fn show_frame(stream: &pipewire::stream::Stream, showing: &mut Showing) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let frame = showing.pattern.frame(showing.index.get());
    let Some(bytes) = data.data() else {
        return;
    };
    let size = frame.data.len().min(bytes.len());
    bytes[..size].copy_from_slice(&frame.data[..size]);
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = i32::try_from(frame.stride).unwrap_or(0);
    *chunk.size_mut() = u32::try_from(size).unwrap_or(0);
    showing.index.set(showing.index.get().wrapping_add(1));
    showing.frames.fetch_add(1, Ordering::Relaxed);
}

/// The camera's one format: I420 at the pattern's size and `fps`.
fn camera_format(pattern: TestPattern, fps: u32) -> Result<Vec<u8>, String> {
    serialize(pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::I420),
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle {
                width: pattern.width,
                height: pattern.height
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction {
                num: fps.max(1),
                denom: 1
            }
        ),
    ))
}

/// The buffers the camera's frames need: one block of a whole I420 frame each.
fn buffers(pattern: TestPattern) -> Result<Vec<u8>, String> {
    let size = pattern.width * pattern.height * 3 / 2;
    let int = |value: u32| Value::Int(i32::try_from(value).unwrap_or(i32::MAX));
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, int(4)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, int(1)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_size, int(size)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_stride, int(pattern.width)),
        ],
    })
}

pub(super) fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_config_override_is_named_even_empty() {
        assert_eq!(config_override(|_| None), None);
        for name in CONFIG_OVERRIDES {
            for value in ["client.conf", ""] {
                let found = config_override(|asked| (asked == name).then(|| value.into()));
                assert_eq!(found, Some(name), "{name}={value:?}");
            }
        }
        assert!(
            CONFIG_OVERRIDES.contains(&"PIPEWIRE_CONFIG_NAME") && CONFIG_OVERRIDES.contains(&"PIPEWIRE_CONFIG_PREFIX")
        );
    }
}
