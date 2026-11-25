//! DeepFilterNet3 on the microphone's frames (`deep-filter-rt`: libDF's real-time inference on tract, the model in
//! the binary), on the CPU.
//!
//! The model is loaded once for the process, on a thread of its own, when the first call's processing starts
//! ([`prepare`]); each call takes a clone of it with a fresh state. Until the model is ready, the frames pass
//! unchanged: the load ends long before a call's media connects. The model's hop is 10 ms at 48 kHz, the device
//! seam's frame, so the [`Reframer`] passes the frames through; it adapts any other hop at a fixed delay.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

use deep_filter_rt::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};

use crate::devices::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE};

/// The model, loaded once: `Err` says why it could not load. Behind a lock as its plan isn't `Sync`; a call only
/// clones it.
static MODEL: OnceLock<Result<Mutex<DfTract>, String>> = OnceLock::new();

/// Starts the model's load on a thread of its own, unless it is loaded or loads.
pub fn prepare() {
    if MODEL.get().is_none() {
        let started = std::thread::Builder::new().name("teams-media-dfn".into()).spawn(|| {
            model();
        });
        if let Err(error) = started {
            log::warn!("the noise suppression model can't load on a thread of its own: {error}");
        }
    }
}

/// The model, loaded on this thread if it is not yet.
fn model() -> &'static Result<Mutex<DfTract>, String> {
    MODEL.get_or_init(|| {
        let started = std::time::Instant::now();
        let loaded = DfTract::new(DfParams::default(), &RuntimeParams::default_with_ch(1)).map_err(|error| {
            log::warn!("the noise suppression model can't load: {error}");
            error.to_string()
        });
        if loaded.is_ok() {
            log::info!(
                "the noise suppression model loaded in {} ms",
                started.elapsed().as_millis()
            );
        }
        loaded.map(Mutex::new)
    })
}

/// One call's DeepFilterNet: its own state of the model, at an attenuation limit.
pub struct DeepFilter {
    model: DfTract,
    limit: f32,
    frames: Reframer,
}

impl std::fmt::Debug for DeepFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepFilter")
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl DeepFilter {
    /// A fresh state of the model at `limit` dB, once the model is loaded; `None` before, or when it can't load.
    #[must_use]
    pub fn ready(limit: f32) -> Option<Self> {
        Self::from(MODEL.get()?.as_ref().ok()?, limit)
    }

    /// A fresh state of the model at `limit` dB, the model loaded on this thread if it is not yet.
    ///
    /// # Errors
    /// Why the model can't load, or its rate isn't the device seam's.
    pub fn loaded(limit: f32) -> Result<Self, String> {
        let model = model().as_ref().map_err(Clone::clone)?;
        Self::from(model, limit).ok_or_else(|| "the model's rate or channels aren't the device seam's".to_owned())
    }

    fn from(model: &Mutex<DfTract>, limit: f32) -> Option<Self> {
        let model = model.lock().unwrap_or_else(PoisonError::into_inner);
        if model.sr != usize::try_from(SAMPLE_RATE).ok()? || model.ch != 1 {
            return None;
        }
        let mut model = DfTract::clone(&model);
        model.set_atten_lim(limit);
        let frames = Reframer::new(model.hop_size);
        Some(Self { model, limit, frames })
    }

    /// The attenuation limit, in dB.
    #[must_use]
    pub fn limit(&self) -> f32 {
        self.limit
    }

    pub fn set_limit(&mut self, limit: f32) {
        if (limit - self.limit).abs() > f32::EPSILON {
            self.model.set_atten_lim(limit);
            self.limit = limit;
        }
    }

    /// How late the output comes against the input, in samples: the model's own delay and the reframing's.
    #[must_use]
    pub fn delay(&self) -> usize {
        self.model.delay() + self.frames.delay()
    }

    /// Takes the noise out of `frame`, in place.
    ///
    /// # Errors
    /// The model's error on a frame.
    pub fn process(&mut self, frame: &mut AudioFrame) -> Result<(), String> {
        let model = &mut self.model;
        self.frames.push(frame.samples_mut(), |noisy, clean| {
            let noisy = ArrayView2::from_shape((1, noisy.len()), noisy).map_err(|error| error.to_string())?;
            let clean = ArrayViewMut2::from_shape((1, clean.len()), clean).map_err(|error| error.to_string())?;
            model
                .process(noisy, clean)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

/// Adapts the device seam's frames to a model's hop: the samples of each frame go in, whole hops are processed, and
/// each frame takes the same number of samples out, [`Reframer::delay`] samples late. The delay is
/// `hop - gcd(hop, FRAME_SAMPLES)`, the least that never leaves a frame short: 0 when the hop divides the frame.
#[derive(Debug)]
pub struct Reframer {
    hop: usize,
    input: VecDeque<f32>,
    output: VecDeque<f32>,
    delay: usize,
    hop_in: Vec<f32>,
    hop_out: Vec<f32>,
}

impl Reframer {
    /// A reframer for hops of `hop` samples (at least 1).
    #[must_use]
    pub fn new(hop: usize) -> Self {
        let hop = hop.max(1);
        let delay = hop - gcd(hop, FRAME_SAMPLES);
        Self {
            hop,
            input: VecDeque::with_capacity(hop + FRAME_SAMPLES),
            output: std::iter::repeat_n(0.0, delay).collect(),
            delay,
            hop_in: vec![0.0; hop],
            hop_out: vec![0.0; hop],
        }
    }

    /// How late each sample comes out, in samples.
    #[must_use]
    pub fn delay(&self) -> usize {
        self.delay
    }

    /// Puts `frame` in, runs `process` on each whole hop (the hop in, its result out), and puts the next samples out
    /// into `frame`.
    ///
    /// # Errors
    /// The first error of `process`; the frame is then left as it came.
    pub fn push<E>(
        &mut self,
        frame: &mut [f32],
        mut process: impl FnMut(&[f32], &mut [f32]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.input.extend(frame.iter().copied());
        while self.input.len() >= self.hop {
            for (slot, sample) in self.hop_in.iter_mut().zip(self.input.drain(..self.hop)) {
                *slot = sample;
            }
            process(&self.hop_in, &mut self.hop_out)?;
            self.output.extend(self.hop_out.iter().copied());
        }
        let ready = frame.len().min(self.output.len());
        for (slot, sample) in frame.iter_mut().zip(self.output.drain(..ready)) {
            *slot = sample;
        }
        Ok(())
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames * FRAME_SAMPLES)
            .map(|index| f32::from(u16::try_from(index % 4096).unwrap()))
            .collect()
    }

    /// The ramp through a reframer of `hop` that copies each hop, a frame at a time.
    fn copied(hop: usize, frames: usize) -> (Vec<f32>, usize) {
        let mut reframer = Reframer::new(hop);
        let mut out = Vec::new();
        for frame in ramp(frames).chunks(FRAME_SAMPLES) {
            let mut frame = frame.to_vec();
            reframer
                .push::<()>(&mut frame, |input, output| {
                    output.copy_from_slice(input);
                    Ok(())
                })
                .unwrap();
            assert!(
                reframer.output.len() < reframer.hop + FRAME_SAMPLES,
                "the output stays bounded"
            );
            out.extend(frame);
        }
        (out, reframer.delay())
    }

    #[test]
    fn a_hop_of_the_frame_passes_each_frame_through_at_once() {
        let (out, delay) = copied(FRAME_SAMPLES, 10);
        assert_eq!(delay, 0);
        assert_eq!(out, ramp(10));
    }

    #[test]
    fn another_hop_comes_out_whole_at_a_fixed_delay() {
        for hop in [160, 256, 441, 512, 960, 1024] {
            let (out, delay) = copied(hop, 20);
            assert_eq!(delay, hop - gcd(hop, FRAME_SAMPLES), "hop {hop}");
            assert!(delay < hop);
            let expected: Vec<f32> = std::iter::repeat_n(0.0, delay)
                .chain(ramp(20))
                .take(out.len())
                .collect();
            assert_eq!(out, expected, "hop {hop}");
        }
    }

    #[test]
    fn a_failed_hop_leaves_the_frame_as_it_came() {
        let mut reframer = Reframer::new(FRAME_SAMPLES);
        let mut frame = vec![0.5; FRAME_SAMPLES];
        let failed = reframer.push(&mut frame, |_, _| Err("refused"));
        assert_eq!(failed, Err("refused"));
        assert_eq!(frame, vec![0.5; FRAME_SAMPLES]);
        assert_eq!(Reframer::new(0).hop, 1);
    }

    #[test]
    fn a_fresh_state_takes_the_limit_and_the_model_s_delay() {
        let mut filter = DeepFilter::loaded(NO_LIMIT).unwrap();
        assert!((filter.limit() - NO_LIMIT).abs() < f32::EPSILON);
        // 10 ms of overlap and two frames of lookahead
        assert_eq!(filter.delay(), 3 * FRAME_SAMPLES);
        filter.set_limit(12.0);
        assert!((filter.limit() - 12.0).abs() < f32::EPSILON);
        assert!(DeepFilter::ready(12.0).is_some(), "the model stays loaded");
        prepare();
        let mut frame = AudioFrame::silence();
        filter.process(&mut frame).unwrap();
        assert_eq!(frame, AudioFrame::silence());
        assert!(format!("{filter:?}").contains("limit"));
    }

    const NO_LIMIT: f32 = super::super::noise::NO_LIMIT_DB;
}
