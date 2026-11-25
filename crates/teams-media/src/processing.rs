//! The microphone's audio processing: echo cancellation, noise suppression and gain control, on the device seam's
//! 10 ms mono frames at 48 kHz. libwebrtc's audio processing module runs echo cancellation (AEC3) and gain control
//! (AGC2); the noise suppression runs between them, at the level of the call's [`NoiseControl`] ([`noise`]):
//! DeepFilterNet3 ([`deep`]) at Auto and High, libwebrtc's classic suppressor at Low, none at Off.
//!
//! Two sides feed one [`AudioProcessing`]: what the speaker plays is the far end ([`AudioProcessing::far_end`],
//! analysed, never changed), and what the microphone hears is the near end ([`AudioProcessing::near_end`], changed in
//! place: the far end's echo taken out, the noise lowered, the level brought up or down). The two sides run on
//! different threads (the session's playout and capture threads), so one [`AudioProcessing`] is shared between them
//! behind an [`Arc`](std::sync::Arc), the modules behind locks. The echo path's delay is the module's own estimate:
//! the session never tells it one.
//!
//! The gain control is a module of its own after the noise suppression, so it levels the cleaned voice, not the
//! noise. libwebrtc's module takes its parts only when it is made: a change to or from Low makes the echo
//! canceller's module again, which then converges again within about a second.
//!
//! The session runs it itself rather than leaving it to libwebrtc's audio source: the far end is played by the device
//! seam, not by a libwebrtc audio device, so only the session can hand the module what the speaker plays.

use std::sync::{Mutex, PoisonError};

use libwebrtc::native::apm::AudioProcessingModule;

use crate::Error;
use crate::devices::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE};

pub mod deep;
pub mod noise;

pub use noise::{NoiseControl, NoiseSuppression};

/// Which parts of the processing run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessingConfig {
    /// AEC3, with the high-pass filter it needs.
    pub echo_cancellation: bool,
    /// Background noise lowered, at the level of the call's [`NoiseControl`].
    pub noise_suppression: bool,
    /// The adaptive digital gain controller (AGC2) and its limiter.
    pub gain_control: bool,
}

impl ProcessingConfig {
    /// What a call runs: all three.
    pub const CALL: Self = Self {
        echo_cancellation: true,
        noise_suppression: true,
        gain_control: true,
    };

    /// Nothing: the microphone's frames pass unchanged.
    pub const OFF: Self = Self {
        echo_cancellation: false,
        noise_suppression: false,
        gain_control: false,
    };

    /// Whether any part runs.
    #[must_use]
    pub fn any(self) -> bool {
        self.echo_cancellation || self.noise_suppression || self.gain_control
    }
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self::CALL
    }
}

/// libwebrtc's module of the echo canceller, with the classic suppressor or without.
struct EchoModule {
    module: AudioProcessingModule,
    classic: bool,
}

impl EchoModule {
    fn new(echo: bool, classic: bool) -> Self {
        Self {
            module: AudioProcessingModule::new(echo, false, echo || classic, classic),
            classic,
        }
    }
}

/// One call's audio processing: the far end it hears played, the near end it cleans.
pub struct AudioProcessing {
    /// The echo canceller and the classic suppressor; `None` when neither can run.
    echo: Option<Mutex<EchoModule>>,
    /// DeepFilterNet's state for this call, while a level runs it (the capture thread's alone).
    deep: Mutex<Option<deep::DeepFilter>>,
    /// The gain control after the noise suppression; `None` when it doesn't run.
    gain: Option<Mutex<AudioProcessingModule>>,
    config: ProcessingConfig,
    noise: NoiseControl,
}

impl std::fmt::Debug for AudioProcessing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioProcessing")
            .field("config", &self.config)
            .field("noise", &self.noise.level())
            .finish_non_exhaustive()
    }
}

impl AudioProcessing {
    /// A processing running `config` on frames at [`SAMPLE_RATE`], its noise suppression at [`NoiseSuppression::Auto`].
    ///
    /// # Errors
    /// Never today: kept for a module that can refuse its setup.
    pub fn new(config: ProcessingConfig) -> Result<Self, Error> {
        Self::with_noise(config, NoiseControl::default())
    }

    /// A processing running `config`, its noise suppression at the level `noise` holds at each frame. With noise
    /// suppression in `config`, DeepFilterNet's model starts to load.
    ///
    /// # Errors
    /// Never today: kept for a module that can refuse its setup.
    pub fn with_noise(config: ProcessingConfig, noise: NoiseControl) -> Result<Self, Error> {
        let classic = config.noise_suppression && noise.level().classic();
        let echo = (config.echo_cancellation || config.noise_suppression)
            .then(|| Mutex::new(EchoModule::new(config.echo_cancellation, classic)));
        let gain = config
            .gain_control
            .then(|| Mutex::new(AudioProcessingModule::new(false, true, false, false)));
        if config.noise_suppression {
            deep::prepare();
        }
        Ok(Self {
            echo,
            deep: Mutex::new(None),
            gain,
            config,
            noise,
        })
    }

    #[must_use]
    pub fn config(&self) -> ProcessingConfig {
        self.config
    }

    /// The level the noise suppression runs at now: [`NoiseSuppression::Off`] when the config has none.
    #[must_use]
    pub fn noise_level(&self) -> NoiseSuppression {
        if self.config.noise_suppression {
            self.noise.level()
        } else {
            NoiseSuppression::Off
        }
    }

    /// Hands the module a frame the speaker plays (the far end, whose echo the microphone may hear), unchanged.
    ///
    /// # Errors
    /// [`Error::Processing`] when the module refuses the frame.
    pub fn far_end(&self, played: &AudioFrame) -> Result<(), Error> {
        let Some(echo) = &self.echo else {
            return Ok(());
        };
        let mut samples = to_i16(played);
        echo.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .module
            .process_reverse_stream(&mut samples, rate(), 1)
            .map_err(|error| Error::Processing(error.message))
    }

    /// Processes a frame the microphone heard, in place.
    ///
    /// # Errors
    /// [`Error::Processing`] when a module refuses the frame.
    pub fn near_end(&self, captured: &mut AudioFrame) -> Result<(), Error> {
        if !self.config.any() {
            // nothing runs: the frame stays exactly as it was (not even through 16-bit samples)
            return Ok(());
        }
        let level = self.noise_level();
        if let Some(echo) = &self.echo {
            let mut echo = echo.lock().unwrap_or_else(PoisonError::into_inner);
            if echo.classic != level.classic() {
                log::info!("the call's noise suppression is now {}", level.key());
                *echo = EchoModule::new(self.config.echo_cancellation, level.classic());
            }
            run(&mut echo.module, captured)?;
        }
        self.deep_filter(level, captured)?;
        if let Some(gain) = &self.gain {
            run(&mut gain.lock().unwrap_or_else(PoisonError::into_inner), captured)?;
        }
        Ok(())
    }

    /// DeepFilterNet on `frame` at `level`'s limit; a level without it drops the call's state, so the next one starts
    /// fresh.
    fn deep_filter(&self, level: NoiseSuppression, frame: &mut AudioFrame) -> Result<(), Error> {
        let mut deep = self.deep.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(limit) = level.deep_filter_limit() else {
            *deep = None;
            return Ok(());
        };
        if deep.is_none() {
            *deep = deep::DeepFilter::ready(limit);
        }
        let Some(filter) = deep.as_mut() else {
            // the model still loads (or can't): the frame passes
            return Ok(());
        };
        filter.set_limit(limit);
        filter.process(frame).map_err(Error::Processing)
    }
}

/// `frame` through `module`'s near-end processing, in place, as 16-bit samples.
fn run(module: &mut AudioProcessingModule, frame: &mut AudioFrame) -> Result<(), Error> {
    let mut samples = to_i16(frame);
    module
        .process_stream(&mut samples, rate(), 1)
        .map_err(|error| Error::Processing(error.message))?;
    for (out, sample) in frame.samples_mut().iter_mut().zip(samples) {
        *out = from_i16(sample);
    }
    Ok(())
}

fn rate() -> i32 {
    i32::try_from(SAMPLE_RATE).unwrap_or(i32::MAX)
}

/// A frame's samples as 16-bit PCM, what libwebrtc takes.
#[must_use]
pub fn to_i16(frame: &AudioFrame) -> [i16; FRAME_SAMPLES] {
    let mut out = [0; FRAME_SAMPLES];
    for (out, sample) in out.iter_mut().zip(frame.samples()) {
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the 16-bit range first")]
        let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        *out = scaled;
    }
    out
}

/// A 16-bit PCM sample as the device seam's `[-1, 1]`.
#[must_use]
pub fn from_i16(sample: i16) -> f32 {
    f32::from(sample) / f32::from(i16::MAX)
}
