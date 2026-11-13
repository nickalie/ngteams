//! The test pattern a camera stand-in shows: an I420 frame with a bar that moves 8 pixels a frame, and the frame's
//! index written into its top rows so whoever receives the frames can count them, see them move and find a gap.
//!
//! The top 8 rows hold 34 blocks of 8 by 8 pixels: a white one and a black one (the sync), then the index's 32 bits,
//! the highest first (white for 1). A frame narrower than 272 pixels or lower than 16 carries no index.

use super::{PixelFormat, VideoFrame};

/// Luma of a set bit, the sync's white block and the bar.
const WHITE: u8 = 235;
/// Luma of a clear bit and the sync's black block.
const BLACK: u8 = 16;
/// Luma behind the bar.
const BACKGROUND: u8 = 64;
/// Neutral chroma.
const GREY: u8 = 128;
/// A block's side, in pixels.
const BLOCK: u32 = 8;
/// The sync's two blocks and the index's 32.
const BLOCKS: u32 = 34;
/// How far the bar moves each frame, and its width.
const BAR_STEP: u32 = 8;
const BAR_WIDTH: u32 = 16;

/// A test pattern of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestPattern {
    pub width: u32,
    pub height: u32,
}

impl TestPattern {
    /// The pattern of `width` by `height` (both even; an odd one is rounded down, and at least 2).
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: (width & !1).max(2),
            height: (height & !1).max(2),
        }
    }

    /// Whether frames of this size carry their index.
    #[must_use]
    pub fn carries_index(&self) -> bool {
        self.width >= BLOCK * BLOCKS && self.height >= 2 * BLOCK
    }

    /// Frame number `index`: I420, the stride its width.
    #[must_use]
    pub fn frame(&self, index: u32) -> VideoFrame {
        let (width, height) = (to_usize(self.width), to_usize(self.height));
        let mut data = vec![GREY; width * height * 3 / 2];
        let (luma, _) = data.split_at_mut(width * height);
        let bar_start = to_usize(index.wrapping_mul(BAR_STEP) % self.width);
        for (row, line) in luma.chunks_exact_mut(width).enumerate() {
            line.fill(BACKGROUND);
            let bar_end = (bar_start + to_usize(BAR_WIDTH)).min(width);
            line[bar_start..bar_end].fill(WHITE);
            if self.carries_index() && row < to_usize(BLOCK) {
                write_blocks(line, index);
            }
        }
        VideoFrame {
            width: self.width,
            height: self.height,
            format: PixelFormat::I420,
            stride: self.width,
            data,
        }
    }
}

/// One of the top rows: the sync blocks, then the index's bits.
fn write_blocks(line: &mut [u8], index: u32) {
    for block in 0..BLOCKS {
        let white = match block {
            0 => true,
            1 => false,
            bit => index >> (BLOCKS - 1 - bit) & 1 == 1,
        };
        let start = to_usize(block * BLOCK);
        line[start..start + to_usize(BLOCK)].fill(if white { WHITE } else { BLACK });
    }
}

/// The index a frame of the pattern carries, or `None` for a frame without one (too small, another picture, a
/// format whose luma isn't readable here). A packed RGB frame is read as the pattern's greys.
#[must_use]
pub fn index_of(frame: &VideoFrame) -> Option<u32> {
    if !TestPattern::new(frame.width, frame.height).carries_index() {
        return None;
    }
    let row = to_usize(BLOCK / 2) * to_usize(frame.stride);
    let luma = |x: u32| -> Option<u8> {
        let x = to_usize(x);
        let at = match frame.format {
            PixelFormat::I420 | PixelFormat::Nv12 => row + x,
            PixelFormat::Yuy2 => row + 2 * x,
            // a grey's green stands for its luma
            PixelFormat::Rgbx | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Bgra => row + 4 * x + 1,
            _ => return None,
        };
        frame.data.get(at).copied()
    };
    let bit = |block: u32| luma(block * BLOCK + BLOCK / 2).map(|value| value >= GREY);
    if bit(0)? && !bit(1)? {
        (2..BLOCKS).try_fold(0_u32, |index, block| Some(index << 1 | u32::from(bit(block)?)))
    } else {
        None
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
