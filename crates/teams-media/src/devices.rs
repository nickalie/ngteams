//! The device seam: where a call's audio and video come from and go to.
//!
//! A call's media session talks to [`Devices`] only: a microphone and a speaker of 10 ms mono frames at 48 kHz
//! ([`AudioFrame`]), a camera of frames ([`VideoFrame`]), and somewhere received frames go ([`VideoSink`]). Two
//! production backends and a file backend stand behind it:
//!
//! - [`file`]: no audio server at all. A tone or a WAV file in, a WAV recording out, a generated test pattern as the
//!   camera ([`pattern`]), received frames counted. Tests and mock calls use it.
//! - [`pipewire`]: one PipeWire server for audio and the camera, selected by the caller at run time
//!   ([`pipewire::Reach`]): the UI sandbox's private one, or the desktop's, whose camera comes through the desktop's
//!   camera portal ([`portal`]: a PipeWire remote showing only the cameras). It lists the devices there are and
//!   follows the user's choice of each kind ([`catalog`]).
//! - Windows: native WASAPI audio and Media Foundation cameras through `GStreamer`, with live device switching and
//!   the system's Windows Graphics Capture picker for screen/window sharing.
//!
//! Every device hands frames over on threads of its own: a source is read with a timeout and a sink takes a frame and
//! returns at once, so the caller's thread never waits on an audio server.

use std::io;
use std::time::Duration;

pub mod catalog;
pub mod file;
#[cfg(any(windows, target_os = "macos", test))]
mod native;
pub mod pattern;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(windows)]
pub use native::WindowsDevices;

#[cfg(windows)]
pub use native::WindowsDevices as SystemDevices;
/// macOS's devices: the same `GStreamer` backend as Windows, over Core Audio and `AVFoundation`.
#[cfg(target_os = "macos")]
pub use native::WindowsDevices as SystemDevices;
/// The native device backend selected at compile time; call sites share the same reach and device contracts.
#[cfg(target_os = "linux")]
pub use pipewire::{PipeWireDevices as SystemDevices, Reach};
#[cfg(not(target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Only the explicitly authorized VM console (Windows); refused on macOS, which has no device sandbox.
    Sandbox,
    /// The current user's desktop.
    Desktop,
}
pub mod wav;

/// The sample rate of every audio frame, in Hz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples in one audio frame: 10 ms at [`SAMPLE_RATE`], mono.
pub const FRAME_SAMPLES: usize = 480;
/// How long one audio frame lasts.
pub const FRAME_DURATION: Duration = Duration::from_millis(10);

/// 10 ms of mono audio at 48 kHz, samples in `[-1, 1]`.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFrame {
    samples: [f32; FRAME_SAMPLES],
}

impl AudioFrame {
    /// A frame of silence.
    #[must_use]
    pub fn silence() -> Self {
        Self {
            samples: [0.0; FRAME_SAMPLES],
        }
    }

    /// A frame of exactly [`FRAME_SAMPLES`] samples, or `None` for any other length.
    #[must_use]
    pub fn from_samples(samples: &[f32]) -> Option<Self> {
        Some(Self {
            samples: samples.try_into().ok()?,
        })
    }

    #[must_use]
    pub fn samples(&self) -> &[f32; FRAME_SAMPLES] {
        &self.samples
    }

    pub fn samples_mut(&mut self) -> &mut [f32; FRAME_SAMPLES] {
        &mut self.samples
    }
}

/// How a video frame's bytes are laid out: the raw formats a camera offers that the media takes, or one Motion JPEG
/// picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Planar 4:2:0: the Y plane (`stride` bytes a row), then U and V at half width and height.
    I420,
    /// Y plane, then one plane of interleaved U and V at half height.
    Nv12,
    /// Packed 4:2:2: Y0 U Y1 V.
    Yuy2,
    Rgbx,
    Bgrx,
    Rgba,
    Bgra,
    Rgb,
    Bgr,
    /// Motion JPEG: `data` is one JPEG picture, as a UVC camera gives it in its compressed mode, and `stride` says
    /// nothing; [`crate::video::convert::to_i420`] decodes it.
    Mjpeg,
}

impl PixelFormat {
    /// Its name, as a log line or a probe's output gives it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::I420 => "I420",
            Self::Nv12 => "NV12",
            Self::Yuy2 => "YUY2",
            Self::Rgbx => "RGBx",
            Self::Bgrx => "BGRx",
            Self::Rgba => "RGBA",
            Self::Bgra => "BGRA",
            Self::Rgb => "RGB",
            Self::Bgr => "BGR",
            Self::Mjpeg => "MJPG",
        }
    }
}

/// One video frame: `stride` bytes per row of its first plane, the planes one after another in `data` (for
/// [`PixelFormat::Mjpeg`], the picture's bytes instead).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub stride: u32,
    pub data: Vec<u8>,
}

/// Why a device can't be opened or used.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The selected reach may not open what was asked: the reason, as the message to print.
    #[error("refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A WAV file this backend can't read.
    #[error("WAV: {0}")]
    Wav(String),
    /// A sound file that can't be read ([`crate::sound_file`]).
    #[error("sound file: {0}")]
    SoundFile(String),
    /// A native capture/playback pipeline failed.
    #[error("media device: {0}")]
    Native(String),
    #[error("PipeWire: {0}")]
    PipeWire(String),
    /// The desktop's camera portal (or the bus it is on) failed or didn't answer.
    #[error("the camera portal: {0}")]
    Portal(String),
    /// The device or its server went away.
    #[error("the device was closed")]
    Closed,
}

/// Where audio comes from: the next 10 ms frame, within `timeout`.
pub trait AudioSource: Send {
    /// The next frame, or `None` when none came within `timeout` (the device is late or not linked yet).
    ///
    /// # Errors
    /// The device went away ([`DeviceError::Closed`]) or can't be read.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError>;
}

/// Where audio goes: takes a 10 ms frame and returns at once (a full queue drops its oldest audio).
pub trait AudioSink: Send {
    /// Queues `frame` to be played.
    ///
    /// # Errors
    /// The device went away or can't be written.
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError>;
}

/// Where video comes from: the next frame, within `timeout`.
pub trait VideoSource: Send {
    /// The next frame, or `None` when none came within `timeout`.
    ///
    /// # Errors
    /// The device went away or can't be read.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError>;
}

/// Where received video goes.
pub trait VideoSink: Send {
    /// Takes one received frame.
    ///
    /// # Errors
    /// The sink can't take it.
    fn show(&mut self, frame: &VideoFrame) -> Result<(), DeviceError>;
}

/// A backend: opens the microphone, the speaker and the camera, and the screen the user shares.
pub trait Devices {
    /// # Errors
    /// The microphone can't be opened.
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError>;

    /// # Errors
    /// The speaker can't be opened.
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError>;

    /// # Errors
    /// The camera can't be opened.
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError>;

    /// The screen (or window) the user picks to share, as the backend asks them; none by default.
    ///
    /// # Errors
    /// The backend shares no screen, the user picked none, or it can't be opened.
    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        Err(DeviceError::Refused("no screen to share here".to_owned()))
    }
}
