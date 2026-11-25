//! A meeting's recap, read when its Recap tab opens (`docs/protocol/recap.md`): what the meeting left, from MCPS
//! ([`teams_proto::MeetingContents`]), and a transcript's `WebVTT`, from the personal site that holds it
//! ([`teams_proto::TranscriptLocation`]), handed on as it came once it reads as a transcript, and its language, from its
//! item's transcripts. Nothing is stored and nothing is written; the recording is streamed (below), and played from
//! its download link as any shared video is ([`crate::file_content`]) only when it can't be streamed.
//!
//! A read that fails, an answer in another shape, or a transcript address the app doesn't read is
//! [`EngineError::RecapUnread`]: the view says so and offers to try again.
//!
//! A recording is streamed as Teams' Recap player streams it ([`teams_proto::recording`]): its DASH presentation read
//! from the personal site that holds it, and its parts from the media transform service, each read when the player
//! asks for it. That works whether or not the account may download the recording (an attendee's view of another's
//! meeting is refused at the download link). A recording whose recap names no drive or item is streamed by the ones its
//! item on Graph names, read by its sharing link ([`FileDownload`]); once that item reads, its presentation not read
//! for any reason is [`EngineError::FileUnavailable`], so the player plays its file. A presentation refused or unreadable is
//! [`EngineError::FileUnavailable`], as is a part refused; one whose read failed on the way is
//! [`EngineError::FileUnread`].
//!
//! A recording's file ([`teams_proto::RecordingFile`]) is its item read on the same site: whether the account may
//! download it (the recap offers Download only then), its name and size. Its bytes are downloaded as any shared file's
//! are ([`crate::file_content`]: Graph's download link for its sharing link, read again when it expired).

use std::sync::Arc;

use teams_proto::recap::transcript_language;
use teams_proto::recording::{read_expiration, read_manifest, read_recording_file};
use teams_proto::transcript::{MAX_TRANSCRIPT, read_vtt};
use teams_proto::{
    Client, Credentials, FileDownload, HttpError, HttpTransport, MediaSegmentLink, MeetingContent, MeetingContents,
    Presentation, RecordingAccess, RecordingExpiration, RecordingFile, RecordingManifest, TranscriptLocation,
};

use crate::engine::{EngineError, Responder};

/// The recap reads of one account.
pub(crate) struct Recaps<C> {
    client: Arc<Client<HttpTransport<C>>>,
}

impl<C: Credentials + 'static> Recaps<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self { client }
    }

    /// What the meeting whose chat is `thread` left, as its recap lists it.
    pub(crate) fn contents(&self, thread: String, reply: Responder<Vec<MeetingContent>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.read(&MeetingContents::of(thread)).await {
                Ok(answer) => MeetingContents::read_answer(answer.body()).ok_or(EngineError::RecapUnread),
                Err(error) => {
                    log::info!("a meeting's recap contents couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// The DASH presentation of the recording `manifest` names, once it reads as one the player plays.
    pub(crate) fn presentation(&self, manifest: RecordingManifest, reply: Responder<Arc<Presentation>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let _ = reply.send(read_presentation(&client, &manifest).await);
        });
    }

    /// The DASH presentation of the recording `link` is the sharing link of, by the drive and item its item on Graph
    /// names (a recap that names neither), once it reads as one the player plays.
    pub(crate) fn presentation_by_link(&self, link: String, reply: Responder<Arc<Presentation>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let _ = reply.send(presentation_by_link(&client, &link).await);
        });
    }

    /// A part of a recording's presentation, at its address.
    pub(crate) fn segment(&self, part: MediaSegmentLink, reply: Responder<Vec<u8>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().media_segment(&part).await {
                Ok(answer) => Ok(answer.body().to_vec()),
                Err(error) => {
                    log::info!("a part of a meeting recording couldn't be read: {error}");
                    Err(file_error(&error))
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// What the recording's file `file` is to the account: whether it may download it, its name and size.
    pub(crate) fn file(&self, file: RecordingFile, reply: Responder<RecordingAccess>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().recording_file(&file).await {
                Ok(answer) => read_recording_file(answer.body()).ok_or_else(|| {
                    log::info!("a meeting recording's item isn't one the recap reads");
                    EngineError::FileUnavailable
                }),
                Err(error) => {
                    log::info!("a meeting recording's item couldn't be read: {error}");
                    Err(file_error(&error))
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// When the recording `read` names expires (ms since the epoch), as its item says; `None` when it says none.
    pub(crate) fn expiration(&self, read: RecordingExpiration, reply: Responder<Option<i64>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().recording_expiration(&read).await {
                Ok(answer) => Ok(read_expiration(answer.body())),
                Err(error) => {
                    log::info!("a meeting recording's expiry couldn't be read: {error}");
                    Err(file_error(&error))
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// The `WebVTT` of the transcript at `location` (a `TranscriptV2` content's), once it reads as one.
    pub(crate) fn transcript(&self, location: &str, reply: Responder<String>) {
        let Some(location) = TranscriptLocation::of(location) else {
            log::info!("a meeting's transcript is somewhere the app doesn't read");
            let _ = reply.send(Err(EngineError::RecapUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().meeting_transcript(&location).await {
                Ok(answer) if answer.body().len() <= MAX_TRANSCRIPT => std::str::from_utf8(answer.body())
                    .ok()
                    .filter(|text| read_vtt(text).is_some())
                    .map(str::to_owned)
                    .ok_or(EngineError::RecapUnread),
                Ok(_) => Err(EngineError::RecapUnread),
                Err(error) => {
                    log::info!("a meeting's transcript couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// The language of the transcript at `location` (a `TranscriptV2` content's), as its item's transcripts give it.
    pub(crate) fn language(&self, location: &str, reply: Responder<Option<String>>) {
        let Some(location) = TranscriptLocation::of(location) else {
            let _ = reply.send(Err(EngineError::RecapUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().meeting_transcript_languages(&location).await {
                Ok(answer) => Ok(transcript_language(answer.body(), location.id())),
                Err(error) => {
                    log::info!("a meeting transcript's language couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }
}

/// The presentation `manifest` names, read now.
async fn read_presentation<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    manifest: &RecordingManifest,
) -> Result<Arc<Presentation>, EngineError> {
    match client.transport().recording_manifest(manifest).await {
        Ok(answer) => read_manifest(answer.body()).map(Arc::new).ok_or_else(|| {
            log::info!("a meeting recording's presentation isn't one the player plays");
            EngineError::FileUnavailable
        }),
        Err(error) => {
            log::info!("a meeting recording's presentation couldn't be read: {error}");
            Err(file_error(&error))
        }
    }
}

/// The presentation of the recording `link` is the sharing link of: its drive and item from its item on Graph, then
/// its presentation on its personal site.
async fn presentation_by_link<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    link: &str,
) -> Result<Arc<Presentation>, EngineError> {
    let read = FileDownload::of_in(client.transport().cloud(), link).ok_or(EngineError::FileUnavailable)?;
    let item = client.read(&read).await.map_err(|error| {
        log::info!("a meeting recording's item couldn't be read: {error}");
        file_error(&error)
    })?;
    let (drive, id) = FileDownload::read_item_ids(&item).ok_or(EngineError::FileUnavailable)?;
    let manifest = RecordingManifest::of(link, &drive, &id).ok_or(EngineError::FileUnavailable)?;
    // the item read did, so the player can still fall back to its download link
    read_presentation(client, &manifest)
        .await
        .map_err(|_| EngineError::FileUnavailable)
}

/// A file read that failed: refused or too large, or a transport error that a later read can resolve.
pub(crate) fn file_error(error: &HttpError) -> EngineError {
    match error {
        HttpError::Status(answer) if !matches!(answer.status(), 408 | 429 | 500..=599) => EngineError::FileUnavailable,
        HttpError::Refused(_) | HttpError::BodyTooLarge => EngineError::FileUnavailable,
        _ => EngineError::FileUnread,
    }
}
