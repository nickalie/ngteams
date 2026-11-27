//! `download`: the attachments of a message saved in a folder that the client names. The account's engine reads them
//! as the chat view reads them, so the client gets no token.
//!
//! - **The attachments** of a message ([`attachments`]): its shared files in their order ([`chat_view::files_of`]),
//!   then the pictures in its text (not an emoticon, and not one in a quote). Each has its index from 1, as `messages`
//!   lists them. A picture is named `image-<index>`, and its file has the extension of its format.
//! - **A file** is saved by the download of the file cards ([`recording_download::start_with`]): the engine reads it
//!   by its link ([`Core::file_stream`]), a SharePoint or OneDrive file through Graph, an own server's file on its
//!   server. **A picture** is read as the chat view shows it ([`Core::message_picture`]). Each is written as
//!   `<name>.part`, then renamed.
//! - **The folder** is an absolute path, made when it is not there. Each file name is made safe ([`files::safe_name`]),
//!   so a name never leaves the folder. A file that is in the folder already stops the download before a file is
//!   written, unless `force`.
//! - A download is a read: the read-only mode serves it too.
//! - The files are saved one after the other. The first failure stops the download, and its error names the files
//!   saved before it. Past [`DOWNLOAD_ANSWER`] the app answers `timeout`, and the download that runs continues.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use gpui::{AsyncApp, Entity, WeakEntity};
use teams_ctl::protocol::{
    AttachmentKind, AttachmentRow, CtlError, DOWNLOAD_ANSWER, ErrorCode, Request, Response, SavedFile,
};
use teams_proto::chat::{FileKind, Node, parse_html};
use teams_store::Message;
use teams_ui::Core;
use teams_ui::chat_view;
use teams_ui::files;
use teams_ui::recording_download::{self, Failure, Opener, Progress, RecordingDownload, Then};

use super::address::{self, engine_error};
use super::dispatch::alive;
use super::writes::within;
use crate::app::Running;

/// A file or a picture that a message carries, and where the engine reads it.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub row: AttachmentRow,
    source: Source,
}

#[derive(Clone, Debug)]
enum Source {
    /// A shared file, by its link (none when its entry has none that the app reads).
    File(Option<String>),
    /// A picture in the text, by its `src`.
    Picture(String),
}

/// The attachments of `message` (see the module docs); none for a deleted one.
#[must_use]
pub fn attachments(message: &Message) -> Vec<Attachment> {
    if message.delete_time.is_some() {
        return Vec::new();
    }
    let files = chat_view::files_of(message).into_iter().map(|file| {
        let file_type = Some(file_type(file.kind()).to_owned());
        let link = file.open_link().map(str::to_owned);
        (Some(file.name), AttachmentKind::File, file_type, Source::File(link))
    });
    let pictures = pictures_of(message)
        .into_iter()
        .map(|src| (None, AttachmentKind::Picture, None, Source::Picture(src)));
    files
        .chain(pictures)
        .zip(1_u32..)
        .map(|((name, kind, file_type, source), index)| Attachment {
            row: AttachmentRow {
                index,
                name: name.unwrap_or_else(|| format!("image-{index}")),
                kind,
                file_type,
                size: None,
            },
            source,
        })
        .collect()
}

/// The type of a file as the wire names it.
fn file_type(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Video => "video",
        FileKind::Image => "image",
        FileKind::Audio => "audio",
        FileKind::Pdf => "pdf",
        FileKind::Word => "word",
        FileKind::Excel => "excel",
        FileKind::PowerPoint => "powerpoint",
        FileKind::Archive => "archive",
        FileKind::Text => "text",
        FileKind::Other => "other",
    }
}

/// The `src` of each picture in the text of `message`, in its order: not an emoticon, a sticker or a quoted picture.
fn pictures_of(message: &Message) -> Vec<String> {
    if !message.message_type.starts_with("RichText") {
        return Vec::new();
    }
    let mut found = Vec::new();
    collect_pictures(&parse_html(&message.content).nodes, &mut found);
    found
}

fn collect_pictures(nodes: &[Node], found: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Image(image) if !image.is_emoji() => found.push(image.src.clone()),
            Node::Paragraph(children)
            | Node::Link { children, .. }
            | Node::Span { children, .. }
            | Node::Heading { children, .. }
            | Node::Indented { children, .. } => collect_pictures(children, found),
            Node::List(list) => {
                for item in &list.items {
                    collect_pictures(item, found);
                }
            }
            Node::Table(table) => {
                for cell in table.rows.iter().flatten() {
                    collect_pictures(&cell.nodes, found);
                }
            }
            Node::Image(_)
            | Node::Quote(_)
            | Node::Sticker(_)
            | Node::Text { .. }
            | Node::Break
            | Node::Mention { .. }
            | Node::Code(_)
            | Node::Rule => {}
        }
    }
}

/// Whether `request` is a `download`.
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(request, Request::Download { .. })
}

/// What a `download` selects.
struct Pick<'a> {
    name: Option<&'a str>,
    index: Option<u32>,
    all: bool,
}

/// `download` for the account `account` names (see the module docs).
///
/// # Errors
/// The account, the chat or the message is not one; the selection names no attachment; the folder is not absolute; a
/// file is there already without `force`; the engine or the disk failed; or the download took longer than
/// [`DOWNLOAD_ANSWER`].
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let Request::Download {
        chat,
        message,
        name,
        index,
        all,
        dir,
        force,
    } = request
    else {
        return Err(CtlError::new(ErrorCode::BadRequest, "not a download"));
    };
    let folder = PathBuf::from(dir);
    if !folder.is_absolute() {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("the folder {dir:?} isn't an absolute path"),
        ));
    }
    let core = cx.update(|cx| {
        let running = alive(running, cx)?;
        address::account(running.read(cx), account, cx).map(|addressed| addressed.core)
    })?;
    let chat = address::chat(&*core, chat).await?;
    let stored = core
        .message(chat.id.clone(), message.clone())
        .await
        .map_err(|error| engine_error(&error))?
        .ok_or_else(|| CtlError::new(ErrorCode::BadRequest, format!("the chat holds no message {message}")))?;
    let pick = Pick {
        name: name.as_deref(),
        index: *index,
        all: *all,
    };
    let chosen = chosen(attachments(&stored), &pick)?;
    let executor = cx.background_executor().clone();
    let late = format!(
        "the download wasn't done in {}s: it continues",
        DOWNLOAD_ANSWER.as_secs()
    );
    within(
        DOWNLOAD_ANSWER,
        &late,
        save_all(core, chosen, folder, *force, cx),
        &executor,
    )
    .await
}

/// The attachments `pick` selects of `attachments`.
fn chosen(attachments: Vec<Attachment>, pick: &Pick<'_>) -> Result<Vec<Attachment>, CtlError> {
    let none = |message: String| CtlError::new(ErrorCode::NoAttachment, message);
    if attachments.is_empty() {
        return Err(none("the message has no attachment".to_owned()));
    }
    if pick.all {
        return Ok(attachments);
    }
    if let Some(index) = pick.index {
        let count = attachments.len();
        return attachments
            .into_iter()
            .find(|attachment| attachment.row.index == index)
            .map(|attachment| vec![attachment])
            .ok_or_else(|| none(format!("the message has no attachment {index}: it has {count}")));
    }
    if let Some(name) = pick.name {
        let named: Vec<Attachment> = attachments
            .into_iter()
            .filter(|attachment| attachment.row.name == name)
            .collect();
        return match named.len() {
            1 => Ok(named),
            0 => Err(none(format!("the message has no attachment {name:?}"))),
            _ => Err(none(format!(
                "several attachments are called {name:?}: select one with --index"
            ))),
        };
    }
    if attachments.len() == 1 {
        return Ok(attachments);
    }
    Err(none(format!(
        "the message has {} attachments: select them with --name, --index or --all",
        attachments.len()
    )))
}

/// An attachment ready to save: its file name in the folder, and the picture's bytes already read.
struct Planned {
    attachment: Attachment,
    file_name: String,
    picture: Option<Vec<u8>>,
}

/// Saves `chosen` in `folder`, one after the other.
async fn save_all(
    core: Arc<dyn Core>,
    chosen: Vec<Attachment>,
    folder: PathBuf,
    force: bool,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let planned = plan(&*core, chosen).await?;
    let targets: Vec<PathBuf> = planned
        .iter()
        .map(|planned| inside(&folder, &planned.file_name))
        .collect::<Result<_, _>>()?;
    let there = {
        let targets = targets.clone();
        cx.background_executor()
            .spawn(async move {
                targets
                    .into_iter()
                    .filter(|target| target.symlink_metadata().is_ok())
                    .collect::<Vec<_>>()
            })
            .await
    };
    if !force && !there.is_empty() {
        let names: Vec<String> = there.iter().map(|target| target.display().to_string()).collect();
        return Err(CtlError::new(
            ErrorCode::FileExists,
            format!("{} is there already: --force replaces it", names.join(", ")),
        ));
    }
    let mut files = Vec::with_capacity(planned.len());
    for (planned, target) in planned.into_iter().zip(targets) {
        let saved = match planned.picture {
            Some(bytes) => write_picture(target.clone(), bytes, cx).await,
            None => save_file(&core, &planned.attachment, target.clone(), cx).await,
        };
        match saved {
            Ok(size) => files.push(SavedFile {
                index: planned.attachment.row.index,
                name: planned.attachment.row.name,
                path: target.display().to_string(),
                size,
            }),
            Err(error) => return Err(failed_after(error, &files)),
        }
    }
    Ok(Response::Downloaded { files })
}

/// `error`, with the files saved before it named.
fn failed_after(mut error: CtlError, files: &[SavedFile]) -> CtlError {
    if !files.is_empty() {
        let saved: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        error.message = format!("{} (saved before it: {})", error.message, saved.join(", "));
    }
    error
}

/// The file name of each attachment, each one once, and the bytes of each picture (its format gives its extension).
async fn plan(core: &dyn Core, chosen: Vec<Attachment>) -> Result<Vec<Planned>, CtlError> {
    let mut taken = HashSet::new();
    let mut planned = Vec::with_capacity(chosen.len());
    for attachment in chosen {
        let (name, picture) = match &attachment.source {
            Source::File(_) => (files::safe_name(&attachment.row.name), None),
            Source::Picture(src) => {
                let picture = core
                    .message_picture(src.clone())
                    .await
                    .map_err(|error| engine_error(&error))?
                    .ok_or_else(|| {
                        CtlError::new(
                            ErrorCode::Engine,
                            format!("the picture {} can't be read", attachment.row.name),
                        )
                    })?;
                let name = format!(
                    "{}.{}",
                    files::safe_name(&attachment.row.name),
                    picture.format.extension()
                );
                (name, Some(picture.bytes.clone()))
            }
        };
        let file_name = unique(name, attachment.row.index, &mut taken);
        planned.push(Planned {
            attachment,
            file_name,
            picture,
        });
    }
    Ok(planned)
}

/// `name`, or with ` (<index>)` before its extension when an earlier attachment of the download took it.
fn unique(name: String, index: u32, taken: &mut HashSet<String>) -> String {
    let name = if taken.contains(&name) {
        match name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({index}).{extension}"),
            _ => format!("{name} ({index})"),
        }
    } else {
        name
    };
    taken.insert(name.clone());
    name
}

/// `folder` joined with `file_name`, which must be one plain name.
fn inside(folder: &Path, file_name: &str) -> Result<PathBuf, CtlError> {
    let mut parts = Path::new(file_name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => Ok(folder.join(file_name)),
        _ => Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("{file_name:?} isn't a plain file name"),
        )),
    }
}

/// Saves the shared file of `attachment` at `target` as its card's Download saves it; its size once saved.
async fn save_file(
    core: &Arc<dyn Core>,
    attachment: &Attachment,
    target: PathBuf,
    cx: &mut AsyncApp,
) -> Result<u64, CtlError> {
    let Source::File(Some(link)) = &attachment.source else {
        return Err(CtlError::new(
            ErrorCode::Engine,
            format!("{} has no link that the app reads it by", attachment.row.name),
        ));
    };
    let opened = link.clone();
    let engine = Arc::clone(core);
    let open: Opener = Arc::new(move |offset, len| engine.file_stream(opened.clone(), offset, len));
    let download = cx.update(|cx| recording_download::start_with(open, link.clone(), target.clone(), Then::Keep, cx));
    match settled(&download, cx).await {
        Progress::Saved => size_of(target, cx).await,
        Progress::Failed(failure) => Err(CtlError::new(
            ErrorCode::Engine,
            format!(
                "{} couldn't be downloaded: {}",
                attachment.row.name,
                failure_words(&failure)
            ),
        )),
        Progress::Running { .. } => Err(CtlError::new(ErrorCode::Engine, "the download stopped")),
    }
}

fn failure_words(failure: &Failure) -> String {
    match failure {
        Failure::Refused => "the account may no longer have access to the file.".to_owned(),
        other => other.words(),
    }
}

/// Where `download` stands once it no longer runs.
async fn settled(download: &Entity<RecordingDownload>, cx: &mut AsyncApp) -> Progress {
    let (done, finished) = async_channel::bounded::<Progress>(1);
    let observing = cx.update(|cx| {
        let progress = download.read(cx).progress().clone();
        if !progress.running() {
            return Err(progress);
        }
        Ok(cx.observe(download, move |download, cx| {
            let progress = download.read(cx).progress();
            if !progress.running() {
                let _ = done.try_send(progress.clone());
            }
        }))
    });
    let _observing = match observing {
        Ok(observing) => observing,
        Err(progress) => return progress,
    };
    finished.recv().await.unwrap_or(Progress::Failed(Failure::Cancelled))
}

/// Writes a picture's `bytes` at `target` through its `.part` file; its size.
async fn write_picture(target: PathBuf, bytes: Vec<u8>, cx: &mut AsyncApp) -> Result<u64, CtlError> {
    cx.background_executor()
        .spawn(async move {
            let part = recording_download::part_path(&target);
            if let Some(folder) = target.parent() {
                std::fs::create_dir_all(folder)?;
            }
            let written = std::fs::write(&part, &bytes).and_then(|()| std::fs::rename(&part, &target));
            if written.is_err() {
                let _ = std::fs::remove_file(&part);
            }
            written.map(|()| bytes.len() as u64)
        })
        .await
        .map_err(|error| disk_error(&error))
}

async fn size_of(target: PathBuf, cx: &mut AsyncApp) -> Result<u64, CtlError> {
    cx.background_executor()
        .spawn(async move { std::fs::metadata(&target).map(|metadata| metadata.len()) })
        .await
        .map_err(|error| disk_error(&error))
}

fn disk_error(error: &std::io::Error) -> CtlError {
    CtlError::new(ErrorCode::Engine, format!("the file couldn't be saved: {error}"))
}
