//! The device seam's file backend, which opens no audio server and no camera: a tone or a WAV file in at the device's
//! pace, a WAV recording out (readable while it is still being written), the test pattern as the camera and received
//! frames counted; the tone and its measurement, and WAV files read and written.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::devices::pattern::{self, TestPattern};
use teams_media::devices::wav::{self, WavWriter};
use teams_media::devices::{
    AudioFrame, DeviceError, Devices, FRAME_DURATION, FRAME_SAMPLES, PixelFormat, SAMPLE_RATE, VideoFrame, VideoSink,
};
use teams_media::tone::{Tone, ToneGenerator, amplitude_at, decibels};

const TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const NOW: Duration = Duration::ZERO;
const PLENTY: Duration = Duration::from_secs(1);

fn devices(input: AudioInput) -> FileDevices {
    FileDevices {
        input,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::Unpaced,
    }
}

/// `count` frames of the microphone, read back to back.
fn read_frames(devices: &FileDevices, count: usize) -> Vec<f32> {
    let mut microphone = devices.microphone().unwrap();
    let mut samples = Vec::new();
    for _ in 0..count {
        let frame = microphone.next_frame(PLENTY).unwrap().expect("a frame");
        samples.extend_from_slice(frame.samples());
    }
    samples
}

#[test]
fn a_tone_is_generated_without_phase_jumps_and_measured_back() {
    let mut generator = ToneGenerator::new(TONE);
    let mut samples = vec![0.0; SAMPLE_RATE as usize];
    for frame in samples.chunks_mut(FRAME_SAMPLES) {
        generator.fill(frame);
    }
    let level = amplitude_at(&samples, TONE.hz);
    assert!((level - TONE.amplitude).abs() < 0.01, "{level}");
    assert!(amplitude_at(&samples, 1_000.0) < 0.01);
    assert!(amplitude_at(&[0.0; FRAME_SAMPLES], TONE.hz) < 1e-6);
    assert!(amplitude_at(&[], TONE.hz).abs() < f32::EPSILON);
    // no sample-to-sample step bigger than the tone's slope allows
    let max_step = 2.0 * std::f32::consts::PI * TONE.hz / 48_000.0 * TONE.amplitude * 1.01;
    assert!(samples.windows(2).all(|pair| (pair[1] - pair[0]).abs() <= max_step));
    assert!((decibels(0.125, 0.5) + 12.04).abs() < 0.01);
}

#[test]
fn the_microphone_hears_the_tone_frame_after_frame() {
    let samples = read_frames(&devices(AudioInput::Tone(TONE)), 100);
    assert_eq!(samples.len(), 100 * FRAME_SAMPLES);
    let level = amplitude_at(&samples, TONE.hz);
    assert!((level - TONE.amplitude).abs() < 0.01, "{level}");
    let silence = read_frames(&devices(AudioInput::Silence), 10);
    assert!(silence.iter().all(|sample| sample.abs() < f32::EPSILON));
}

#[test]
fn a_real_time_source_hands_out_frames_at_the_device_s_pace() {
    let devices = FileDevices {
        pacing: Pacing::RealTime,
        ..devices(AudioInput::Tone(TONE))
    };
    let mut microphone = devices.microphone().unwrap();
    let start = Instant::now();
    for _ in 0..30 {
        microphone.next_frame(PLENTY).unwrap().expect("a frame");
    }
    // the first frame is due at once, the 30th 290 ms later
    let elapsed = start.elapsed();
    assert!(elapsed >= FRAME_DURATION * 29, "{elapsed:?}");
    // a frame not due yet isn't handed out before its time
    assert!(microphone.next_frame(NOW).unwrap().is_none());
    assert!(microphone.next_frame(PLENTY).unwrap().is_some());

    let mut camera = FileDevices {
        pacing: Pacing::RealTime,
        ..devices
    }
    .camera()
    .unwrap();
    let start = Instant::now();
    for _ in 0..4 {
        camera.next_frame(PLENTY).unwrap().expect("a frame");
    }
    // 15 frames a second: the fourth 200 ms after the first
    assert!(start.elapsed() >= Duration::from_millis(195), "{:?}", start.elapsed());
}

#[test]
fn a_wav_file_is_played_once_then_silence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.wav");
    let mut generator = ToneGenerator::new(TONE);
    let mut samples = vec![0.0; 25 * FRAME_SAMPLES + 100];
    generator.fill(&mut samples);
    let mut writer = WavWriter::create(&path).unwrap();
    writer.write(&samples).unwrap();
    writer.finish().unwrap();

    let heard = read_frames(&devices(AudioInput::Wav(path)), 30);
    let (played, after) = heard.split_at(samples.len());
    // 16-bit PCM: within one step of what went in
    assert!(
        played
            .iter()
            .zip(&samples)
            .all(|(heard, sent)| (heard - sent).abs() < 1.0 / 16_000.0)
    );
    assert!(after.iter().all(|sample| sample.abs() < f32::EPSILON));
}

#[test]
fn the_speaker_records_what_it_plays_and_the_recording_is_readable_while_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.wav");
    let devices = FileDevices {
        recording: Some(path.clone()),
        ..devices(AudioInput::Silence)
    };
    let mut speaker = devices.speaker().unwrap();
    let mut generator = ToneGenerator::new(TONE);
    let mut frame = AudioFrame::silence();
    for _ in 0..150 {
        generator.fill(frame.samples_mut());
        speaker.play(&frame).unwrap();
    }
    // 1.5 s played: the header was brought up to date after the first second, before the writer was done
    let partial = wav::read(&path).unwrap();
    assert!(partial.len() >= SAMPLE_RATE as usize, "{}", partial.len());
    assert!((amplitude_at(&partial[..SAMPLE_RATE as usize], TONE.hz) - TONE.amplitude).abs() < 0.01);
    drop(speaker);
    let whole = wav::read(&path).unwrap();
    assert_eq!(whole.len(), 150 * FRAME_SAMPLES);
    #[cfg(unix)]
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

    // without a recording, what is played goes nowhere
    let mut nowhere = self::devices(AudioInput::Silence).speaker().unwrap();
    nowhere.play(&frame).unwrap();
}

#[test]
fn wav_files_are_read_in_the_forms_the_backend_takes_and_refused_otherwise() {
    // stereo 32-bit float, mixed down
    let mut stereo = wav_header(3, 2, 32, 48_000, 16);
    for (left, right) in [(0.5_f32, 0.25_f32), (-1.0, 1.0)] {
        stereo.extend_from_slice(&left.to_le_bytes());
        stereo.extend_from_slice(&right.to_le_bytes());
    }
    assert_eq!(wav::decode(&stereo).unwrap(), [0.375, 0.0]);
    // an unfinished recording: its data size still 0, read to the end
    let mut unfinished = wav_header(1, 1, 16, 48_000, 0);
    unfinished.extend_from_slice(&16_384_i16.to_le_bytes());
    assert_eq!(wav::decode(&unfinished).unwrap(), [0.5]);
    for (bytes, wanted) in [
        (wav_header(1, 1, 16, 44_100, 0), "44100 Hz"),
        (wav_header(1, 1, 24, 48_000, 0), "24 bits"),
        (b"RIFF\0\0\0\0WAVEdata\0\0\0\0".to_vec(), "before its fmt"),
        (b"not a wav".to_vec(), "RIFF"),
    ] {
        let error = wav::decode(&bytes).unwrap_err();
        assert!(
            matches!(&error, DeviceError::Wav(text) if text.contains(wanted)),
            "{error}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        wav::read(&dir.path().join("missing.wav")),
        Err(DeviceError::Io(_))
    ));
}

/// A WAV header of the given format, its `data` chunk declaring `data` bytes.
fn wav_header(format: u16, channels: u16, bits: u16, rate: u32, data: u32) -> Vec<u8> {
    let block = channels * bits / 8;
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&format.to_le_bytes());
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    header.extend_from_slice(&block.to_le_bytes());
    header.extend_from_slice(&bits.to_le_bytes());
    // an unknown chunk first, odd-sized (padded), skipped
    header.extend_from_slice(b"LIST\x03\0\0\0abc\0");
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data.to_le_bytes());
    header
}

#[test]
fn the_camera_is_the_test_pattern_whose_frames_carry_their_index_and_move() {
    let mut camera = devices(AudioInput::Silence).camera().unwrap();
    let first = camera.next_frame(PLENTY).unwrap().expect("a frame");
    let second = camera.next_frame(PLENTY).unwrap().expect("a frame");
    assert_eq!((first.width, first.height, first.format), (640, 360, PixelFormat::I420));
    assert_eq!(first.stride, 640);
    assert_eq!(first.data.len(), 640 * 360 * 3 / 2);
    assert_eq!(pattern::index_of(&first), Some(0));
    assert_eq!(pattern::index_of(&second), Some(1));
    assert_ne!(first.data, second.data, "the bar moves");

    let pattern = TestPattern::new(640, 360);
    for index in [0, 1, 0x8000_0001, u32::MAX] {
        assert_eq!(pattern::index_of(&pattern.frame(index)), Some(index));
    }
    // too small to carry an index, or a picture that isn't the pattern
    assert_eq!(pattern::index_of(&TestPattern::new(160, 120).frame(3)), None);
    let blank = VideoFrame {
        data: vec![0; 640 * 360 * 3 / 2],
        ..pattern.frame(0)
    };
    assert_eq!(pattern::index_of(&blank), None);
    // a packed RGB frame of the pattern (a screen's) is read by its greys
    let i420 = pattern.frame(7);
    let bgrx = VideoFrame {
        format: PixelFormat::Bgrx,
        stride: 640 * 4,
        data: i420.data[..640 * 360]
            .iter()
            .flat_map(|&luma| [luma, luma, luma, 255])
            .collect(),
        ..i420
    };
    assert_eq!(pattern::index_of(&bgrx), Some(7));
}

#[test]
fn received_frames_are_counted_with_their_changes_and_gaps() {
    let counter = FrameCounter::new();
    let mut sink: Box<dyn VideoSink> = Box::new(counter.clone());
    let pattern = TestPattern::new(640, 360);
    for index in [0, 1, 1, 2, 5] {
        sink.show(&pattern.frame(index)).unwrap();
    }
    let counts = counter.counts();
    assert_eq!(counts.frames, 5);
    assert_eq!(counts.changed, 4, "the repeated frame doesn't count as a change");
    assert_eq!(counts.indexed, 5);
    // 1 after 1 (repeated) and 5 after 2 (two lost)
    assert_eq!(counts.gaps, 2);
    assert_eq!(counts.last_index, Some(5));
}
