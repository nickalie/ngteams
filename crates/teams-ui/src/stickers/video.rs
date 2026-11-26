//! Telegram's video stickers (`.webm`): VP9, its alpha in a second VP9 stream (512 by 512, at most 3 s, at most
//! 30 fps), made frames for a GIF.
//!
//! - **The decoder** is `GStreamer`'s, through [`teams_media::video_file`]: the app links it already for the chat's
//!   videos and the calls' sounds, on each platform, with the VP9 and alpha decoders of its usual plugins. A computer
//!   without them skips a video sticker with [`Skip::NoVideoDecoder`].
//! - **Each frame** is scaled down to the kept size ([`super::import::KEPT_SIDE`]) as it is decoded, so the whole video
//!   is never held at its own size. A frame in the same [`MAX_FPS`] slot as the frame before it is dropped: a GIF's
//!   frame time is in hundredths of a second, and browsers slow a faster frame down.
//! - **Each frame's time** is the time to the next kept frame, cut from the video's own clock in hundredths, so the
//!   whole loop keeps its length. The last frame lasts to the video's end: the end the file declares (the alpha
//!   decoder often gives no frame for the last block), at most [`MAX_LAST_FRAME`] past the frame.
//! - **What is read is capped**: each frame at [`super::import::MAX_SOURCE_SIDE`] on its longer side, the kept frames
//!   at [`super::import::MAX_FRAMES`], the decode at [`teams_media::video_file::DEADLINE`].

use std::ops::ControlFlow;
use std::time::Duration;

use image::RgbaImage;
use teams_media::video_file::{self, RgbaFrame, VideoFileError};

use super::import::{MAX_FRAMES, MAX_SOURCE_SIDE, Skip, kept};

pub use teams_media::video_file::is_webm;

/// The most frames a second a video sticker keeps.
pub const MAX_FPS: f64 = 30.0;
/// The longest the last frame is held to reach the end a file declares.
const MAX_LAST_FRAME: Duration = Duration::from_secs(1);
/// A frame's shortest time, in milliseconds (two hundredths: the shortest a browser plays as given).
const MIN_DELAY_MS: u32 = 20;

/// The frames kept so far, and why the read stopped early, if it did.
#[derive(Default)]
struct Timed {
    frames: Vec<(RgbaImage, Duration)>,
    last_slot: Option<u64>,
    end: Duration,
    stopped: Option<Skip>,
}

/// The [`MAX_FPS`] slot a frame shown at `at` falls in.
fn slot(at: Duration) -> u64 {
    // a frame at k / MAX_FPS s stays in slot k when its time is cut to milliseconds, as Matroska keeps it
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a short video's time, never negative"
    )]
    let slot = (at.as_secs_f64() * MAX_FPS + 0.05).floor() as u64;
    slot
}

/// The hundredths of a second in `at`, rounded.
fn hundredths(at: Duration) -> u64 {
    u64::try_from((at.as_micros() + 5_000) / 10_000).unwrap_or(u64::MAX)
}

impl Timed {
    fn take(&mut self, frame: RgbaFrame) -> ControlFlow<()> {
        if frame.width > MAX_SOURCE_SIDE || frame.height > MAX_SOURCE_SIDE {
            return self.stop(Skip::TooBig);
        }
        self.end = self.end.max(frame.at + frame.length.unwrap_or_default());
        let slot = slot(frame.at);
        if self.last_slot == Some(slot) {
            return ControlFlow::Continue(());
        }
        if self.frames.len() == MAX_FRAMES {
            return self.stop(Skip::TooManyFrames);
        }
        let Some(image) = RgbaImage::from_raw(frame.width, frame.height, frame.rgba) else {
            return self.stop(Skip::Unreadable);
        };
        self.frames.push((kept(image), frame.at));
        self.last_slot = Some(slot);
        ControlFlow::Continue(())
    }

    fn stop(&mut self, why: Skip) -> ControlFlow<()> {
        self.stopped = Some(why);
        ControlFlow::Break(())
    }

    /// The kept frames, each with its time in milliseconds, after a read that ended with `read` (the length the file
    /// declares).
    fn finish(self, read: Result<Option<Duration>, VideoFileError>) -> Result<Vec<(RgbaImage, u32)>, Skip> {
        if let Some(why) = self.stopped {
            return Err(why);
        }
        let declared = read.map_err(skip)?.unwrap_or_default();
        let last = self.frames.last().ok_or(Skip::Unreadable)?.1;
        let end = self.end.max(declared.min(last + MAX_LAST_FRAME));
        let end = if end > last {
            end
        } else {
            last + Duration::from_secs_f64(1.0 / MAX_FPS)
        };
        let starts: Vec<Duration> = self.frames.iter().map(|(_, at)| *at).collect();
        Ok(self
            .frames
            .into_iter()
            .enumerate()
            .map(|(index, (frame, at))| {
                let next = starts.get(index + 1).copied().unwrap_or(end);
                let ms = hundredths(next).saturating_sub(hundredths(at)).saturating_mul(10);
                (frame, u32::try_from(ms).unwrap_or(u32::MAX).max(MIN_DELAY_MS))
            })
            .collect())
    }
}

/// Why a decode failed, as a sticker's skip.
fn skip(error: VideoFileError) -> Skip {
    match error {
        VideoFileError::NotWebm => Skip::NotPicture,
        VideoFileError::NoDecoder => Skip::NoVideoDecoder,
        VideoFileError::Damaged | VideoFileError::TooSlow => Skip::Unreadable,
    }
}

/// The frames of the `.webm` `bytes` (see the module docs), each with its time in milliseconds.
///
/// # Errors
/// It is not a video this computer decodes, it is damaged, or it is too large.
pub fn frames(bytes: &[u8]) -> Result<Vec<(RgbaImage, u32)>, Skip> {
    let mut timed = Timed::default();
    let read = video_file::decode_webm(bytes, &mut |frame| timed.take(frame));
    timed.finish(read)
}

#[cfg(test)]
#[path = "video_tests.rs"]
mod tests;
