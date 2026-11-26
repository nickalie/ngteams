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
    let length = decode_webm(bytes, 4096, &mut |frame| {
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
    let length = decode_webm(&synthetic_webm(), 4096, &mut |_| {
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
    assert!(!is_type(&caps_of(&pad), "video/"));
    drain(&pipeline, &pad);
    assert!(pad.is_linked());
    let _ = pipeline.set_state(gst::State::Null);
}

/// Reads `bytes` within `limits`, and what that gave: the count of frames, or why none.
fn read_within(bytes: &[u8], limits: Limits) -> Result<usize, VideoFileError> {
    start_gstreamer().unwrap();
    let mut count = 0;
    read(bytes, limits, &mut |_| {
        count += 1;
        ControlFlow::Continue(())
    })?;
    Ok(count)
}

const ANY: Limits = Limits {
    max_side: 4096,
    alpha_decoder: true,
};

#[test]
fn a_stream_with_alpha_and_no_alpha_decoder_is_not_decoded_without_it() {
    let without = Limits {
        alpha_decoder: false,
        ..ANY
    };
    assert_eq!(read_within(&synthetic_webm(), without), Err(VideoFileError::NoDecoder));
    assert!(read_within(&synthetic_webm(), ANY).unwrap() >= 14);
}

#[test]
fn a_frame_larger_than_the_limit_is_never_decoded() {
    let small = Limits { max_side: 63, ..ANY };
    assert_eq!(read_within(&synthetic_webm(), small), Err(VideoFileError::TooBig));
    assert_eq!(
        read_within(&synthetic_webm(), Limits { max_side: 64, ..ANY }).map(|n| n >= 14),
        Ok(true)
    );
}

#[test]
fn the_limits_refuse_a_large_or_an_alpha_stream_only() {
    start_gstreamer().unwrap();
    let vp9 = |width: i32, alpha: bool| {
        gst::Caps::builder("video/x-vp9")
            .field("width", width)
            .field("height", 64_i32)
            .field("codec-alpha", alpha)
            .build()
    };
    let without = Limits {
        max_side: 64,
        alpha_decoder: false,
    };
    assert_eq!(without.refuses(&vp9(64, false)), None);
    assert_eq!(without.refuses(&vp9(64, true)), Some(VideoFileError::NoDecoder));
    assert_eq!(without.refuses(&vp9(65, false)), Some(VideoFileError::TooBig));
    assert_eq!(Limits { max_side: 64, ..ANY }.refuses(&vp9(64, true)), None);
    let vp8 = gst::Caps::builder("video/x-vp8").field("codec-alpha", true).build();
    assert_eq!(without.refuses(&vp8), Some(VideoFileError::NoDecoder));
    let raw = gst::Caps::builder("video/x-raw").field("height", 65_i32).build();
    assert_eq!(without.refuses(&raw), Some(VideoFileError::TooBig));
    assert_eq!(without.refuses(&gst::Caps::new_empty()), None);
    let sink = without.sink_caps();
    assert!(sink.can_intersect(&gst::Caps::builder("video/x-raw").field("width", 64_i32).build()));
    assert!(!sink.can_intersect(&gst::Caps::builder("video/x-raw").field("width", 65_i32).build()));
}

/// A `WebM` file of a short sound and no video, made here.
fn sound_only_webm() -> Vec<u8> {
    start_gstreamer().unwrap();
    let pipeline = gst::parse::launch(
        "audiotestsrc num-buffers=10 ! opusenc ! webmmux streamable=true ! appsink name=out sync=false",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let out = pipeline.by_name("out").unwrap().downcast::<gst_app::AppSink>().unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut bytes = Vec::new();
    while let Ok(sample) = out.pull_sample() {
        bytes.extend_from_slice(&sample.buffer().unwrap().map_readable().unwrap());
    }
    pipeline.set_state(gst::State::Null).unwrap();
    bytes
}

#[test]
fn a_webm_with_no_video_ends_at_once() {
    let bytes = sound_only_webm();
    assert!(is_webm(&bytes));
    let started = Instant::now();
    assert_eq!(all_frames(&bytes).unwrap_err(), VideoFileError::Damaged);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn a_frame_size_the_stream_changes_to_past_the_limit_is_too_big() {
    let limits = Limits { max_side: 64, ..ANY };
    let decoding = decoding(&synthetic_webm(), &["videoconvert"], || limits.sink_caps(), 4).unwrap();
    let verdict = Verdict::default();
    link_video(&decoding, limits, &verdict);
    decoding.target.set_active(true).unwrap();
    let raw = |width: i32| {
        gst::event::Caps::new(
            &gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("width", width)
                .field("height", 64_i32)
                .build(),
        )
    };
    let _ = decoding.target.send_event(gst::event::StreamStart::new("video"));
    let _ = decoding.target.send_event(raw(64));
    assert_eq!(verdict.get(), None);
    let _ = decoding.target.send_event(raw(65));
    assert_eq!(verdict.get(), Some(VideoFileError::TooBig));
    let _ = decoding.pipeline.set_state(gst::State::Null);
}
