//! The microphone's audio processing on the device seam's 10 ms frames: what the speaker plays comes back into the
//! microphone after a delay and an attenuation, and is handed to the processing as the far end on the same frames;
//! the echo is suppressed by a set margin, a near-end voice is kept while both talk, and noise under a voice is
//! lowered. The signals are `teams_media::test_signal`'s (a steady tone is noise to the noise suppression), all
//! synthetic. The echo, double-talk and steady-noise tests run the classic suppressor (Low); the levels' tests compare
//! DeepFilterNet (High) with it on noise that comes and goes.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use teams_media::devices::AudioFrame;
use teams_media::processing::deep::DeepFilter;
use teams_media::processing::noise::{AUTO_LIMIT_DB, NO_LIMIT_DB};
use teams_media::processing::{AudioProcessing, NoiseControl, NoiseSuppression, ProcessingConfig};
use teams_media::test_signal::{Noise, gap_level, rms, speech_like, voice, voice_level};
use teams_media::tone::decibels;

/// How far below what the microphone hears the echo must be once the canceller has converged, in dB.
const ECHO_MARGIN_DB: f32 = 25.0;
/// How far a near-end voice may fall while the far end talks too, in dB.
const KEPT_DB: f32 = 6.0;
/// How far steady noise must be lowered, in dB.
const NOISE_MARGIN_DB: f32 = 6.0;
/// How far the voice under that noise may fall, in dB.
const VOICE_UNDER_NOISE_DB: f32 = 3.0;

/// Frames the canceller is given to converge before it is judged (3 s).
const CONVERGE: usize = 300;
/// Frames skipped at each change of the voice: the processing delays its output by up to about 15 ms.
const SKIP: usize = 2;
/// How much louder the far end is played than the microphone hears it back.
const ATTENUATION_DB: f32 = 6.0;
const FAR_PEAK: f32 = 0.5;
const VOICE: f32 = 0.2;

/// The sink looped back into the source: each far-end frame is handed to the processing as it is played, and the
/// microphone hears it `delay` frames later, [`ATTENUATION_DB`] down, over `near`'s frame. Returns what the
/// microphone heard and what the processing made of it.
fn looped(
    processing: &AudioProcessing,
    far: &[AudioFrame],
    near: &[AudioFrame],
    delay: usize,
) -> (Vec<AudioFrame>, Vec<AudioFrame>) {
    assert_eq!(far.len(), near.len());
    let gain = 10.0_f32.powf(-ATTENUATION_DB / 20.0);
    let mut heard = Vec::with_capacity(far.len());
    let mut processed = Vec::with_capacity(far.len());
    for (index, played) in far.iter().enumerate() {
        processing.far_end(played).unwrap();
        let mut microphone = near[index].clone();
        if let Some(echoed) = index.checked_sub(delay).map(|at| &far[at]) {
            for (sample, echo) in microphone.samples_mut().iter_mut().zip(echoed.samples()) {
                *sample += gain * echo;
            }
        }
        heard.push(microphone.clone());
        processing.near_end(&mut microphone).unwrap();
        processed.push(microphone);
    }
    (heard, processed)
}

/// The call's processing with the classic suppressor (Low), whose model needs no load.
fn classic() -> AudioProcessing {
    AudioProcessing::with_noise(ProcessingConfig::CALL, NoiseControl::new(NoiseSuppression::Low)).unwrap()
}

fn silence(frames: usize) -> Vec<AudioFrame> {
    vec![AudioFrame::silence(); frames]
}

#[test]
fn the_echo_of_what_the_speaker_plays_is_suppressed_by_the_margin() {
    // 100 ms, and the UI sandbox's loopback's 250 ms
    for delay in [10, 25] {
        let processing = classic();
        let far = speech_like(1000, 7, FAR_PEAK);
        let (heard, processed) = looped(&processing, &far, &silence(1000), delay);
        let (echo, left) = (rms(&heard[CONVERGE..]), rms(&processed[CONVERGE..]));
        let suppressed = -decibels(left, echo);
        eprintln!("{delay}0 ms: the echo at {echo:.4} comes out at {left:.6}, {suppressed:.1} dB down");
        assert!(echo > 0.02, "the microphone hears the echo: {echo}");
        assert!(
            suppressed >= ECHO_MARGIN_DB,
            "{delay}0 ms: the echo is only {suppressed:.1} dB down"
        );
    }
}

/// QA of task 844: the echo canceller stays at every level, Off (no noise suppression) and DeepFilterNet's too.
#[test]
fn the_echo_is_suppressed_by_the_margin_at_each_level() {
    DeepFilter::loaded(NO_LIMIT_DB).unwrap();
    for level in [NoiseSuppression::Off, NoiseSuppression::Auto, NoiseSuppression::High] {
        for delay in [10, 25] {
            let processing = AudioProcessing::with_noise(ProcessingConfig::CALL, NoiseControl::new(level)).unwrap();
            let far = speech_like(1000, 7, FAR_PEAK);
            let (heard, processed) = looped(&processing, &far, &silence(1000), delay);
            let (echo, left) = (rms(&heard[CONVERGE..]), rms(&processed[CONVERGE..]));
            let suppressed = -decibels(left, echo);
            eprintln!("{level:?}, {delay}0 ms: the echo at {echo:.4} comes out at {left:.6}, {suppressed:.1} dB down");
            assert!(
                suppressed >= ECHO_MARGIN_DB,
                "{level:?}, {delay}0 ms: the echo is only {suppressed:.1} dB down"
            );
        }
    }
}

#[test]
fn with_the_processing_off_the_microphone_s_frames_pass_unchanged() {
    let processing = AudioProcessing::with_noise(ProcessingConfig::OFF, NoiseControl::default()).unwrap();
    assert!(!processing.config().any());
    let far = speech_like(300, 7, FAR_PEAK);
    let (heard, processed) = looped(&processing, &far, &voice(300, VOICE), 10);
    assert_eq!(heard, processed);
}

#[test]
fn a_near_end_voice_is_kept_while_both_talk() {
    let processing = classic();
    let far = speech_like(2000, 11, FAR_PEAK);
    // the far end alone for 10 s, then both
    let mut near = silence(1000);
    near.extend(voice(1000, VOICE));
    let (heard, processed) = looped(&processing, &far, &near, 10);
    let (heard, processed) = (&heard[1000..], &processed[1000..]);
    // judged from 1 s into the double talk, once the gain control has settled on the voice
    let (spoken, kept) = (
        voice_level(heard, 100, 1000, SKIP),
        voice_level(processed, 100, 1000, SKIP),
    );
    let change = decibels(kept, spoken);
    eprintln!("the voice at {spoken:.4} comes out at {kept:.4} ({change:.1} dB) while the far end talks");
    assert!(change >= -KEPT_DB, "the voice falls {:.1} dB while both talk", -change);
    // and between the voice's segments, the echo is still taken out
    let (echo, left) = (gap_level(heard, 0, 1000, SKIP), gap_level(processed, 0, 1000, SKIP));
    let suppressed = -decibels(left, echo);
    eprintln!("between the voice's segments the echo at {echo:.4} comes out at {left:.6}, {suppressed:.1} dB down");
    assert!(
        suppressed >= ECHO_MARGIN_DB,
        "the echo between the voice's segments is only {suppressed:.1} dB down"
    );
}

#[test]
fn noise_under_a_voice_is_lowered_and_the_voice_kept() {
    let processing = classic();
    // noise alone for 3 s, then the voice over it
    let mut near = silence(CONVERGE);
    near.extend(voice(1200, VOICE));
    Noise::new(3).add_to(&mut near, 0.05);
    let (heard, processed) = looped(&processing, &silence(near.len()), &near, 0);
    let voiced = |frames: &[AudioFrame]| voice_level(&frames[CONVERGE..], 200, 1200, SKIP);
    let (spoken, kept) = (voiced(&heard), voiced(&processed));
    let gaps = |frames: &[AudioFrame]| gap_level(&frames[CONVERGE..], 200, 1200, SKIP);
    let (noise, left) = (gaps(&heard), gaps(&processed));
    let lowered = -decibels(left, noise);
    let change = decibels(kept, spoken);
    eprintln!(
        "noise at {noise:.4} comes out at {left:.4} ({lowered:.1} dB down); the voice at {spoken:.4} at {kept:.4} \
         ({change:.1} dB)"
    );
    assert!(lowered >= NOISE_MARGIN_DB, "the noise is only {lowered:.1} dB down");
    assert!(change >= -VOICE_UNDER_NOISE_DB, "the voice falls {:.1} dB", -change);
    // before the voice, the noise alone is lowered too
    let (alone, left_alone) = (rms(&heard[100..CONVERGE]), rms(&processed[100..CONVERGE]));
    let lowered_alone = -decibels(left_alone, alone);
    assert!(
        lowered_alone >= NOISE_MARGIN_DB,
        "noise alone is only {lowered_alone:.1} dB down"
    );
}

/// Only the noise suppression: no echo canceller or gain control to change the levels compared.
const NOISE_ONLY: ProcessingConfig = ProcessingConfig {
    echo_cancellation: false,
    noise_suppression: true,
    gain_control: false,
};
/// Frames of noise alone before the voice starts (3 s).
const NOISE_ALONE: usize = 300;
/// How much further High must lower the noise than Low, in dB.
const HIGH_OVER_LOW_DB: f32 = 6.0;
/// How far a level's measured attenuation may pass its limit.
const LIMIT_MARGIN_DB: f32 = 1.0;

/// Noise that comes and goes as keys, steps or a door do: 40 ms bursts of white noise, each at a random level of up
/// to `peak` or silent, from `seed`.
fn bursts(frames: usize, seed: u64, peak: f32) -> Vec<AudioFrame> {
    let mut noise = Noise::new(seed);
    let mut out = silence(frames);
    for burst in out.chunks_mut(4) {
        let level = peak * noise.next_sample().max(0.0);
        noise.add_to(burst, level);
    }
    out
}

/// [`NOISE_ALONE`] frames of the bursts, then the voice over them for `voiced` frames.
fn noisy_voice(voiced: usize) -> Vec<AudioFrame> {
    let mut near = silence(NOISE_ALONE);
    near.extend(voice(voiced, VOICE));
    for (frame, noise) in near.iter_mut().zip(bursts(NOISE_ALONE + voiced, 5, 0.1)) {
        for (sample, added) in frame.samples_mut().iter_mut().zip(noise.samples()) {
            *sample += added;
        }
    }
    near
}

/// `near` through the noise suppression at `level`, with the model loaded first.
fn suppressed(near: &[AudioFrame], level: NoiseSuppression) -> Vec<AudioFrame> {
    DeepFilter::loaded(NO_LIMIT_DB).unwrap();
    let processing = AudioProcessing::with_noise(NOISE_ONLY, NoiseControl::new(level)).unwrap();
    assert_eq!(processing.noise_level(), level);
    looped(&processing, &silence(near.len()), near, 0).1
}

#[test]
fn high_lowers_noise_that_comes_and_goes_more_than_low() {
    let near = noisy_voice(500);
    let (low, high) = (
        suppressed(&near, NoiseSuppression::Low),
        suppressed(&near, NoiseSuppression::High),
    );
    let alone = |frames: &[AudioFrame]| rms(&frames[100..NOISE_ALONE]);
    let (noise, at_low, at_high) = (alone(&near), alone(&low), alone(&high));
    let (low_db, high_db) = (-decibels(at_low, noise), -decibels(at_high, noise));
    eprintln!("noise alone at {noise:.4}: Low takes {low_db:.1} dB out, High {high_db:.1} dB");
    assert!(noise > 0.01, "the noise is heard: {noise}");
    assert!(
        high_db >= low_db + HIGH_OVER_LOW_DB,
        "High takes {high_db:.1} dB out, Low {low_db:.1} dB"
    );
    // and between the voice's syllables, past DeepFilterNet's 30 ms delay
    let gaps = |frames: &[AudioFrame]| gap_level(&frames[NOISE_ALONE..], 0, 500, 4);
    let (gap_low, gap_high) = (gaps(&low), gaps(&high));
    eprintln!("between the syllables: Low leaves {gap_low:.5}, High {gap_high:.5}");
    assert!(
        gap_high < gap_low,
        "High leaves {gap_high} between the syllables, Low {gap_low}"
    );
}

#[test]
fn at_off_the_noise_passes() {
    let near = noisy_voice(100);
    let off = suppressed(&near, NoiseSuppression::Off);
    let (noise, left) = (rms(&near[100..NOISE_ALONE]), rms(&off[100..NOISE_ALONE]));
    assert!(
        decibels(left, noise).abs() < 0.5,
        "the noise at {noise} comes out at {left}"
    );
    let none = AudioProcessing::with_noise(ProcessingConfig::OFF, NoiseControl::new(NoiseSuppression::High)).unwrap();
    assert_eq!(none.noise_level(), NoiseSuppression::Off);
}

#[test]
fn a_level_changed_during_the_call_applies_at_once() {
    DeepFilter::loaded(NO_LIMIT_DB).unwrap();
    let control = NoiseControl::new(NoiseSuppression::Off);
    let processing = AudioProcessing::with_noise(NOISE_ONLY, control.clone()).unwrap();
    let near = bursts(1500, 9, 0.1);
    let mut out = Vec::with_capacity(near.len());
    // 3 s at each level: Off, High, Auto on High's state, Low, then High on a fresh state
    for (stretch, level) in near.chunks(300).zip([
        NoiseSuppression::Off,
        NoiseSuppression::High,
        NoiseSuppression::Auto,
        NoiseSuppression::Low,
        NoiseSuppression::High,
    ]) {
        control.set(level);
        assert_eq!(processing.noise_level(), level);
        out.extend(looped(&processing, &silence(stretch.len()), stretch, 0).1);
    }
    // judged from 1 s into each stretch, once the suppressor has settled
    let lowered = |stretch: usize| {
        let at = stretch * 300 + 100..stretch * 300 + 300;
        -decibels(rms(&out[at.clone()]), rms(&near[at]))
    };
    let (off, high, auto, low, high_again) = (lowered(0), lowered(1), lowered(2), lowered(3), lowered(4));
    eprintln!("lowered by: Off {off:.1} dB, High {high:.1}, Auto {auto:.1}, Low {low:.1}, High {high_again:.1}");
    assert!(off.abs() < 0.5, "Off lowers the noise {off:.1} dB");
    assert!(high > low + HIGH_OVER_LOW_DB, "High {high:.1} dB, Low {low:.1} dB");
    assert!(auto > low, "Auto {auto:.1} dB, Low {low:.1} dB");
    assert!(
        auto <= AUTO_LIMIT_DB + LIMIT_MARGIN_DB,
        "Auto {auto:.1} dB runs past its {AUTO_LIMIT_DB} dB limit"
    );
    assert!(
        auto + LIMIT_MARGIN_DB < high,
        "Auto {auto:.1} dB, High {high:.1} dB: Auto is the gentler"
    );
    assert!(
        auto + LIMIT_MARGIN_DB < high_again,
        "High after Low {high_again:.1} dB, Auto {auto:.1} dB"
    );
}
