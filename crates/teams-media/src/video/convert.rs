//! Frames between the devices, the codec and a window: whatever raw format a camera hands over turned into the I420
//! the encoder takes ([`to_i420`], a Motion JPEG picture decoded first, scaled down by a whole factor to fit the size
//! the call sends), and a decoded I420
//! frame turned into the premultiplied BGRA a window draws ([`i420_to_bgra`]); also our I420 to and from libwebrtc's
//! buffer ([`to_buffer`], [`from_buffer`]). Colours as the camera's pictures and H.264's are written: BT.601, limited
//! range.

use libwebrtc::native::yuv_helper;
use libwebrtc::video_frame::{I420Buffer, VideoBuffer as _};

use crate::devices::{PixelFormat, VideoFrame};

/// The largest picture the call sends: a bigger camera frame is scaled down by a whole factor until it fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxSize {
    pub width: u32,
    pub height: u32,
}

impl MaxSize {
    /// 1280 by 720.
    pub const HD: Self = Self {
        width: 1280,
        height: 720,
    };
}

/// One pixel as luma and chroma.
#[derive(Clone, Copy)]
struct Yuv {
    y: u8,
    u: u8,
    v: u8,
}

/// `frame` as I420 (even sides, each plane's stride its width), scaled down by the smallest whole factor that fits
/// `max`; `None` for a frame whose data is shorter than its format and size need, that is empty, or a Motion JPEG
/// picture that doesn't decode.
#[must_use]
pub fn to_i420(frame: &VideoFrame, max: MaxSize) -> Option<VideoFrame> {
    if frame.format == PixelFormat::Mjpeg {
        return to_i420(&super::mjpeg::decode(frame)?, max);
    }
    if frame.width == 0 || frame.height == 0 || !holds(frame) {
        return None;
    }
    let factor = (1..=16)
        .find(|factor| frame.width / factor <= max.width && frame.height / factor <= max.height)
        .unwrap_or(16);
    let width = (frame.width / factor) & !1;
    let height = (frame.height / factor) & !1;
    if width == 0 || height == 0 {
        return None;
    }
    if factor == 1 && frame.format == PixelFormat::I420 && frame.stride == width && frame.width == width {
        let size = to_usize(width) * to_usize(height) * 3 / 2;
        return frame.data.get(..size).map(|data| VideoFrame {
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data: data.to_vec(),
        });
    }
    if matches!(
        frame.format,
        PixelFormat::Bgrx | PixelFormat::Bgra | PixelFormat::Rgbx | PixelFormat::Rgba | PixelFormat::Nv12
    ) {
        return by_libyuv(frame, factor, width, height);
    }
    let (w, h) = (to_usize(width), to_usize(height));
    let mut data = vec![0_u8; w * h * 3 / 2];
    let (luma, chroma) = data.split_at_mut(w * h);
    let (u_plane, v_plane) = chroma.split_at_mut(w * h / 4);
    for (row, line) in luma.chunks_exact_mut(w).enumerate() {
        for (column, out) in line.iter_mut().enumerate() {
            *out = pixel(frame, column * to_usize(factor), row * to_usize(factor)).y;
        }
    }
    for row in 0..h / 2 {
        for column in 0..w / 2 {
            let at = pixel(frame, column * 2 * to_usize(factor), row * 2 * to_usize(factor));
            u_plane[row * w / 2 + column] = at.u;
            v_plane[row * w / 2 + column] = at.v;
        }
    }
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    })
}

/// The three planes of an I420 picture that libyuv writes, with the stride of each.
struct Planes<'a> {
    y: (&'a mut [u8], u32),
    u: (&'a mut [u8], u32),
    v: (&'a mut [u8], u32),
}

/// `frame` (32-bit RGB or NV12, which [`holds`] its size) as I420 at `width` by `height`: libyuv converts its top-left
/// `width * factor` by `height * factor` pixels, then scales them down by `factor`. In an unoptimized debug build on a
/// loaded host, a Rust loop over the pixels of a 1280x800 screen took 0.4 s. The shared screen then went out at about
/// 2 frames a second.
fn by_libyuv(frame: &VideoFrame, factor: u32, width: u32, height: u32) -> Option<VideoFrame> {
    if factor > 1 {
        let (whole_width, whole_height) = (width.checked_mul(factor)?, height.checked_mul(factor)?);
        let mut whole = I420Buffer::new(whole_width, whole_height);
        let (stride_y, stride_u, stride_v) = whole.strides();
        let (luma, cb, cr) = whole.data_mut();
        let planes = Planes {
            y: (luma, stride_y),
            u: (cb, stride_u),
            v: (cr, stride_v),
        };
        libyuv_into(frame, planes, whole_width, whole_height)?;
        return from_buffer(&whole.scale(i32::try_from(width).ok()?, i32::try_from(height).ok()?));
    }
    let (columns, rows) = (to_usize(width), to_usize(height));
    let mut data = vec![0_u8; columns * rows * 3 / 2];
    let (luma, chroma) = data.split_at_mut(columns * rows);
    let (cb, cr) = chroma.split_at_mut(columns * rows / 4);
    let planes = Planes {
        y: (luma, width),
        u: (cb, width / 2),
        v: (cr, width / 2),
    };
    libyuv_into(frame, planes, width, height)?;
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    })
}

/// libyuv converts the top-left `width` by `height` pixels of `frame` (32-bit RGB or NV12) into `planes`, BT.601
/// limited range as [`from_rgb`]. `None` for another format.
fn libyuv_into(frame: &VideoFrame, planes: Planes<'_>, width: u32, height: u32) -> Option<()> {
    let (width, height) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
    let Planes {
        y: (y, stride_y),
        u: (u, stride_u),
        v: (v, stride_v),
    } = planes;
    match frame.format {
        // libyuv's ARGB is B, G, R, A in memory, and its ABGR is R, G, B, A
        PixelFormat::Bgrx | PixelFormat::Bgra => yuv_helper::argb_to_i420(
            &frame.data,
            frame.stride,
            y,
            stride_y,
            u,
            stride_u,
            v,
            stride_v,
            width,
            height,
        ),
        PixelFormat::Rgbx | PixelFormat::Rgba => yuv_helper::abgr_to_i420(
            &frame.data,
            frame.stride,
            y,
            stride_y,
            u,
            stride_u,
            v,
            stride_v,
            width,
            height,
        ),
        PixelFormat::Nv12 => {
            let (luma, chroma) = frame
                .data
                .split_at_checked(to_usize(frame.stride) * to_usize(frame.height))?;
            yuv_helper::nv12_to_i420(
                luma,
                frame.stride,
                chroma,
                frame.stride,
                y,
                stride_y,
                u,
                stride_u,
                v,
                stride_v,
                width,
                height,
            );
        }
        _ => return None,
    }
    Some(())
}

/// An I420 frame of ours (each plane's stride its width) as libwebrtc's buffer.
pub(crate) fn to_buffer(frame: &VideoFrame) -> I420Buffer {
    let mut buffer = I420Buffer::new(frame.width, frame.height);
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (width, height) = (to_usize(frame.width), to_usize(frame.height));
    let (chroma_width, chroma_height) = (width / 2, height / 2);
    let luma_size = width * height;
    let (y, u, v) = buffer.data_mut();
    copy_plane(&frame.data, width, y, to_usize(stride_y), width, height);
    copy_plane(
        frame.data.get(luma_size..).unwrap_or_default(),
        chroma_width,
        u,
        to_usize(stride_u),
        chroma_width,
        chroma_height,
    );
    copy_plane(
        frame.data.get(luma_size + luma_size / 4..).unwrap_or_default(),
        chroma_width,
        v,
        to_usize(stride_v),
        chroma_width,
        chroma_height,
    );
    buffer
}

/// Copies `rows` rows of `columns` bytes from `from` (rows `from_stride` apart) to `to` (rows `to_stride` apart).
fn copy_plane(from: &[u8], from_stride: usize, to: &mut [u8], to_stride: usize, columns: usize, rows: usize) {
    for row in 0..rows {
        let (Some(source), Some(target)) = (
            from.get(row * from_stride..row * from_stride + columns),
            to.get_mut(row * to_stride..row * to_stride + columns),
        ) else {
            return;
        };
        target.copy_from_slice(source);
    }
}

/// A decoded picture as our I420 (even sides, each plane's stride its width).
pub(crate) fn from_buffer(buffer: &I420Buffer) -> Option<VideoFrame> {
    let (width, height) = (buffer.width() & !1, buffer.height() & !1);
    if width == 0 || height == 0 {
        return None;
    }
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (from_y, from_u, from_v) = buffer.data();
    let (columns, rows) = (to_usize(width), to_usize(height));
    let mut data = vec![0_u8; columns * rows * 3 / 2];
    let (luma, chroma) = data.split_at_mut(columns * rows);
    let (u_plane, v_plane) = chroma.split_at_mut(columns * rows / 4);
    let (half_columns, half_rows) = (columns / 2, rows / 2);
    copy_plane(from_y, to_usize(stride_y), luma, columns, columns, rows);
    copy_plane(
        from_u,
        to_usize(stride_u),
        u_plane,
        half_columns,
        half_columns,
        half_rows,
    );
    copy_plane(
        from_v,
        to_usize(stride_v),
        v_plane,
        half_columns,
        half_columns,
        half_rows,
    );
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    })
}

/// Whether `frame`'s data is as long as its format, stride and height need.
fn holds(frame: &VideoFrame) -> bool {
    let (stride, height) = (to_usize(frame.stride), to_usize(frame.height));
    let needed = match frame.format {
        PixelFormat::I420 | PixelFormat::Nv12 => stride * height + stride * height.div_ceil(2),
        _ => stride * height,
    };
    frame.stride >= frame.width.saturating_mul(bytes_per_pixel(frame.format)) && frame.data.len() >= needed
}

/// Bytes a pixel takes in the first plane (one for the planar formats' luma; Motion JPEG is decoded before it is read).
fn bytes_per_pixel(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Mjpeg => 1,
        PixelFormat::Yuy2 => 2,
        PixelFormat::Rgb | PixelFormat::Bgr => 3,
        PixelFormat::Rgbx | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Bgra => 4,
    }
}

/// The pixel at `(column, row)` of `frame` (which [`holds`] its size).
fn pixel(frame: &VideoFrame, column: usize, row: usize) -> Yuv {
    let stride = to_usize(frame.stride);
    let data = &frame.data;
    let at = |index: usize| data.get(index).copied().unwrap_or(0);
    let luma_size = stride * to_usize(frame.height);
    match frame.format {
        PixelFormat::I420 => {
            let chroma = (row / 2) * (stride / 2) + column / 2;
            Yuv {
                y: at(row * stride + column),
                u: at(luma_size + chroma),
                v: at(luma_size + luma_size / 4 + chroma),
            }
        }
        PixelFormat::Nv12 => {
            let chroma = luma_size + (row / 2) * stride + (column / 2) * 2;
            Yuv {
                y: at(row * stride + column),
                u: at(chroma),
                v: at(chroma + 1),
            }
        }
        PixelFormat::Yuy2 => {
            let pair = row * stride + (column / 2) * 4;
            Yuv {
                y: at(pair + (column % 2) * 2),
                u: at(pair + 1),
                v: at(pair + 3),
            }
        }
        rgb => {
            let start = row * stride + column * to_usize(bytes_per_pixel(rgb));
            let (red, green, blue) = match rgb {
                PixelFormat::Rgbx | PixelFormat::Rgba | PixelFormat::Rgb => (at(start), at(start + 1), at(start + 2)),
                _ => (at(start + 2), at(start + 1), at(start)),
            };
            from_rgb(red, green, blue)
        }
    }
}

/// BT.601 limited range from RGB.
fn from_rgb(r: u8, g: u8, b: u8) -> Yuv {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    Yuv {
        y: clamp_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16),
        u: clamp_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128),
        v: clamp_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128),
    }
}

/// An I420 `frame` (even sides, each plane's stride its width, as [`to_i420`] and the decoder make it) as premultiplied
/// BGRA, rows `width * 4` bytes apart, opaque; `None` for another format, an empty or odd-sided frame, or one whose data
/// is too short. libyuv converts it: in the unoptimized debug and coverage builds, a Rust loop over the pixels of a
/// 720p frame takes about 70 ms, and the far end then shows a share at few frames a second.
#[must_use]
pub fn i420_to_bgra(frame: &VideoFrame) -> Option<Vec<u8>> {
    let (width, height) = (frame.width, frame.height);
    if frame.format != PixelFormat::I420
        || width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || !holds(frame)
    {
        return None;
    }
    let luma_size = to_usize(frame.stride) * to_usize(height);
    let (luma, chroma) = frame.data.split_at(luma_size);
    let (cb, cr) = chroma.split_at(luma_size / 4);
    let row = width.checked_mul(4)?;
    let mut out = vec![0_u8; to_usize(row) * to_usize(height)];
    // libyuv's ARGB is B, G, R, A in memory, from BT.601 limited range as `to_rgb`
    yuv_helper::i420_to_argb(
        luma,
        frame.stride,
        cb,
        frame.stride / 2,
        cr,
        frame.stride / 2,
        &mut out,
        row,
        i32::try_from(width).ok()?,
        i32::try_from(height).ok()?,
    );
    Some(out)
}

/// RGB from BT.601 limited range.
#[cfg(feature = "effects")]
pub(crate) fn to_rgb(luma: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let scaled = 298 * (i32::from(luma) - 16);
    let (cb, cr) = (i32::from(cb) - 128, i32::from(cr) - 128);
    (
        clamp_u8((scaled + 409 * cr + 128) >> 8),
        clamp_u8((scaled - 100 * cb - 208 * cr + 128) >> 8),
        clamp_u8((scaled + 516 * cb + 128) >> 8),
    )
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(format: PixelFormat, width: u32, height: u32, bytes: &[u8]) -> VideoFrame {
        let stride = width * bytes_per_pixel(format);
        let size = to_usize(stride) * to_usize(height);
        VideoFrame {
            width,
            height,
            format,
            stride,
            data: bytes.iter().copied().cycle().take(size).collect(),
        }
    }

    #[test]
    fn white_and_black_rgb_are_limited_range_luma() {
        let white = to_i420(&solid(PixelFormat::Bgrx, 4, 4, &[255, 255, 255, 0]), MaxSize::HD).unwrap();
        assert_eq!(white.data[0], 235);
        let black = to_i420(&solid(PixelFormat::Rgb, 4, 4, &[0, 0, 0]), MaxSize::HD).unwrap();
        assert_eq!(black.data[0], 16);
        assert_eq!(black.data[16], 128, "grey chroma");
    }

    /// A `width` by `height` picture in `format` (32-bit RGB or NV12) whose colour changes from each 2x2 block to the
    /// next, and the I420 of each pixel by the per-pixel formula.
    fn blocks(format: PixelFormat, width: u32, height: u32) -> (VideoFrame, Vec<Yuv>) {
        let (columns, rows) = (to_usize(width), to_usize(height));
        let rgb = |column: usize, row: usize| {
            let (x, y) = (column / 2, row / 2);
            (
                u8::try_from((x * 37 + y * 11) % 256).unwrap(),
                u8::try_from((x * 5 + y * 53) % 256).unwrap(),
                u8::try_from((x * 19 + y * 29 + 7) % 256).unwrap(),
            )
        };
        let expected: Vec<Yuv> = (0..rows)
            .flat_map(|row| (0..columns).map(move |column| (column, row)))
            .map(|(column, row)| {
                let (r, g, b) = rgb(column, row);
                from_rgb(r, g, b)
            })
            .collect();
        let data = match format {
            PixelFormat::Nv12 => {
                let mut data: Vec<u8> = expected.iter().map(|yuv| yuv.y).collect();
                for row in (0..rows).step_by(2) {
                    for column in (0..columns).step_by(2) {
                        let at = expected[row * columns + column];
                        data.extend([at.u, at.v]);
                    }
                }
                data
            }
            _ => (0..rows)
                .flat_map(|row| (0..columns).map(move |column| (column, row)))
                .flat_map(|(column, row)| {
                    let (r, g, b) = rgb(column, row);
                    match format {
                        PixelFormat::Rgbx | PixelFormat::Rgba => [r, g, b, 255],
                        _ => [b, g, r, 255],
                    }
                })
                .collect(),
        };
        let stride = width * bytes_per_pixel(format);
        let frame = VideoFrame {
            width,
            height,
            format,
            stride,
            data,
        };
        (frame, expected)
    }

    #[test]
    fn libyuv_converts_rgb_and_nv12_as_the_per_pixel_formula() {
        let (width, height) = (64, 32);
        for format in [
            PixelFormat::Bgrx,
            PixelFormat::Bgra,
            PixelFormat::Rgbx,
            PixelFormat::Rgba,
            PixelFormat::Nv12,
        ] {
            let (frame, expected) = blocks(format, width, height);
            let i420 = to_i420(&frame, MaxSize::HD).unwrap();
            assert_eq!((i420.width, i420.height, i420.stride), (width, height, width));
            let (columns, rows) = (to_usize(width), to_usize(height));
            let (luma, chroma) = i420.data.split_at(columns * rows);
            let (cb, cr) = chroma.split_at(columns * rows / 4);
            for (index, got) in luma.iter().enumerate() {
                assert!(got.abs_diff(expected[index].y) <= 1, "{format:?} luma {index}");
            }
            for (index, (got_u, got_v)) in cb.iter().zip(cr).enumerate() {
                let at = expected[(index / (columns / 2)) * 2 * columns + (index % (columns / 2)) * 2];
                assert!(
                    got_u.abs_diff(at.u) <= 2 && got_v.abs_diff(at.v) <= 2,
                    "{format:?} chroma {index}"
                );
            }
        }
    }

    #[test]
    fn an_rgb_or_nv12_frame_is_cut_to_even_sides_and_scaled_down_by_a_whole_factor() {
        let (odd, _) = blocks(PixelFormat::Bgrx, 5, 3);
        let odd = to_i420(&odd, MaxSize::HD).unwrap();
        assert_eq!((odd.width, odd.height, odd.data.len()), (4, 2, 12));
        for format in [PixelFormat::Bgrx, PixelFormat::Nv12] {
            let (frame, _) = blocks(format, 1920, 1080);
            let scaled = to_i420(&frame, MaxSize::HD).unwrap();
            assert_eq!((scaled.width, scaled.height), (960, 540), "{format:?}");
            assert_eq!(scaled.data.len(), 960 * 540 * 3 / 2, "{format:?}");
        }
        let grey = solid(PixelFormat::Rgbx, 1920 * 2, 1080 * 2, &[128, 128, 128, 0]);
        let scaled = to_i420(&grey, SCREEN).unwrap();
        assert_eq!((scaled.width, scaled.height), (1920, 1080));
        assert!(scaled.data[..1920 * 1080].iter().all(|y| y.abs_diff(126) <= 1));
        assert!(scaled.data[1920 * 1080..].iter().all(|c| c.abs_diff(128) <= 1));
    }

    /// The screen thread converts each frame of a shared screen. A slow conversion sends the share at few frames a
    /// second: the per-pixel loop took 0.4 s for this frame in a debug build on a loaded host.
    #[test]
    fn a_shared_screen_frame_converts_in_a_small_part_of_its_frame_time() {
        let screen = solid(PixelFormat::Bgrx, 1280, 800, &[30, 60, 90, 0, 200, 150, 100, 0]);
        let mut times: Vec<_> = (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                assert!(to_i420(&screen, SCREEN).is_some());
                started.elapsed()
            })
            .collect();
        times.sort();
        assert!(times[2] < std::time::Duration::from_millis(30), "{times:?}");
    }

    /// The largest picture of a shared screen, as the screen thread sends it.
    const SCREEN: MaxSize = MaxSize {
        width: 1920,
        height: 1080,
    };

    #[test]
    fn a_big_frame_is_scaled_down_by_a_whole_factor() {
        let frame = solid(PixelFormat::Yuy2, 1920, 1080, &[100, 128, 100, 128]);
        let scaled = to_i420(&frame, MaxSize::HD).unwrap();
        assert_eq!((scaled.width, scaled.height), (960, 540));
        assert_eq!(scaled.data.len(), 960 * 540 * 3 / 2);
        assert_eq!(scaled.data[0], 100);
    }

    #[test]
    fn i420_round_trips_through_bgra_near_its_colour() {
        let frame = solid(PixelFormat::Rgba, 8, 8, &[200, 40, 90, 255]);
        let i420 = to_i420(&frame, MaxSize::HD).unwrap();
        let bgra = i420_to_bgra(&i420).unwrap();
        assert_eq!(bgra.len(), 8 * 8 * 4);
        let (b, g, r, a) = (bgra[0], bgra[1], bgra[2], bgra[3]);
        assert!(
            r.abs_diff(200) <= 3 && g.abs_diff(40) <= 3 && b.abs_diff(90) <= 3,
            "{r} {g} {b}"
        );
        assert_eq!(a, 255);
    }

    #[test]
    fn an_odd_sided_or_empty_i420_frame_is_refused() {
        let i420 = |width: u32, height: u32| VideoFrame {
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data: vec![128; to_usize(width * height * 2)],
        };
        assert!(i420_to_bgra(&i420(3, 2)).is_none());
        assert!(i420_to_bgra(&i420(2, 3)).is_none());
        assert!(i420_to_bgra(&i420(0, 2)).is_none());
        assert!(i420_to_bgra(&i420(2, 0)).is_none());
        assert_eq!(i420_to_bgra(&i420(2, 2)).map(|bgra| bgra.len()), Some(16));
    }

    #[test]
    fn a_short_frame_is_refused() {
        let mut frame = solid(PixelFormat::Rgb, 8, 8, &[1, 2, 3]);
        frame.data.truncate(10);
        assert!(to_i420(&frame, MaxSize::HD).is_none());
        assert!(i420_to_bgra(&frame).is_none());
    }

    /// `jpeg` without its Huffman tables (`DHT` segments), as many UVC cameras send a Motion JPEG picture.
    fn without_huffman_tables(jpeg: &[u8]) -> Vec<u8> {
        let mut out = jpeg[..2].to_vec();
        let mut at = 2;
        while at + 4 <= jpeg.len() {
            let marker = jpeg[at + 1];
            let end = at + 2 + usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
            if marker == 0xda {
                out.extend_from_slice(&jpeg[at..]);
                break;
            }
            if marker != 0xc4 {
                out.extend_from_slice(&jpeg[at..end]);
            }
            at = end;
        }
        out
    }

    /// A 1280 by 720 picture of one colour, `(200, 40, 40)`, as JPEG.
    fn picture_jpeg() -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(1280, 720, image::Rgb([200, 40, 40]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&picture)
            .unwrap();
        jpeg
    }

    #[test]
    fn a_motion_jpeg_picture_is_decoded_then_converted_even_without_its_huffman_tables_and_dropped_when_cut_short() {
        let jpeg = picture_jpeg();
        let stripped = without_huffman_tables(&jpeg);
        assert!(stripped.len() < jpeg.len(), "the tables were taken out");
        let want = from_rgb(200, 40, 40);
        for data in [jpeg, stripped] {
            let frame = VideoFrame {
                width: 1280,
                height: 720,
                format: PixelFormat::Mjpeg,
                stride: 0,
                data,
            };
            let i420 = to_i420(&frame, MaxSize::HD).expect("decoded");
            assert_eq!((i420.width, i420.height, i420.format), (1280, 720, PixelFormat::I420));
            let (y, u, v) = (
                i420.data[640],
                i420.data[1280 * 720 + 10],
                i420.data[1280 * 720 * 5 / 4 + 10],
            );
            assert!(
                y.abs_diff(want.y) <= 3 && u.abs_diff(want.u) <= 3 && v.abs_diff(want.v) <= 3,
                "{y} {u} {v}"
            );
        }
        let mut cut = picture_jpeg();
        cut.truncate(cut.len() / 2);
        let cut = VideoFrame {
            width: 1280,
            height: 720,
            format: PixelFormat::Mjpeg,
            stride: 0,
            data: cut,
        };
        assert!(
            to_i420(&cut, MaxSize::HD).is_none(),
            "a picture cut short is dropped, not filled with grey"
        );
        let broken = VideoFrame {
            width: 1280,
            height: 720,
            format: PixelFormat::Mjpeg,
            stride: 0,
            data: b"not a picture".to_vec(),
        };
        assert!(to_i420(&broken, MaxSize::HD).is_none());
        assert_eq!(PixelFormat::Mjpeg.name(), "MJPG");
    }

    #[test]
    fn an_i420_frame_goes_to_libwebrtc_and_back_unchanged() {
        let (width, height) = (8, 4);
        let data: Vec<u8> = (0..width * height * 3 / 2)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let frame = VideoFrame {
            width: 8,
            height: 4,
            format: PixelFormat::I420,
            stride: 8,
            data,
        };
        assert_eq!(from_buffer(&to_buffer(&frame)), Some(frame));
    }
}
