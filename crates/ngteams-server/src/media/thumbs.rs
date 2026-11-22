//! A file's thumbnails, for its card: made from an image file the first time they are asked for, in Graph's three
//! sizes, and kept in the store beside a note of what was made, so every node serves them without decoding again.
//! A file that isn't a picture the `image` crate reads, or is too large to decode, has none.

use std::collections::BTreeMap;
use std::io::Cursor;

use bytes::Bytes;
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};

use super::{Error, drive, repo};
use crate::app::App;

/// Graph's thumbnail sizes: the longest side of each.
pub(super) const SIZES: [(&str, u32); 3] = [("small", 96), ("medium", 176), ("large", 800)];
/// The largest file decoded for its thumbnails.
const MAX_SOURCE: i64 = 32 * 1024 * 1024;
/// The largest picture decoded, by side and by memory.
const MAX_SIDE: u32 = 16_384;
const MAX_DECODED: u64 = 256 * 1024 * 1024;

/// Pictures a node decodes at once: each may take [`MAX_DECODED`] of memory.
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Runs `work` over `source` off the async threads, when a decoder is free in turn. The decode holds its turn
/// until it ends: a request that goes away leaves it running.
pub(crate) async fn decoding<T: Send + 'static>(
    source: Bytes,
    work: impl FnOnce(&[u8]) -> Option<T> + Send + 'static,
) -> Option<T> {
    let turn = DECODERS.acquire().await.ok()?;
    let decode = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        work(&source)
    });
    decode.await.ok().flatten()
}

/// What was made for a file: each size's width and height; none when the file has no thumbnail.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Made {
    pub sizes: BTreeMap<String, (u32, u32)>,
}

fn note_key(item: &str) -> String {
    format!("drive-thumbnails/{item}/made.json")
}

fn picture_key(item: &str, size: &str) -> String {
    format!("drive-thumbnails/{item}/{size}.jpg")
}

/// Every key a file's thumbnails may be kept under.
pub(super) fn keys(item: &str) -> impl Iterator<Item = String> {
    let pictures: Vec<String> = SIZES.iter().map(|(size, _)| picture_key(item, size)).collect();
    std::iter::once(note_key(item)).chain(pictures)
}

/// The thumbnails of `item`, made now when nobody asked before.
pub(super) async fn of(app: &App, item: &repo::Item) -> Result<Made, Error> {
    if let Some(note) = app.storage.get(&note_key(&item.id)).await? {
        return Ok(serde_json::from_slice(&note).unwrap_or_default());
    }
    let made = make(app, item).await;
    // a delete meanwhile cleared keys that weren't there yet, whether every write went through or not
    if repo::item(&app.db, &item.id).await?.is_none() && !super::sites::exists(&app.db, &item.id).await? {
        for key in keys(&item.id) {
            super::clear(app.storage.as_ref(), &key).await;
        }
        return Err(Error::Missing);
    }
    made
}

async fn make(app: &App, item: &repo::Item) -> Result<Made, Error> {
    let mut made = Made::default();
    if item.size <= MAX_SOURCE
        && let Some(source) = app.storage.get(&drive::key(&item.id)).await?
        && let Some(pictures) = decoding(source, render).await
    {
        for (size, width, height, bytes) in pictures {
            app.storage.put(&picture_key(&item.id, size), bytes).await?;
            made.sizes.insert(size.to_owned(), (width, height));
        }
    }
    let note = serde_json::to_vec(&made).unwrap_or_default();
    app.storage.put(&note_key(&item.id), note.into()).await?;
    Ok(made)
}

/// One thumbnail's bytes, when the file has it.
pub(super) async fn picture(app: &App, item: &repo::Item, size: &str) -> Result<Option<(&'static str, Bytes)>, Error> {
    if !SIZES.iter().any(|(known, _)| *known == size) {
        return Ok(None);
    }
    Ok(app
        .storage
        .get(&picture_key(&item.id, size))
        .await?
        .map(|bytes| ("image/jpeg", bytes)))
}

/// `source` as a picture, within the decoder's limits; `None` when it isn't one.
pub(crate) fn decode(source: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(source)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    reader.limits(limits);
    reader.decode().ok()
}

/// `picture` as a JPEG whose longest side is at most `side`, never larger than the picture itself: its width,
/// height and bytes.
pub(super) fn jpeg(picture: &DynamicImage, side: u32) -> Option<(u32, u32, Bytes)> {
    let scaled = if picture.width() <= side && picture.height() <= side {
        picture.clone()
    } else {
        picture.thumbnail(side, side)
    };
    let flat = DynamicImage::ImageRgb8(scaled.to_rgb8());
    let mut bytes = Cursor::new(Vec::new());
    flat.write_to(&mut bytes, ImageFormat::Jpeg).ok()?;
    Some((flat.width(), flat.height(), Bytes::from(bytes.into_inner())))
}

/// Each of Graph's sizes of `source`; `None` when it isn't a picture.
fn render(source: &[u8]) -> Option<Vec<(&'static str, u32, u32, Bytes)>> {
    let picture = decode(source)?;
    SIZES
        .iter()
        .map(|(size, side)| jpeg(&picture, *side).map(|(width, height, bytes)| (*size, width, height, bytes)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_request_keeps_its_decoder_until_the_decode_ends() {
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        let mut requests = Vec::new();
        for _ in 0..DECODERS.available_permits() {
            let (release, held) = mpsc::channel::<()>();
            let started = started.clone();
            releases.push(release);
            requests.push(tokio::spawn(decoding(Bytes::new(), move |_| {
                started.send(()).unwrap();
                held.recv().ok();
                Some(())
            })));
        }
        for request in requests {
            starts.recv().await.unwrap();
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
        }

        let mut next = tokio::spawn(decoding(Bytes::new(), |_| Some(())));
        let waited = tokio::time::timeout(Duration::from_millis(300), &mut next).await;
        assert!(
            waited.is_err(),
            "a decode started while the dropped requests' decodes still ran"
        );
        drop(releases);
        assert_eq!(next.await.unwrap(), Some(()));
    }
}
