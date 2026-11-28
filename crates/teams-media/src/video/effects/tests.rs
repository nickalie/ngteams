use std::time::{Duration, Instant};

use super::compose::Compositor;
use super::*;

/// An I420 frame `width` by `height`: luma a diagonal ramp, chroma grey.
fn ramp(width: u32, height: u32) -> VideoFrame {
    let (w, h) = (to_usize(width), to_usize(height));
    let mut data = vec![128_u8; w * h * 3 / 2];
    for (row, line) in data[..w * h].chunks_exact_mut(w).enumerate() {
        for (column, value) in line.iter_mut().enumerate() {
            *value = u8::try_from((column * 7 + row * 3) % 220 + 16).unwrap_or(0);
        }
    }
    VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    }
}

/// A mask of the model's size with the person on its left half.
fn left_half_person() -> Vec<u8> {
    (0..MODEL_HEIGHT * MODEL_WIDTH)
        .map(|index| if index % MODEL_WIDTH < MODEL_WIDTH / 2 { 255 } else { 0 })
        .collect()
}

#[test]
fn no_effect_leaves_the_frame_as_it_is() {
    let frame = ramp(64, 36);
    let mut effected = frame.clone();
    Effects::new().apply(&mut effected, &Background::None);
    assert_eq!(effected, frame);
}

#[test]
fn a_picture_replaces_the_background_and_keeps_the_person() {
    let frame = ramp(320, 180);
    let mut picture = frame.clone();
    picture.data.fill(200);
    let mut effected = frame.clone();
    Compositor::default().replace(&mut effected, &left_half_person(), &picture);
    let width = 320;
    let (person, background) = (40 * width + 20, 40 * width + 300);
    assert_eq!(effected.data[person], frame.data[person], "the person's pixel kept");
    assert_eq!(effected.data[background], 200, "the background's pixel replaced");
}

#[test]
fn blur_smooths_the_background_and_keeps_the_person() {
    let mut frame = ramp(320, 180);
    // a sharp checkerboard: the blur flattens it where it is the background
    for (index, value) in frame.data[..320 * 180].iter_mut().enumerate() {
        *value = if (index % 320 / 4 + index / 320 / 4) % 2 == 0 {
            40
        } else {
            220
        };
    }
    let mut effected = frame.clone();
    Compositor::default().blur(&mut effected, &left_half_person());
    let spread = |data: &[u8], from: usize| {
        let row = &data[90 * 320 + from..90 * 320 + from + 16];
        row.iter().max().copied().unwrap_or(0) - row.iter().min().copied().unwrap_or(0)
    };
    assert_eq!(
        spread(&effected.data, 20),
        spread(&frame.data, 20),
        "the person's side untouched"
    );
    assert!(
        spread(&effected.data, 280) < 30,
        "the background flattened: {}",
        spread(&effected.data, 280)
    );
}

#[test]
fn a_stock_picture_is_prepared_at_the_frame_size() {
    let picture = compose::backdrop_frame(&Backdrop::Stock(&STOCK[0]), 640, 360).unwrap();
    assert_eq!((picture.width, picture.height), (640, 360));
    assert_eq!(picture.data.len(), 640 * 360 * 3 / 2);
    let tall = compose::backdrop_frame(&Backdrop::Stock(&STOCK[1]), 360, 640).unwrap();
    assert_eq!((tall.width, tall.height), (360, 640));
}

#[test]
fn every_stock_picture_decodes() {
    for stock in &STOCK {
        let decoded = image::load_from_memory(stock.jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1280, 720), "{}", stock.key);
    }
    assert_eq!(super::stock("waves").map(|stock| stock.name), Some("Purple waves"));
    let keys: Vec<&str> = STOCK.iter().map(|stock| stock.key).collect();
    assert_eq!(keys, teams_media_types::STOCK_BACKGROUNDS);
    assert!(super::stock("nothing").is_none());
}

#[test]
fn a_missing_picture_blurs_instead() {
    let mut frame = ramp(160, 90);
    let before = frame.clone();
    let mut effects = Effects::new();
    effects.apply(
        &mut frame,
        &Background::Picture(Backdrop::File("/nonexistent/picture.jpg".into())),
    );
    assert_ne!(frame, before, "the frame was still changed: blurred");
}

#[test]
fn an_added_picture_is_scaled_down_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("big.png");
    image::RgbImage::from_pixel(3000, 1000, image::Rgb([10, 200, 30]))
        .save(&source)
        .unwrap();
    let kept = import(&source, &dir.path().join("added")).unwrap();
    let decoded = image::open(&kept).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1920, 640));
    assert_eq!(added(&dir.path().join("added")), vec![kept]);
    assert!(import(&dir.path().join("missing.png"), dir.path()).is_err());
}

#[test]
fn the_bundled_model_loads_and_gives_a_mask_of_its_size() {
    // without it every frame would be all background: the person blurred or replaced too
    let plan = segment::plan().unwrap();
    let mask = segment::Segment::segment(&plan, &segment::model_input(&ramp(1280, 720))).unwrap();
    assert_eq!(mask.len(), MODEL_WIDTH * MODEL_HEIGHT);
    assert!(mask.iter().all(|value| value.is_finite()), "confidences");
}

#[test]
fn the_edge_is_refined_at_the_frame_shape_scaled_to_fit() {
    assert_eq!(edge::work_size(1280, 720), (512, 288));
    assert_eq!(edge::work_size(1920, 1080), (512, 288));
    assert_eq!(edge::work_size(640, 480), (384, 288));
    assert_eq!(edge::work_size(720, 1280), (162, 288));
    assert_eq!(edge::work_size(320, 180), (320, 180), "never scaled up");
    assert_eq!(edge::work_size(1, 1), (2, 2));
    assert_eq!(edge::work_size(0, 0), (2, 2));
}

#[test]
fn a_window_mean_keeps_a_flat_plane_and_spreads_a_spike_evenly() {
    let mut scratch = Vec::new();
    let mut flat = vec![0.25_f32; 12 * 7];
    edge::box_mean(&mut flat, (12, 7), 2, &mut scratch);
    assert!(flat.iter().all(|value| (value - 0.25).abs() < 1e-6), "{flat:?}");
    let mut spike = vec![0.0_f32; 11 * 11];
    spike[5 * 11 + 5] = 25.0;
    edge::box_mean(&mut spike, (11, 11), 2, &mut scratch);
    for y in 0..11_usize {
        for x in 0..11_usize {
            let inside = x.abs_diff(5) <= 2 && y.abs_diff(5) <= 2;
            let want = if inside { 1.0 } else { 0.0 };
            assert!(
                (spike[y * 11 + x] - want).abs() < 1e-5,
                "({x}, {y}): {}",
                spike[y * 11 + x]
            );
        }
    }
    // at the edges the window is clipped, its mean over the pixels it holds
    let mut ramp: Vec<f32> = (0..6_u8).map(f32::from).collect();
    edge::box_mean(&mut ramp, (6, 1), 1, &mut scratch);
    assert_eq!(ramp, vec![0.5, 1.0, 2.0, 3.0, 4.0, 4.5]);
    // a plane shorter than its size is left alone
    let mut short = vec![1.0_f32; 3];
    edge::box_mean(&mut short, (4, 4), 1, &mut scratch);
    assert_eq!(short, vec![1.0; 3]);
}

/// Whether two values are the same but for rounding.
fn close(one: f32, other: f32) -> bool {
    (one - other).abs() < 1e-6
}

#[test]
fn smoothstep_eases_from_low_to_high() {
    assert!(close(edge::smoothstep(0.2, 0.8, 0.0), 0.0));
    assert!(close(edge::smoothstep(0.2, 0.8, 0.5), 0.5));
    assert!(close(edge::smoothstep(0.2, 0.8, 1.0), 1.0));
    assert!(edge::smoothstep(0.2, 0.8, 0.3) < 0.1);
}

/// A frame `width` by `height` whose luma is bright left of `edge` and dark right of it (the person's side and the
/// room's).
fn split_frame(width: u32, height: u32, edge: usize) -> VideoFrame {
    let mut frame = ramp(width, height);
    let w = to_usize(width);
    for (index, value) in frame.data[..w * to_usize(height)].iter_mut().enumerate() {
        *value = if index % w < edge { 200 } else { 40 };
    }
    frame
}

#[test]
fn the_soft_edge_follows_the_frame_and_fades_over_a_few_pixels() {
    // the model's edge at the frame's middle (640), the frame's own edge 8 px further right
    let (width, height) = (1280_usize, 720_usize);
    let frame = split_frame(1280, 720, 648);
    let mut refiner = edge::EdgeRefiner::default();
    let (alpha, work) = refiner.refine(&left_half_person(), &frame.data[..width * height], width, height);
    assert_eq!(work, (512, 288));
    let row: Vec<u8> = alpha[144 * 512..145 * 512].to_vec();
    assert_eq!(row[0], 255, "deep inside the person");
    assert_eq!(row[511], 0, "far out in the room");
    assert!(row.windows(2).all(|pair| pair[0] >= pair[1]), "falls off once: {row:?}");
    // the half-way point moved from the model's edge (256, as a flat frame leaves it) toward the frame's (259.2)
    let half = row.iter().position(|&value| value < 128).unwrap();
    assert!((257..=260).contains(&half), "half-way at {half}");
    // soft: some pixels between sure and not, but only a narrow band of them
    let between = row.iter().filter(|&&value| value > 8 && value < 247).count();
    assert!((3..=16).contains(&between), "{between} pixels in the fade: {row:?}");
}

#[test]
fn a_flat_frame_keeps_the_model_edge_and_softens_it() {
    let (width, height) = (640_usize, 360_usize);
    let mut frame = ramp(640, 360);
    frame.data[..width * height].fill(120);
    let mut refiner = edge::EdgeRefiner::default();
    let (alpha, work) = refiner.refine(&left_half_person(), &frame.data[..width * height], width, height);
    assert_eq!(work, (512, 288));
    let row = &alpha[100 * 512..101 * 512];
    let half = row.iter().position(|&value| value < 128).unwrap();
    assert!((254..=258).contains(&half), "half-way at {half}");
    let between = row.iter().filter(|&&value| value > 0 && value < 255).count();
    assert!(between >= 4, "soft: {:?}", &row[244..268]);
}

#[test]
fn the_mask_follows_motion_at_once_and_smooths_flicker() {
    // a small change (flicker) is taken only in part
    let flicker = segment::follow(0.5, 0.55);
    assert!(flicker > 0.5 && flicker < 0.52, "{flicker}");
    // a large one (the person moved) all the way
    assert!((segment::follow(0.0, 1.0) - 1.0).abs() < 1e-6);
    assert!(segment::follow(1.0, 0.0).abs() < 1e-6);
    // and nothing moves where nothing changed
    assert!(close(segment::follow(0.3, 0.3), 0.3));
}

/// A runtime that gives the same confidence everywhere.
struct Fixed(f32);

impl segment::Segment for Fixed {
    fn segment(&self, _: &[f32]) -> Result<Vec<f32>, String> {
        Ok(vec![self.0; MODEL_WIDTH * MODEL_HEIGHT])
    }
}

/// A runtime that fails, as a lost GPU does.
struct Lost;

impl segment::Segment for Lost {
    fn segment(&self, _: &[f32]) -> Result<Vec<f32>, String> {
        Err("the device was lost".into())
    }
}

#[test]
fn a_run_takes_the_gpu_while_it_works_and_the_cpu_once_it_fails() {
    let input = segment::model_input(&ramp(320, 180));
    let cpu = segment::plan();
    let mut gpu: Option<segment::Shared> = Some(std::sync::Arc::new(Fixed(0.25)));
    let (confidence, failed) = segment::run_once(&mut gpu, &cpu, &input);
    assert_eq!(confidence.unwrap(), vec![0.25; MODEL_WIDTH * MODEL_HEIGHT]);
    assert!(failed.is_none() && gpu.is_some(), "the GPU kept");
    let mut gpu: Option<segment::Shared> = Some(std::sync::Arc::new(Lost));
    let (confidence, failed) = segment::run_once(&mut gpu, &cpu, &input);
    let on_cpu = segment::Segment::segment(cpu.as_ref().unwrap(), &input).unwrap();
    assert_eq!(confidence.unwrap(), on_cpu, "the CPU ran the same frame");
    assert_eq!(failed.as_deref(), Some("the device was lost"));
    assert!(gpu.is_none(), "the GPU let go");
    let (confidence, failed) = segment::run_once(&mut gpu, &Err("no model".into()), &input);
    assert_eq!(confidence, Err("no model".into()));
    assert!(failed.is_none());
}

#[test]
fn the_gpu_gives_the_cpu_mask_where_there_is_one() {
    // the first ask starts the GPU's setup on its own thread; until it settles the CPU runs
    let _ = gpu::ready();
    let until = Instant::now() + Duration::from_secs(600);
    while gpu::preparing() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!gpu::preparing(), "the GPU's setup settled");
    // no adapter at all (not even a software one) leaves only the CPU, which the other tests cover
    if let Some(on_gpu) = gpu::ready() {
        println!("on {on_gpu:?}");
        assert!(format!("{on_gpu:?}").starts_with("Gpu { name: "));
        let input = segment::model_input(&ramp(1280, 720));
        let on_cpu = segment::Segment::segment(&segment::plan().unwrap(), &input).unwrap();
        let mask = segment::Segment::segment(on_gpu.as_ref(), &input).unwrap();
        assert!(
            gpu::farthest(&mask, &on_cpu) < 0.01,
            "{}",
            gpu::farthest(&mask, &on_cpu)
        );
    }
}

#[test]
fn an_adapter_that_isnt_there_is_said_so() {
    let error = gpu::prepare(&[]).unwrap_err();
    assert!(error.starts_with("no GPU"), "{error}");
}

#[test]
fn masks_apart_and_panics_caught() {
    assert!(close(gpu::farthest(&[0.0, 0.5], &[0.25, 0.5]), 0.25));
    assert!(gpu::farthest(&[0.0], &[0.0, 1.0]).is_infinite());
    assert_eq!(gpu::caught(|| 7), Ok(7));
    assert_eq!(
        gpu::caught(|| panic!("a lost device")),
        Err::<(), _>("a lost device".into())
    );
    let reason = String::from("lost");
    assert_eq!(gpu::caught(|| panic!("{reason}")), Err::<(), _>("lost".into()));
    assert_eq!(
        gpu::caught(|| std::panic::panic_any(7_u8)),
        Err::<(), _>("it panicked".into())
    );
}

#[test]
fn a_picture_of_an_odd_shape_is_cropped_before_it_is_scaled() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("thin.png");
    image::RgbImage::from_pixel(1920, 1, image::Rgb([10, 200, 30]))
        .save(&source)
        .unwrap();
    let picture = compose::backdrop_frame(&Backdrop::File(source), 1280, 720).unwrap();
    assert_eq!((picture.width, picture.height), (1280, 720));
}

/// What a 720p frame costs: the model's run (on its own thread) and the camera thread's part (the frame scaled for
/// the model and put together with each effect). Timed in a release build (`cargo test --release -p teams-media
/// effects -- --ignored --nocapture`): the debug build's numbers say nothing.
#[test]
#[ignore = "a timing: run in a release build"]
fn a_720p_frame_costs_milliseconds() {
    let source = ramp(1280, 720);
    let plan = segment::plan().unwrap();
    let input = segment::model_input(&source);
    let model = median(|| {
        segment::Segment::segment(&plan, &input).unwrap();
    });
    println!("the model on the CPU: median {model:?}");
    let _ = gpu::ready();
    while gpu::preparing() {
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Some(on_gpu) = gpu::ready() {
        let on = median(|| {
            segment::Segment::segment(on_gpu.as_ref(), &input).unwrap();
        });
        println!("the model on {on_gpu:?}: median {on:?}");
    }
    let scaled = median(|| {
        segment::model_input(&source);
    });
    println!("the frame scaled for the model: median {scaled:?}");
    let mut effects = Effects::new();
    for background in [Background::Blur, Background::Picture(Backdrop::Stock(&STOCK[2]))] {
        let mut frame = source.clone();
        effects.apply(&mut frame, &background);
        effects.segmenter.settle(&source, Duration::from_secs(5));
        let cost = median(|| {
            let mut frame = source.clone();
            effects.apply(&mut frame, &background);
        });
        println!("the camera thread with {background:?}: median {cost:?}");
        assert!(cost < Duration::from_millis(33), "a frame takes {cost:?}");
    }
    assert!(model < Duration::from_millis(66), "the model takes {model:?}");
}

/// The median of 60 runs of `run`, after 5 to warm up.
fn median(mut run: impl FnMut()) -> Duration {
    for _ in 0..5 {
        run();
    }
    let mut costs: Vec<Duration> = (0..60)
        .map(|_| {
            let start = Instant::now();
            run();
            start.elapsed()
        })
        .collect();
    costs.sort();
    costs[30]
}

/// The effects on pictures of a person (`TEAMS_EFFECTS_SAMPLE`, any pictures, `:` between them), written as PNGs
/// named after each under `TEAMS_EFFECTS_OUT`, for a look.
#[test]
#[ignore = "writes pictures for a look"]
fn effects_on_a_sample_picture() {
    let (Ok(samples), Ok(out)) = (
        std::env::var("TEAMS_EFFECTS_SAMPLE"),
        std::env::var("TEAMS_EFFECTS_OUT"),
    ) else {
        return;
    };
    for sample in std::env::split_paths(&samples) {
        let stem = sample.file_stem().unwrap().to_string_lossy().into_owned();
        let picture = compose::backdrop_frame(&Backdrop::File(sample), 1280, 720).unwrap();
        let mut effects = Effects::new();
        for (name, background) in [
            ("blur", Background::Blur),
            ("picture", Background::Picture(Backdrop::Stock(&STOCK[0]))),
        ] {
            effects.segmenter.settle(&picture, Duration::from_secs(5));
            let mut frame = picture.clone();
            effects.apply(&mut frame, &background);
            let bgra = crate::video::convert::i420_to_bgra(&frame).unwrap();
            let rgba: Vec<u8> = bgra
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[b, g, r, _]| [r, g, b, 255])
                .collect();
            image::RgbaImage::from_raw(1280, 720, rgba)
                .unwrap()
                .save(std::path::Path::new(&out).join(format!("{stem}-{name}.png")))
                .unwrap();
        }
    }
}
