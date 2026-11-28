//! A file downloaded through the engine: a meeting recording (the recap's More options → Download, offered only when
//! the recording's item says the account may download it, as Teams' Stream page offers its Download), a Microsoft
//! file (its card's Download, or the video player's), or a file on an own server (its card's Download and Open in
//! desktop app: such a server has no page a browser opens a file at).
//!
//! - **The bytes** are read by the engine at the download link Graph hands out for the file's sharing link, else for
//!   its own address ([`Core::file_stream`], as a chat's video is played), [`CHUNK`] at a time, each written as it
//!   arrives. A link that expired is read again by the engine; a stream that broke off is opened again where the file
//!   stands, a few times ([`RETRIES`]) while nothing more arrives. A recording refused is not asked again.
//! - **The file** is written beside its place as `<name>.part`, then renamed over it once whole; a download that failed
//!   or was cancelled leaves nothing behind. A download whose place is the place of another running download of the
//!   app, or the `.part` file of it, or the reverse, fails before it writes ([`Places`]). The download locks its
//!   `.part` file while it writes it, so a second writer of that file (another download to the same place by a path
//!   that the app does not find, or one in another ngteams) fails ([`Failure::Busy`]).
//! - **It runs apart from the view**: the downloads are the app's ([`Downloads`]), so leaving the recap or the chat
//!   doesn't stop one, and the recap shows the ones of its recordings again when it is shown. Writing happens off the
//!   UI thread; the recording keeps playing meanwhile.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use same_file::Handle;
use teams_core::{EngineError, FileStream, Reply};

use crate::bridge::Core;

/// The most bytes one stream asks for: the engine's longest ([`teams_proto::MAX_STREAM`]).
pub const CHUNK: u64 = teams_proto::MAX_STREAM;

/// How many times running a stream that brought nothing is opened again before the download fails.
pub const RETRIES: u32 = 4;

/// How long the download waits before opening a stream again, by the attempt (doubled each time, from this).
pub const RETRY_AFTER: Duration = Duration::from_secs(1);

/// How many bytes the view is told of at a time (and once the whole file came).
const TELL_EVERY: u64 = 256 * 1024;

/// The buffer between the stream and the file.
const WRITE_BUFFER: usize = 1024 * 1024;

/// Why a download failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The recording was refused (no longer there, or no longer the account's to download).
    Refused,
    /// The network failed on the way, again and again.
    Network,
    /// The file couldn't be written (its words).
    Disk(String),
    /// The user cancelled it.
    Cancelled,
    /// Another job holds the `.part` file of the same place, or saves to it or to its `.part` file (`report.part` while
    /// `report` is written): another download, a download to the same place by another path or in another ngteams,
    /// or a dismissed download that did not end yet. It may not show as running.
    Busy,
}

impl Failure {
    /// What the recap says of it, under "Couldn't download <name>": why, and what to do.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Refused => "You may no longer have access to the recording.".to_owned(),
            Self::Network => "Check your connection and try again.".to_owned(),
            Self::Disk(error) => format!("The file couldn't be saved: {error}"),
            Self::Cancelled => "The download was cancelled.".to_owned(),
            Self::Busy => "Another download is using this file. Save it to another place.".to_owned(),
        }
    }
}

/// Where a download stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Running: the bytes written so far and the file's size, once the first stream told it.
    Running {
        written: u64,
        total: Option<u64>,
    },
    Saved,
    Failed(Failure),
}

impl Progress {
    /// Whether it still runs.
    #[must_use]
    pub const fn running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// How much of the file is written, from 0 to 1, once its size is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        match *self {
            Self::Running {
                written,
                total: Some(total),
            } if total > 0 => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a fraction of a progress bar: a file's size is far under f64's exact range"
                )]
                let fraction = (written as f64 / total as f64).clamp(0.0, 1.0);
                #[expect(clippy::cast_possible_truncation, reason = "a fraction within 0..=1")]
                Some(fraction as f32)
            }
            _ => None,
        }
    }
}

/// Opens a stream of the file at an offset, of at most a length: the engine's [`Core::file_stream`] of its link.
pub type Opener = Arc<dyn Fn(u64, u64) -> Reply<FileStream> + Send + Sync>;

/// What happens to the file once it is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    Keep,
    /// It is opened in the system's app for it (an own server's file: Open in desktop app).
    Open,
}

/// One file's download (see the module docs).
pub struct RecordingDownload {
    open: Opener,
    /// The file's sharing link.
    link: String,
    /// Where it is saved.
    target: PathBuf,
    then: Then,
    progress: Progress,
    running: Option<Task<()>>,
    /// Whether the job of the last start still has its `.part` file: set at the start, cleared by the job as it ends
    /// (see [`Job`]). A cancelled download still has it until its job is dropped.
    writing: Arc<AtomicBool>,
}

impl RecordingDownload {
    /// The recording's sharing link.
    #[must_use]
    pub fn link(&self) -> &str {
        &self.link
    }

    /// Where it is saved.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The file's name, as the recap says it.
    #[must_use]
    pub fn name(&self) -> String {
        self.target.file_name().map_or_else(
            || self.target.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    #[must_use]
    pub const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// What happens to the file once it is saved.
    #[must_use]
    pub const fn then(&self) -> Then {
        self.then
    }

    /// Starts (or, after a failure, starts again) writing the recording to its file. When the job of an earlier start
    /// of this one did not end yet, this one fails at once ([`Failure::Busy`]). When the job of another download of
    /// the app still writes to the same place (its `.part` file), to the `.part` file of this one, or the reverse,
    /// the job of this one fails so before it writes ([`Places`]): two writers of one file would overwrite each
    /// other's bytes. Its job also fails so when another writer holds the lock of the `.part` file, or put another
    /// file at its path before the rename.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.writing.load(Ordering::Acquire) {
            self.running = None;
            self.progress = Progress::Failed(Failure::Busy);
            cx.notify();
            return;
        }
        self.progress = Progress::Running {
            written: 0,
            total: None,
        };
        // the latest progress, and a wake for the view when it moved on (wakes not taken yet fold into one)
        let latest = Arc::new(Mutex::new((0, 0)));
        let (wake, woken) = async_channel::bounded::<()>(1);
        self.writing = Arc::new(AtomicBool::new(true));
        let job = Job {
            writing: Arc::clone(&self.writing),
            places: cx.default_global::<Downloads>().places.clone(),
            place: None,
            part: None,
        };
        let executor = cx.background_executor().clone();
        let job = cx.background_spawn(save(
            Arc::clone(&self.open),
            self.target.clone(),
            Told {
                latest: Arc::clone(&latest),
                wake,
            },
            move |wait| executor.timer(wait),
            job,
        ));
        self.running = Some(cx.spawn(async move |this, cx| {
            while woken.recv().await.is_ok() {
                let (written, total) = *latest.lock().unwrap_or_else(PoisonError::into_inner);
                let told = this.update(cx, |this, cx| {
                    this.progress = Progress::Running {
                        written,
                        total: Some(total),
                    };
                    cx.notify();
                });
                if told.is_err() {
                    return;
                }
            }
            let saved = job.await;
            let _ = this.update(cx, |this, cx| {
                this.progress = match saved {
                    Ok(()) => {
                        this.opened(cx);
                        Progress::Saved
                    }
                    Err(failure) => Progress::Failed(failure),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Opens the saved file in the system's app for it (its `file://` link), when that is what follows the save.
    fn opened(&self, cx: &App) {
        if self.then == Then::Open
            && let Ok(link) = url::Url::from_file_path(&self.target)
        {
            cx.open_url(link.as_str());
        }
    }

    /// Stops a running download; what it wrote is removed by its job as the job ends ([`Job`]).
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.progress.running() {
            return;
        }
        // dropping the task drops its stream (the engine stops reading), its file and its `.part` file
        self.running = None;
        self.progress = Progress::Failed(Failure::Cancelled);
        cx.notify();
    }
}

/// The app's recording downloads, by the recording's sharing link: the last one of each.
#[derive(Default)]
pub struct Downloads {
    latest: HashMap<String, Entity<RecordingDownload>>,
    /// The ones still running that a later download of their link took the place of: kept until they end.
    earlier: Vec<Entity<RecordingDownload>>,
    /// The places that the jobs of the app write to.
    places: Places,
}

impl Global for Downloads {}

/// The places that the jobs of the app write to, with the links of their folders resolved ([`resolved`]): one for each
/// job, from before it opens its `.part` file until it ends ([`Job`]), also of a download that was dismissed. A job
/// takes its place only when no other job writes to the same place, to the `.part` file of its place, or the reverse
/// ([`places_collide`]), else it fails ([`Failure::Busy`]): a rename of one over the other would replace the file
/// that the other job writes. The job resolves its place, off the UI thread, after it made the folder.
///
/// It does not find another path to the same place that the resolution of folder links does not find (a name in
/// another case on a file system that ignores case, a bind mount), and not a download in another ngteams. For one of
/// those to the same place, the lock of the `.part` file stops it. For a download to the `.part` file of the other, a
/// rename over the `.part` file that this one writes makes `save` fail ([`Failure::Busy`]) before its own rename, and
/// that file stays. When this one renames first, the app saves the two files.
#[derive(Clone, Default)]
struct Places(Arc<Mutex<Vec<PathBuf>>>);

impl Places {
    /// Takes `place` (resolved) for a job, unless another job writes to it or to a place that collides with it.
    fn take(&self, place: &Path) -> bool {
        let mut places = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if places.iter().any(|taken| places_collide(taken, place)) {
            return false;
        }
        places.push(place.to_owned());
        true
    }

    /// Lets `place` go, as the job that took it ends.
    fn release(&self, place: &Path) {
        let mut places = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = places.iter().position(|taken| taken == place) {
            places.swap_remove(at);
        }
    }
}

/// Whether downloads to `one` and to `other` (both [`resolved`]) write the same file: the same place, or one place is
/// the `.part` file of the other.
fn places_collide(one: &Path, other: &Path) -> bool {
    one == other || part_path(one) == other || one == part_path(other)
}

/// `place` in its folder's real path (the links in it resolved, so `alias/report.part` is `folder/report.part` when
/// `alias` is a link to `folder`), or `place` as it is when its folder can't be resolved. It reads the disk: never on
/// the UI thread.
fn resolved(place: &Path) -> PathBuf {
    place
        .parent()
        .and_then(|folder| fs::canonicalize(folder).ok())
        .zip(place.file_name())
        .map_or_else(|| place.to_owned(), |(folder, name)| folder.join(name))
}

/// The download of the recording whose sharing link is `link`, when one was started this run (and not dismissed).
#[must_use]
pub fn download_of(link: &str, cx: &App) -> Option<Entity<RecordingDownload>> {
    cx.try_global::<Downloads>()?.latest.get(link).cloned()
}

/// Starts downloading the recording whose sharing link is `link` to `target`, in place of an earlier one of it that
/// ended (one still running to the same place is kept and handed back).
pub fn start(core: Arc<dyn Core>, link: String, target: PathBuf, cx: &mut App) -> Entity<RecordingDownload> {
    let opened = link.clone();
    let open: Opener = Arc::new(move |offset, len| core.file_stream(opened.clone(), offset, len));
    start_with(open, link, target, Then::Keep, cx)
}

/// As [`start`], the file's streams opened by `open`, and `then` done once it is saved. A running download of the
/// link (the one the card shows, or one that a later one took the place of) is handed back, and the card shows it
/// again, only when it does the same (the same `then`, and for [`Then::Keep`] the same `target`, whose `.part` file it
/// writes). One that does another (an Open while a Download runs, or the reverse) runs on to its end beside the new
/// one, which the card then shows. A new download to a place that a running download of another link writes to fails
/// before it writes (see [`RecordingDownload::start`]).
pub fn start_with(open: Opener, link: String, target: PathBuf, then: Then, cx: &mut App) -> Entity<RecordingDownload> {
    let latest = download_of(&link, cx).filter(|download| download.read(cx).progress.running());
    let mut earlier = cx
        .try_global::<Downloads>()
        .map(|downloads| downloads.earlier.clone())
        .unwrap_or_default();
    earlier.retain(|download| download.read(cx).progress.running());
    let same = |download: &Entity<RecordingDownload>| {
        let download = download.read(cx);
        download.link == link && download.then == then && (then == Then::Open || download.target == target)
    };
    if let Some(latest) = latest.as_ref().filter(|download| same(download)) {
        return latest.clone();
    }
    let download = if let Some(at) = earlier.iter().position(same) {
        earlier.remove(at)
    } else {
        cx.new(|cx| {
            let mut download = RecordingDownload {
                open,
                link: link.clone(),
                target,
                then,
                progress: Progress::Saved,
                running: None,
                writing: Arc::default(),
            };
            download.start(cx);
            download
        })
    };
    earlier.extend(latest);
    let downloads = cx.default_global::<Downloads>();
    downloads.earlier = earlier;
    downloads.latest.insert(link, download.clone());
    download
}

/// Forgets the download of the recording whose sharing link is `link` (its note dismissed), cancelling it if it runs.
pub fn dismiss(link: &str, cx: &mut App) {
    let Some(download) = cx
        .try_global::<Downloads>()
        .and_then(|downloads| downloads.latest.get(link).cloned())
    else {
        return;
    };
    download.update(cx, RecordingDownload::cancel);
    cx.default_global::<Downloads>().latest.remove(link);
}

/// Where the file is written until it is whole: `<name>.part` beside it.
#[must_use]
pub fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

/// Where a running download tells the view how far it got: the latest bytes written and the file's size, and a wake.
struct Told {
    latest: Arc<Mutex<(u64, u64)>>,
    wake: async_channel::Sender<()>,
}

/// A download's job, from its start until it ends or is dropped (cancelled): it clears the download's `writing` as it
/// ends, and before that removes the `.part` file it took, unless the file was saved or is no longer at its path
/// (another writer put its own file there, which stays), and then lets its place go.
struct Job {
    writing: Arc<AtomicBool>,
    /// The places that the jobs of the app write to.
    places: Places,
    /// The place (resolved) that the job took in [`Places`], until it ends.
    place: Option<PathBuf>,
    /// The `.part` file the job locked and writes (its path, and the file that holds the lock), until it is renamed
    /// over its place.
    part: Option<(PathBuf, fs::File)>,
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some((part, file)) = self.part.take() {
            // removed while it is still locked, so that no other writer takes it before
            if matches!(still_at(&file, &part), Ok(true)) {
                let _ = fs::remove_file(part);
            }
            drop(file);
        }
        if let Some(place) = self.place.take() {
            self.places.release(&place);
        }
        self.writing.store(false, Ordering::Release);
    }
}

/// Whether `file` is still the file at `path` (not renamed or removed, and not replaced by another file).
fn still_at(file: &fs::File, path: &Path) -> io::Result<bool> {
    let at = match Handle::from_path(path) {
        Ok(at) => at,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(Handle::from_file(file.try_clone()?)? == at)
}

/// Writes the file to `target` (through its [`part_path`]), telling `told` how far it got (every [`TELL_EVERY`]
/// bytes, and once the whole file came); what it wrote is removed when it fails or is dropped (by `job`). The
/// `.part` file is locked while it is written: when another writer has it locked, or the file it locked is no longer
/// the one at its path (another writer saved or removed it in the meantime), this fails ([`Failure::Busy`]) and does
/// not change it. It also fails so when, before the rename, another file is at its path: that file stays. Before it
/// opens the `.part` file, it takes its place in the app's [`Places`], or fails ([`Failure::Busy`]).
async fn save<F: Future<Output = ()>>(
    open: Opener,
    target: PathBuf,
    told: Told,
    sleep: impl FnMut(Duration) -> F,
    mut job: Job,
) -> Result<(), Failure> {
    let part = part_path(&target);
    let disk = |error: io::Error| Failure::Disk(error.to_string());
    // the downloads folder may not be there yet
    if let Some(folder) = part.parent() {
        fs::create_dir_all(folder).map_err(disk)?;
    }
    // the folder is there now: its links resolve
    let place = resolved(&target);
    if !job.places.take(&place) {
        return Err(Failure::Busy);
    }
    job.place = Some(place);
    // not emptied before it is locked: it may be the file of another writer
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&part)
        .map_err(disk)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(Failure::Busy),
        Err(fs::TryLockError::Error(error)) => return Err(disk(error)),
    }
    // the writer that had the lock before may have renamed or removed it since it was opened
    if !still_at(&file, &part).map_err(disk)? {
        return Err(Failure::Busy);
    }
    let (_, file) = job.part.insert((part.clone(), file));
    let file: &fs::File = file;
    // what an earlier run left in it
    file.set_len(0).map_err(disk)?;
    let mut sink = BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut said = 0;
    let fetched = fetch_from(open.as_ref(), &mut sink, sleep, |written, total| {
        *told.latest.lock().unwrap_or_else(PoisonError::into_inner) = (written, total);
        if written.saturating_sub(said) >= TELL_EVERY || written >= total {
            said = written;
            // a wake the view hasn't taken yet already brings it the latest
            let _ = told.wake.try_send(());
        }
    })
    .await;
    drop(told);
    fetched?;
    let file = sink.into_inner().map_err(|error| disk(error.into_error()))?;
    file.sync_all().map_err(disk)?;
    // another writer may have put its own file at the path in the meantime (a rename over it): that file is not saved
    if !still_at(file, &part).map_err(disk)? {
        return Err(Failure::Busy);
    }
    // renamed while it is still locked, so that no other writer takes it before
    fs::rename(&part, &target).map_err(disk)?;
    // saved: not removed, and the lock goes with the file
    job.part = None;
    Ok(())
}

/// Reads the recording whose sharing link is `link` into `sink` from its start, stream after stream ([`CHUNK`] each),
/// calling `told` with the bytes written and the file's size after each piece. A stream that broke off is opened again
/// where the file stands, after `sleep`ing a while ([`RETRY_AFTER`], doubled each time); [`RETRIES`] in a row that
/// bring nothing fail it ([`Failure::Network`]), as does the recording refused ([`Failure::Refused`]) or `sink`
/// refusing a write ([`Failure::Disk`]).
///
/// # Errors
/// Why it failed, as above.
pub async fn fetch<F: Future<Output = ()>>(
    core: &dyn Core,
    link: &str,
    sink: &mut impl Write,
    sleep: impl FnMut(Duration) -> F,
    told: impl FnMut(u64, u64),
) -> Result<(), Failure> {
    fetch_from(
        &|offset, len| core.file_stream(link.to_owned(), offset, len),
        sink,
        sleep,
        told,
    )
    .await
}

/// As [`fetch`], each stream opened by `open`.
async fn fetch_from<F: Future<Output = ()>>(
    open: &(dyn Fn(u64, u64) -> Reply<FileStream> + Send + Sync),
    sink: &mut impl Write,
    mut sleep: impl FnMut(Duration) -> F,
    mut told: impl FnMut(u64, u64),
) -> Result<(), Failure> {
    let mut written = 0_u64;
    let mut total: Option<u64> = None;
    let mut idle = 0_u32;
    while total.is_none_or(|total| written < total) {
        let before = written;
        match open(written, CHUNK).await {
            Ok(mut stream) if stream.offset == written => {
                total = Some(stream.total);
                while let Some(piece) = stream.next().await {
                    // a piece that failed ends the stream: it is opened again below
                    let Ok(piece) = piece else { break };
                    sink.write_all(&piece)
                        .map_err(|error| Failure::Disk(error.to_string()))?;
                    written += piece.len() as u64;
                    told(written, stream.total);
                }
            }
            Ok(_) | Err(EngineError::FileUnavailable) => return Err(Failure::Refused),
            Err(_) => {}
        }
        if total.is_some_and(|total| written >= total) {
            break;
        }
        if written > before {
            idle = 0;
            continue;
        }
        idle += 1;
        if idle > RETRIES {
            return Err(Failure::Network);
        }
        sleep(RETRY_AFTER * 2_u32.pow(idle - 1)).await;
    }
    sink.flush().map_err(|error| Failure::Disk(error.to_string()))
}

/// A size as the recap says it: "512 KB", "4.4 MB", "1.2 GB".
#[must_use]
pub fn byte_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    let tenths = |unit: u64| {
        let tenths = bytes.saturating_mul(10) / unit;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    match bytes {
        bytes if bytes < KB => format!("{bytes} B"),
        bytes if bytes < MB => format!("{} KB", bytes.div_ceil(KB)),
        bytes if bytes < GB => format!("{} MB", tenths(MB)),
        _ => format!("{} GB", tenths(GB)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_file_is_still_at_its_path_only_until_it_is_renamed_removed_or_replaced() {
        let folder = tempfile::tempdir().unwrap();
        let part = folder.path().join("Weekly sync.mp4.part");
        let file = fs::File::create(&part).unwrap();
        assert!(still_at(&file, &part).unwrap());

        let saved = folder.path().join("Weekly sync.mp4");
        fs::rename(&part, &saved).unwrap();
        assert!(!still_at(&file, &part).unwrap(), "renamed");
        fs::File::create(&part).unwrap();
        assert!(!still_at(&file, &part).unwrap(), "another file at its path");
        fs::remove_file(&part).unwrap();
        assert!(!still_at(&file, &part).unwrap(), "removed");
    }

    #[test]
    fn a_place_collides_with_itself_and_with_its_part_file_in_the_two_directions() {
        let report = Path::new("/saved/report");
        assert!(places_collide(report, report));
        assert!(places_collide(report, Path::new("/saved/report.part")));
        assert!(places_collide(Path::new("/saved/report.part"), report));
        assert!(!places_collide(report, Path::new("/saved/report.part.part")));
        assert!(!places_collide(report, Path::new("/saved/other")));
        assert!(!places_collide(report, Path::new("/other/report.part")));
    }

    #[cfg(unix)]
    #[test]
    fn a_place_collides_with_the_part_file_of_another_through_a_link_to_its_folder() {
        let chosen = tempfile::tempdir().unwrap();
        let folder = chosen.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let alias = chosen.path().join("alias");
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        let report = resolved(&folder.join("report"));
        assert!(places_collide(&report, &resolved(&alias.join("report"))));
        assert!(places_collide(&report, &resolved(&alias.join("report.part"))));
        assert!(places_collide(&resolved(&alias.join("report.part")), &report));
        assert!(!places_collide(&report, &resolved(&alias.join("other"))));
    }

    #[test]
    fn a_place_is_taken_by_one_job_until_that_job_ends() {
        let places = Places::default();
        let report = Path::new("/saved/report");
        assert!(places.take(report));
        assert!(!places.take(report), "the same place");
        assert!(!places.take(Path::new("/saved/report.part")), "its `.part` file");
        assert!(places.take(Path::new("/saved/other")));
        let job = Job {
            writing: Arc::new(AtomicBool::new(true)),
            places: places.clone(),
            place: Some(report.to_owned()),
            part: None,
        };
        drop(job);
        assert!(places.take(Path::new("/saved/report.part")), "let go as the job ended");
        assert!(
            !places.take(Path::new("/saved/other")),
            "the other job still has its place"
        );
    }

    #[test]
    fn a_dropped_job_keeps_the_file_that_another_writer_put_at_its_part_path() {
        let folder = tempfile::tempdir().unwrap();
        let part = folder.path().join("report.part");
        let file = fs::File::create(&part).unwrap();
        file.try_lock().unwrap();
        let job = Job {
            writing: Arc::new(AtomicBool::new(true)),
            places: Places::default(),
            place: None,
            part: Some((part.clone(), file)),
        };
        // another download saved its own file over it (a rename, which a lock does not stop)
        let other = folder.path().join("report.part.part");
        fs::write(&other, b"the other download").unwrap();
        fs::rename(&other, &part).unwrap();

        drop(job);
        assert_eq!(fs::read(&part).unwrap(), b"the other download");
    }

    #[test]
    fn a_dropped_job_removes_its_part_file_and_then_lets_its_lock_go() {
        let folder = tempfile::tempdir().unwrap();
        let part = folder.path().join("Weekly sync.mp4.part");
        let file = fs::File::create(&part).unwrap();
        file.try_lock().unwrap();
        let writing = Arc::new(AtomicBool::new(true));
        let job = Job {
            writing: Arc::clone(&writing),
            places: Places::default(),
            place: None,
            part: Some((part.clone(), file)),
        };
        let other = fs::OpenOptions::new().write(true).open(&part).unwrap();
        assert!(matches!(other.try_lock(), Err(fs::TryLockError::WouldBlock)));

        drop(job);
        assert!(!part.exists());
        assert!(!writing.load(Ordering::Acquire));
        // the lock went with the job's file
        other.try_lock().unwrap();
    }
}
