//! A call's processing loads DeepFilterNet's model itself: in a process of its own, so no other test loads the
//! process-wide model first. The signal is `teams_media::test_signal`'s noise, synthetic.

// test code throughout (it unwraps like the tests do)
#![cfg(test)]

use teams_media::devices::AudioFrame;
use teams_media::processing::{AudioProcessing, NoiseControl, NoiseSuppression, ProcessingConfig, from_i16, to_i16};
use teams_media::test_signal::{Noise, rms};
use teams_media::tone::decibels;

/// Only the noise suppression: no echo canceller or gain control to change the frames.
const NOISE_ONLY: ProcessingConfig = ProcessingConfig {
    echo_cancellation: false,
    noise_suppression: true,
    gain_control: false,
};
/// How far High must lower the noise once the model is ready, in dB.
const LOWERED_DB: f32 = 6.0;

fn at_i16(frame: &AudioFrame) -> Vec<f32> {
    to_i16(frame).into_iter().map(from_i16).collect()
}

#[test]
fn a_call_s_processing_loads_the_model_itself_and_the_frames_pass_until_it_is_ready() {
    let processing = AudioProcessing::with_noise(NOISE_ONLY, NoiseControl::new(NoiseSuppression::High)).unwrap();
    assert!(format!("{processing:?}").contains("High"), "{processing:?}");
    assert_eq!(ProcessingConfig::default(), ProcessingConfig::CALL);
    let mut noise = vec![AudioFrame::silence(); 100];
    Noise::new(13).add_to(&mut noise, 0.05);
    // the load takes far longer than one frame: the first frame passes as it came
    let mut first = noise[0].clone();
    processing.near_end(&mut first).unwrap();
    assert_eq!(at_i16(&first), at_i16(&noise[0]));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let lowered = loop {
        let mut out = noise.clone();
        for frame in &mut out {
            processing.near_end(frame).unwrap();
        }
        if -decibels(rms(&out[50..]), rms(&noise[50..])) > LOWERED_DB {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(
        lowered,
        "the model loaded on its own thread within a minute and took the noise out"
    );
}
