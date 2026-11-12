//! M0 Task 16: the synthetic video source and the benchmark's statistics, without a compositor.
#![cfg(target_os = "linux")]

use std::time::Duration;

use gpui_video::{
    bench::{
        FieldError, ShowTracker, Stats, cpu_percent, fdinfo_gpu_memory, nvidia_smi_memory, parse_debug_duration,
        parse_fields, parse_frame_duration, parse_schedstat_runtime, per_second,
    },
    frames::{
        COUNTER_MAX, FrameError, FrameGenerator, HEIGHT, Nv12Frame, Rect, WIDTH, bgra_luma, nv12_to_bgra, read_counter,
        read_counter_bgra,
    },
};

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn us(micros: u64) -> Duration {
    Duration::from_micros(micros)
}

// --- frame generator --------------------------------------------------------------------------------------------

#[test]
fn nv12_frame_has_720p_planes() {
    let generator = FrameGenerator::new(0, WIDTH, HEIGHT).unwrap();
    let frame = generator.nv12(7);
    assert_eq!((frame.width, frame.height), (1280, 720));
    assert_eq!(frame.y.len(), 1280 * 720);
    // 4:2:0: one interleaved U/V pair per 2×2 block
    assert_eq!(frame.uv.len(), 1280 * 720 / 2);
}

#[test]
fn bgra_frame_is_four_bytes_a_pixel_and_opaque() {
    let generator = FrameGenerator::new(3, WIDTH, HEIGHT).unwrap();
    let bgra = generator.bgra(42);
    assert_eq!(bgra.len(), 1280 * 720 * 4);
    assert!(bgra.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == u8::MAX));
}

#[test]
fn counter_reads_back_from_nv12_and_bgra() {
    for stream in 0..9 {
        let generator = FrameGenerator::new(stream, WIDTH, HEIGHT).unwrap();
        // every stream's colour with a few values; every value on one stream
        let counters: &[u32] = if stream == 0 {
            &[0, 1, 2, 29, 30, 1000, 65_535, 123_456, COUNTER_MAX]
        } else {
            &[0, 123_456, COUNTER_MAX]
        };
        for &counter in counters {
            let frame = generator.nv12(counter);
            let full = Rect::new(0, 0, WIDTH, HEIGHT);
            let from_luma = read_counter(full, |x, y| frame.y.get((y * WIDTH + x) as usize).copied());
            assert_eq!(from_luma, Some(counter), "stream {stream}, NV12 luma");
            let bgra = generator.bgra(counter);
            assert_eq!(
                read_counter_bgra(&bgra, WIDTH, HEIGHT),
                Some(counter),
                "stream {stream}, BGRA"
            );
        }
    }
}

#[test]
fn counter_wraps_above_its_range() {
    let generator = FrameGenerator::new(0, WIDTH, HEIGHT).unwrap();
    let bgra = generator.bgra(COUNTER_MAX + 1);
    assert_eq!(read_counter_bgra(&bgra, WIDTH, HEIGHT), Some(0));
}

/// The mean luma of the `factor`×`factor` block of a 720p BGRA frame whose top-left is (`x`, `y`).
fn block_luma(bgra: &[u8], x: u32, y: u32, factor: u32) -> u8 {
    let sum: u32 = (0..factor * factor)
        .map(|offset| {
            let index = (((y + offset / factor) * WIDTH + x + offset % factor) * 4) as usize;
            u32::from(bgra_luma(&bgra[index..index + 4]))
        })
        .sum();
    u8::try_from(sum / (factor * factor)).unwrap_or(u8::MAX)
}

/// A box-filtered downscale of a BGRA frame by `factor`, then the counter read from a sub-rectangle of a larger
/// image: the way a 3×3 grid tile shows up in a screenshot.
#[test]
fn counter_reads_back_from_a_scaled_tile_inside_a_bigger_image() {
    let generator = FrameGenerator::new(5, WIDTH, HEIGHT).unwrap();
    let bgra = generator.bgra(98_765);
    for factor in [2, 3] {
        let (tile_w, tile_h) = (WIDTH / factor, HEIGHT / factor);
        let (offset_x, offset_y) = (37, 11);
        let image_w = tile_w + offset_x + 5;
        let image_h = tile_h + offset_y + 5;
        let mut luma = vec![0_u8; (image_w * image_h) as usize];
        for ty in 0..tile_h {
            for tx in 0..tile_w {
                luma[((ty + offset_y) * image_w + tx + offset_x) as usize] =
                    block_luma(&bgra, tx * factor, ty * factor, factor);
            }
        }
        let rect = Rect::new(offset_x, offset_y, tile_w, tile_h);
        let counter = read_counter(rect, |x, y| luma.get((y * image_w + x) as usize).copied());
        assert_eq!(counter, Some(98_765), "downscaled by {factor}");
    }
}

#[test]
fn a_frame_without_a_counter_reads_as_none() {
    let grey = vec![0x80_u8; (WIDTH * HEIGHT * 4) as usize];
    assert_eq!(read_counter_bgra(&grey, WIDTH, HEIGHT), None);
    let black = vec![0_u8; (WIDTH * HEIGHT * 4) as usize];
    assert_eq!(read_counter_bgra(&black, WIDTH, HEIGHT), None);
    // a rectangle reaching outside the image
    let generator = FrameGenerator::new(0, WIDTH, HEIGHT).unwrap();
    let frame = generator.nv12(5);
    let outside = Rect::new(WIDTH, 0, WIDTH, HEIGHT);
    assert_eq!(
        read_counter(outside, |x, y| frame
            .y
            .get((y * WIDTH + x) as usize)
            .copied()
            .filter(|_| x < WIDTH)),
        None
    );
}

#[test]
fn a_damaged_counter_is_refused_not_misread() {
    let generator = FrameGenerator::new(1, WIDTH, HEIGHT).unwrap();
    let mut frame = generator.nv12(0b1010_1010);
    // paint over one data cell: the complement row no longer matches
    for y in 40..80 {
        for x in 40..80 {
            let index = (y * WIDTH + x) as usize;
            frame.y[index] = if frame.y[index] > 128 { 16 } else { 235 };
        }
    }
    let full = Rect::new(0, 0, WIDTH, HEIGHT);
    assert_eq!(
        read_counter(full, |x, y| frame.y.get((y * WIDTH + x) as usize).copied()),
        None
    );
}

#[test]
fn frames_change_between_counters_and_streams() {
    let first = FrameGenerator::new(0, WIDTH, HEIGHT).unwrap();
    let second = FrameGenerator::new(1, WIDTH, HEIGHT).unwrap();
    let a = first.nv12(10);
    let b = first.nv12(11);
    // the moving bar below the counter changes from frame to frame
    let below_counter = (200 * WIDTH) as usize;
    assert_ne!(a.y[below_counter..], b.y[below_counter..]);
    // streams are told apart by their colour
    assert_ne!(first.nv12(10).uv, second.nv12(10).uv);
}

#[test]
fn nv12_into_reuses_the_buffer_and_matches_nv12() {
    let generator = FrameGenerator::new(2, WIDTH, HEIGHT).unwrap();
    let mut frame = generator.nv12(1);
    generator.nv12_into(77, &mut frame);
    let fresh = generator.nv12(77);
    assert_eq!(frame.y, fresh.y);
    assert_eq!(frame.uv, fresh.uv);
}

#[test]
fn nv12_to_bgra_uses_bt601_limited_range() {
    let frame = Nv12Frame {
        width: 2,
        height: 2,
        y: vec![16, 235, 126, 81],
        uv: vec![128, 128],
    };
    let mut bgra = Vec::new();
    nv12_to_bgra(&frame, &mut bgra);
    assert_eq!(bgra.len(), 16);
    assert_eq!(&bgra[0..4], &[0, 0, 0, 255], "video black");
    assert_eq!(&bgra[4..8], &[255, 255, 255, 255], "video white");
    // mid grey stays grey
    assert_eq!(bgra[8], bgra[9]);
    assert_eq!(bgra[9], bgra[10]);
    // pure red in BT.601 limited: Y 81, U 90, V 240
    let red = Nv12Frame {
        width: 2,
        height: 2,
        y: vec![81; 4],
        uv: vec![90, 240],
    };
    nv12_to_bgra(&red, &mut bgra);
    let [b, g, r, a] = [bgra[0], bgra[1], bgra[2], bgra[3]];
    assert!(r >= 250 && g <= 5 && b <= 5 && a == 255, "red came out as {r},{g},{b}");
}

#[test]
fn generator_refuses_odd_or_too_small_sizes() {
    assert_eq!(
        FrameGenerator::new(0, 1281, 720).err(),
        Some(FrameError::OddSize {
            width: 1281,
            height: 720
        })
    );
    assert!(matches!(
        FrameGenerator::new(0, 320, 180),
        Err(FrameError::TooSmall { .. })
    ));
}

// --- statistics --------------------------------------------------------------------------------------------------

#[test]
fn stats_use_nearest_rank_percentiles() {
    let samples: Vec<Duration> = (1..=100).map(ms).collect();
    let stats = Stats::of(&samples).unwrap();
    assert_eq!(stats.count, 100);
    assert_eq!(stats.p50, ms(50));
    assert_eq!(stats.p95, ms(95));
    assert_eq!(stats.max, ms(100));
    assert_eq!(stats.mean, us(50_500));
}

#[test]
fn stats_do_not_depend_on_sample_order() {
    let mut samples = vec![ms(30), ms(10), ms(20), ms(40), ms(1000)];
    let stats = Stats::of(&samples).unwrap();
    samples.reverse();
    assert_eq!(Stats::of(&samples).unwrap(), stats);
    // 5 samples: p50 is the 3rd, p95 the 5th
    assert_eq!(stats.p50, ms(30));
    assert_eq!(stats.p95, ms(1000));
}

#[test]
fn stats_of_one_and_of_none() {
    let one = Stats::of(&[ms(7)]).unwrap();
    assert_eq!((one.p50, one.p95, one.max, one.mean), (ms(7), ms(7), ms(7), ms(7)));
    assert_eq!(Stats::of(&[]), None);
}

#[test]
fn rates_per_second() {
    assert!((per_second(300, Duration::from_secs(10)) - 30.0).abs() < 1e-9);
    assert!((per_second(1, ms(500)) - 2.0).abs() < 1e-9);
    assert!(per_second(5, Duration::ZERO).abs() < f64::EPSILON);
    assert!((cpu_percent(ms(250), ms(1000)) - 25.0).abs() < 1e-9);
    assert!((cpu_percent(ms(3000), ms(1000)) - 300.0).abs() < 1e-9);
    assert!(cpu_percent(ms(1), Duration::ZERO).abs() < f64::EPSILON);
}

#[test]
fn show_tracker_counts_shown_and_skipped_frames() {
    let mut tracker = ShowTracker::new(2);
    tracker.show(0, 10, ms(5));
    tracker.show(1, 3, ms(7));
    // the same frame again is not a new frame
    tracker.show(0, 10, ms(40));
    // frames 11 and 12 never reached the screen
    tracker.show(0, 13, ms(9));
    tracker.show(1, 4, ms(6));
    assert_eq!(tracker.shown(), 4);
    assert_eq!(tracker.skipped(), 2);
    assert_eq!(tracker.last(), &[Some(13), Some(4)]);
    let mut latencies = tracker.latencies().to_vec();
    latencies.sort();
    assert_eq!(latencies, vec![ms(5), ms(6), ms(7), ms(9)]);
    // out of range tiles are ignored
    tracker.show(9, 1, ms(1));
    assert_eq!(tracker.shown(), 4);
}

#[test]
fn show_tracker_reset_keeps_the_last_frames() {
    let mut tracker = ShowTracker::new(1);
    tracker.show(0, 5, ms(1));
    tracker.reset_counts();
    assert_eq!((tracker.shown(), tracker.skipped()), (0, 0));
    assert!(tracker.latencies().is_empty());
    // 6 follows 5: nothing skipped across the reset
    tracker.show(0, 6, ms(1));
    assert_eq!((tracker.shown(), tracker.skipped()), (1, 0));
}

// --- parsers -----------------------------------------------------------------------------------------------------

#[test]
fn debug_durations_parse() {
    assert_eq!(parse_debug_duration("4.5ms"), Some(us(4500)));
    assert_eq!(parse_debug_duration("812.25µs"), Some(Duration::from_nanos(812_250)));
    assert_eq!(parse_debug_duration("1.25s"), Some(ms(1250)));
    assert_eq!(parse_debug_duration("15ns"), Some(Duration::from_nanos(15)));
    assert_eq!(parse_debug_duration("12ms"), Some(ms(12)));
    assert_eq!(parse_debug_duration("ms"), None);
    assert_eq!(parse_debug_duration("-1ms"), None);
    assert_eq!(parse_debug_duration("3 parsecs"), None);
}

#[test]
fn gpui_frame_duration_lines_parse() {
    assert_eq!(parse_frame_duration("frame duration: 3.2ms"), Some(us(3200)));
    assert_eq!(parse_frame_duration("frame duration: 950µs"), Some(us(950)));
    assert_eq!(parse_frame_duration("[INFO gpui] frame duration: 1ms"), None);
    assert_eq!(parse_frame_duration("something else"), None);
}

#[test]
fn summary_fields_parse() {
    let fields = parse_fields("summary draws=600 elapsed_ms=20000 last=1,2,3").unwrap();
    assert_eq!(fields.get("draws").map(String::as_str), Some("600"));
    assert_eq!(fields.get("last").map(String::as_str), Some("1,2,3"));
    assert_eq!(fields.number::<u64>("elapsed_ms").unwrap(), 20_000);
    assert_eq!(
        fields.number::<u64>("missing").unwrap_err(),
        FieldError::Missing("missing".to_owned())
    );
    assert!(matches!(fields.number::<u64>("last"), Err(FieldError::Invalid { .. })));
    assert!(parse_fields("ready compositor=Wayland").is_none());
    assert!(parse_fields("summary broken").is_none());
}

#[test]
fn schedstat_runtime_parses() {
    assert_eq!(
        parse_schedstat_runtime("123456789 5000 42\n"),
        Some(Duration::from_nanos(123_456_789))
    );
    assert_eq!(parse_schedstat_runtime(""), None);
    assert_eq!(parse_schedstat_runtime("x 1 2"), None);
}

#[test]
fn drm_fdinfo_memory_sums_resident_regions() {
    let i915 = "pos:\t0\nflags:\t02100002\ndrm-driver:\ti915\ndrm-client-id:\t7\n\
                drm-total-system0:\t34816 KiB\ndrm-shared-system0:\t0\ndrm-resident-system0:\t34816 KiB\n\
                drm-resident-stolen-system0:\t0\ndrm-engine-render:\t1 ns\n";
    assert_eq!(fdinfo_gpu_memory(i915), Some(("i915".to_owned(), 7, 34_816 * 1024)));
    let xe = "drm-driver:\txe\ndrm-client-id:\t3\ndrm-resident-gtt:\t2 MiB\ndrm-resident-vram0:\t512 KiB\n";
    assert_eq!(
        fdinfo_gpu_memory(xe),
        Some(("xe".to_owned(), 3, 2 * 1024 * 1024 + 512 * 1024))
    );
    assert_eq!(fdinfo_gpu_memory("pos:\t0\nflags:\t0\n"), None);
}

#[test]
fn nvidia_smi_process_memory_parses() {
    let output = "GPU 00000000:01:00.0\n    Processes\n        GPU instance ID                   : N/A\n\
                  \x20       Process ID                        : 1384\n\
                  \x20           Type                          : G\n\
                  \x20           Name                          : niri\n\
                  \x20           Used GPU Memory               : 2 MiB\n\
                  \x20       Process ID                        : 1234\n\
                  \x20           Type                          : G\n\
                  \x20           Used GPU Memory               : 312 MiB\n";
    assert_eq!(nvidia_smi_memory(output, 1234), Some(312 * 1024 * 1024));
    assert_eq!(nvidia_smi_memory(output, 1384), Some(2 * 1024 * 1024));
    assert_eq!(nvidia_smi_memory(output, 7), None);
    let unknown = "        Process ID                        : 1234\n            Used GPU Memory               : N/A\n";
    assert_eq!(nvidia_smi_memory(unknown, 1234), None);
}
