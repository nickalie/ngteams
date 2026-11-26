use super::*;

/// The synthetic, marked video sticker of the fixtures (see its first line).
fn synthetic_webm() -> Vec<u8> {
    let hex = include_str!("../../../tests/fixtures/telegram/synthetic-sticker.webm.hex");
    let digits: String = hex
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .concat();
    digits
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn all_frames(bytes: &[u8]) -> Result<Vec<RgbaFrame>, VideoFileError> {
    Ok(frames_and_length(bytes)?.0)
}

fn frames_and_length(bytes: &[u8]) -> Result<(Vec<RgbaFrame>, Option<Duration>), VideoFileError> {
    let mut frames = Vec::new();
    let length = decode_webm(bytes, &mut |frame| {
        frames.push(frame);
        ControlFlow::Continue(())
    })?;
    Ok((frames, length))
}

fn about_half_a_second(length: Option<Duration>) -> bool {
    length.is_some_and(|length| length.as_millis().abs_diff(500) <= 2)
}

fn pixel(frame: &RgbaFrame, x: u32, y: u32) -> [u8; 4] {
    let at = usize::try_from((y * frame.width + x) * 4).unwrap();
    frame.rgba[at..at + 4].try_into().unwrap()
}

fn near(got: [u8; 4], want: [u8; 4]) -> bool {
    got.iter().zip(want).all(|(got, want)| got.abs_diff(want) <= 8)
}

#[test]
fn the_synthetic_sticker_decodes_with_its_alpha_and_its_times() {
    let (frames, length) = frames_and_length(&synthetic_webm()).unwrap();
    assert!(
        [14, 15].contains(&frames.len()),
        "15 frames at 30 fps, the last one often not given by the alpha decoder: {}",
        frames.len()
    );
    assert!(about_half_a_second(length), "the length the file declares: {length:?}");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!((frame.width, frame.height), (64, 64));
        let millis = u64::try_from(frame.at.as_millis()).unwrap();
        assert!(
            millis.abs_diff(u64::try_from(index).unwrap() * 100 / 3) <= 1,
            "{index}: {millis} ms"
        );
        assert!(
            near(pixel(frame, 10, 50), [255, 0, 0, 255]),
            "{index}: {:?}",
            pixel(frame, 10, 50)
        );
        assert_eq!(pixel(frame, 50, 10)[3], 0, "{index}: the top right is clear");
        assert!(
            near(pixel(frame, 50, 50), [0, 255, 0, 255]),
            "{index}: {:?}",
            pixel(frame, 50, 50)
        );
    }
    let bar = |frame: &RgbaFrame, x| near(pixel(frame, x, 6), [255, 255, 255, 255]);
    assert!(
        bar(&frames[0], 1) && !bar(&frames[0], 9),
        "the white bar starts at the left"
    );
    assert!(bar(&frames[2], 9) && !bar(&frames[2], 1), "and moves 4 px a frame");
}

#[test]
fn a_reader_that_has_enough_stops_the_read() {
    let mut count = 0;
    let length = decode_webm(&synthetic_webm(), &mut |_| {
        count += 1;
        if count == 2 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();
    assert_eq!(count, 2);
    assert!(about_half_a_second(length), "{length:?}");
}

#[test]
fn what_is_not_a_whole_webm_is_refused() {
    assert_eq!(all_frames(b"RIFF....WEBPVP8 ").unwrap_err(), VideoFileError::NotWebm);
    assert!(!is_webm(&[0x1a, 0x45]));
    let mut damaged = synthetic_webm();
    damaged.truncate(60);
    damaged.extend(std::iter::repeat_n(0x5a, 400));
    let got = all_frames(&damaged);
    assert!(
        matches!(got, Err(VideoFileError::Damaged)) || got.as_ref().is_ok_and(Vec::is_empty),
        "{got:?}"
    );
}

#[test]
fn a_missing_decoder_and_a_damaged_stream_are_told_apart() {
    start_gstreamer().unwrap();
    let missing = gst::message::Error::new(gst::CoreError::MissingPlugin, "no VP9 decoder");
    assert_eq!(failure(&missing), VideoFileError::NoDecoder);
    let codec = gst::message::Error::new(gst::StreamError::CodecNotFound, "no VP9 decoder");
    assert_eq!(failure(&codec), VideoFileError::NoDecoder);
    let damaged = gst::message::Error::new(gst::StreamError::Decode, "bad block");
    assert_eq!(failure(&damaged), VideoFileError::Damaged);
    assert_eq!(failure(&gst::message::Eos::new()), VideoFileError::Damaged);
}

#[test]
fn a_sample_that_is_not_a_whole_frame_is_refused() {
    start_gstreamer().unwrap();
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", 2_i32)
        .field("height", 2_i32)
        .build();
    let short = gst::Sample::builder()
        .caps(&caps)
        .buffer(&gst::Buffer::from_slice(vec![0_u8; 15]))
        .build();
    assert_eq!(frame_of(&short), None);
    let mut buffer = gst::Buffer::from_slice(vec![7_u8; 16]);
    buffer.get_mut().unwrap().set_pts(gst::ClockTime::from_mseconds(40));
    let whole = gst::Sample::builder().caps(&caps).buffer(&buffer).build();
    let frame = frame_of(&whole).unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.at, frame.length),
        (2, 2, Duration::from_millis(40), None)
    );
    assert_eq!(
        frame_of(&gst::Sample::builder().buffer(&buffer).build()),
        None,
        "no caps"
    );
}

#[test]
fn a_stream_that_is_not_video_drains_to_a_fakesink() {
    start_gstreamer().unwrap();
    let pipeline = gst::Pipeline::new();
    let sound = gst::ElementFactory::make("audiotestsrc").build().unwrap();
    pipeline.add(&sound).unwrap();
    let pad = sound.static_pad("src").unwrap();
    assert!(!is_video(&pad));
    drain(&pipeline, &pad);
    assert!(pad.is_linked());
    let _ = pipeline.set_state(gst::State::Null);
}
