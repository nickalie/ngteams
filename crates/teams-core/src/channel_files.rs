//! A channel's files, uploaded as Teams' web client uploads them (`teams_proto::channel_file`): an empty file of the
//! name in the channel's folder on its team's site, numbered as the web client numbers it while the name is taken,
//! then its bytes, then the end of the upload. Remove sends the file to the site's recycle bin. The team's site gives
//! the team access, so a channel file is never shared. Each write goes through the outbox once, again only as
//! [`CallRetries`] allows; an unknown outcome is never sent again.

use std::path::PathBuf;

use teams_proto::channel_file::{is_name_taken, numbered_name, uploaded_file};
use teams_proto::{
    ChannelFileWrite, ChannelFolder, Client, Credentials, HttpError, HttpTransport, Response, Transport, UploadedFile,
};

use crate::{CallRetries, CallTried, EngineError, Outbox};

/// How many numbered names an upload tries after its own name is taken.
const NUMBERED_NAMES: u32 = 20;

/// Uploads the file at `path` into the folder of `conversation`'s channel (a post's chain is its channel's).
pub(crate) async fn upload<C, T>(
    client: &Client<HttpTransport<C>>,
    outbox: &Outbox<T>,
    conversation: &str,
    path: PathBuf,
) -> Result<UploadedFile, EngineError>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError>,
{
    if client.transport().is_read_only() {
        return Err(failed("This app is read-only."));
    }
    let channel = teams_proto::stored_conversation(conversation);
    if !teams_proto::is_team_thread_id(channel) {
        return Err(failed("Files go to a channel's folder only from a channel."));
    }
    let (name, bytes) = tokio::task::spawn_blocking(move || crate::attachments::read_file(&path))
        .await
        .map_err(|_| failed("The file couldn't be read."))??;
    let folder = folder(client, channel).await?;
    let stub = add_stub(outbox, &folder, &name).await?;
    let size = bytes.len() as u64;
    let finished = fill(outbox, &folder, &stub, bytes, size).await;
    if finished.is_err() {
        // the empty file would stay in the team's folder for everyone; the web client's Remove recycles it too
        let _ = recycle(outbox, &folder, stub.unique_id()).await;
    }
    finished
}

/// Sends a channel file the draft let go to the site's recycle bin.
pub(crate) async fn discard<C, T>(
    client: &Client<HttpTransport<C>>,
    outbox: &Outbox<T>,
    file: &UploadedFile,
) -> Result<(), EngineError>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError>,
{
    if client.transport().is_read_only() {
        return Err(failed("This app is read-only."));
    }
    let channel = file.channel().ok_or_else(|| failed("This file isn't a channel's."))?;
    if !file.is_in(client.transport().cloud()) {
        return Err(failed("This file isn't in this channel's folder."));
    }
    // the transport finds the file in the folder before the write goes
    let folder = folder(client, channel).await?;
    recycle(outbox, &folder, file.unique_id()).await
}

async fn folder<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    channel: &str,
) -> Result<ChannelFolder, EngineError> {
    client
        .transport()
        .channel_folder(channel)
        .await
        .map_err(|error| match error {
            HttpError::Refused(_) => failed("This channel has no files folder this app can reach."),
            _ => failed("Couldn't open this channel's files folder. Check your connection and try again."),
        })
}

/// An empty file of `name` in the folder, or of the first numbered name the folder doesn't have yet.
async fn add_stub<T: Transport<Error = HttpError>>(
    outbox: &Outbox<T>,
    folder: &ChannelFolder,
    name: &str,
) -> Result<UploadedFile, EngineError> {
    for attempt in 0..=NUMBERED_NAMES {
        let tried = if attempt == 0 {
            name.to_owned()
        } else {
            numbered_name(name, attempt)
        };
        let write = ChannelFileWrite::add_stub(folder, &tried)
            .ok_or_else(|| failed("This file can't be uploaded. Rename it and try again."))?;
        let answer = send(outbox, write).await?;
        if answer.status() == 400 && is_name_taken(answer.body()) {
            continue;
        }
        if !answer.is_success() {
            return Err(answered(&answer));
        }
        return uploaded_file(folder, answer.body())
            .ok_or_else(|| failed("The file service's answer couldn't be read. Try again."));
    }
    Err(failed(
        "This channel has too many files of this name. Rename the file and try again.",
    ))
}

/// The bytes into the empty file, then the end of the upload: the file as the folder now holds it.
async fn fill<T: Transport<Error = HttpError>>(
    outbox: &Outbox<T>,
    folder: &ChannelFolder,
    stub: &UploadedFile,
    bytes: Vec<u8>,
    size: u64,
) -> Result<UploadedFile, EngineError> {
    let start = ChannelFileWrite::start_upload(folder, stub.unique_id(), stub.name(), bytes)
        .ok_or_else(|| failed("This file can't be uploaded. Choose it again."))?;
    let finish = ChannelFileWrite::finish_upload(&start, stub.name(), size)
        .ok_or_else(|| failed("This file can't be uploaded. Choose it again."))?;
    let answer = send(outbox, start).await?;
    if !answer.is_success() {
        return Err(answered(&answer));
    }
    let answer = send(outbox, finish).await?;
    if !answer.is_success() {
        return Err(answered(&answer));
    }
    uploaded_file(folder, answer.body())
        .filter(|file| file.unique_id() == stub.unique_id())
        .ok_or_else(|| failed("The upload completed, but its file details couldn't be read. Choose the file again."))
}

async fn recycle<T: Transport<Error = HttpError>>(
    outbox: &Outbox<T>,
    folder: &ChannelFolder,
    file: &str,
) -> Result<(), EngineError> {
    let write = ChannelFileWrite::recycle(folder, file).ok_or_else(|| failed("This file can't be removed."))?;
    let answer = send(outbox, write).await?;
    if answer.is_success() {
        Ok(())
    } else {
        Err(answered(&answer))
    }
}

/// One write through the outbox, another attempt only as [`CallRetries`] allows; the service's answer, whatever its
/// status.
async fn send<T: Transport<Error = HttpError>>(
    outbox: &Outbox<T>,
    write: ChannelFileWrite,
) -> Result<Response, EngineError> {
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_channel_file(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) => Ok(answer),
            CallTried::Denied(reason) | CallTried::NotSent(reason) => Err(failed(&reason)),
            CallTried::Uncertain(_) => Err(failed(
                "The file service didn't confirm the upload. Check your connection before trying again.",
            )),
            CallTried::Throttled(_) => Err(failed("The file service is busy. Try again.")),
        };
    }
}

fn answered(answer: &Response) -> EngineError {
    log::info!("a channel file write was answered HTTP {}", answer.status());
    failed(crate::attachments::NOT_UPLOADED)
}

fn failed(why: &str) -> EngineError {
    EngineError::Attachment { why: why.to_owned() }
}
