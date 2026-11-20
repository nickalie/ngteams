//! The sandbox's clipboard: content put on the nested niri's own selection, so a test or a lab run can make the app
//! paste a picture, text or files without the desktop's clipboard ever being read or written.
//!
//! A Wayland client of the nested niri's socket (never of `WAYLAND_DISPLAY`) takes the seat's selection through
//! wlroots' data-control protocol (`zwlr_data_control_manager_v1`), the way a clipboard manager does: it offers the
//! given MIME types and, whenever a client pastes, writes the bytes of the type asked for to the descriptor that
//! client handed over. The app sees an ordinary `wl_data_device` selection once its window has keyboard focus, as it
//! would after a copy in another program.
//!
//! An offer ([`Clipboard`]) is served on a thread of its own until a newer selection replaces it (the compositor
//! cancels it), it is dropped, or the compositor goes away. Each paste is written on a thread of its own, so a client
//! that asks and never reads holds up nothing else.
//!
//! [`read`] is the other side: what the selection holds now, each of its MIME types read in full, for a test to check
//! what the app copied or that an offer arrived.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{ErrorKind, Read as _, Write as _},
    os::{
        fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle,
    backend::{ObjectId, WaylandError},
    delegate_noop, event_created_child,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
    },
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::{SOCKET_WAIT, SandboxResult};

/// What a clipboard holds: each MIME type with its bytes, in the order offered.
pub type Offers = Vec<(String, Vec<u8>)>;

/// How often the serving thread looks whether it was stopped.
const POLL: Duration = Duration::from_millis(100);
/// Round trips [`read`] makes for the compositor to name the selection.
const SELECTION_TRIPS: usize = 5;

/// The client's state, for both sides: the bytes it serves and whether its offer ended; the MIME types of each offer
/// it was told of and the selection, once told.
#[derive(Default)]
struct State {
    served: Vec<(String, Arc<[u8]>)>,
    /// The source was cancelled or the device finished.
    ended: bool,
    types: HashMap<ObjectId, Vec<String>>,
    /// Whether the compositor named the selection yet.
    told: bool,
    /// The selection's offer, `None` for an empty clipboard.
    selection: Option<ZwlrDataControlOfferV1>,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwlrDataControlManagerV1);

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::Selection { id } => {
                // only the newest selection matters; the one before it is let go
                if let Some(old) = state.selection.take()
                    && id.as_ref() != Some(&old)
                {
                    state.types.remove(&old.id());
                    old.destroy();
                }
                state.told = true;
                state.selection = id;
            }
            zwlr_data_control_device_v1::Event::Finished => state.ended = true,
            _ => {}
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.types.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                // a type never offered gets nothing: the descriptor closes at once
                if let Some((_, bytes)) = state.served.iter().find(|(offered, _)| *offered == mime_type) {
                    let bytes = Arc::clone(bytes);
                    thread::spawn(move || write_all(fd, &bytes));
                }
            }
            zwlr_data_control_source_v1::Event::Cancelled => state.ended = true,
            _ => {}
        }
    }
}

/// Writes `bytes` to a paster's `fd`, blocking (a paster may hand over a non-blocking pipe), then closes it.
fn write_all(fd: OwnedFd, bytes: &[u8]) {
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is `fd`'s, open for this call; F_GETFL reads its status flags only.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags >= 0 && flags & libc::O_NONBLOCK != 0 {
        // SAFETY: as above; F_SETFL changes only its status flags.
        unsafe { libc::fcntl(raw, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }
    if let Err(error) = File::from(fd).write_all(bytes) {
        log::warn!("the sandbox clipboard could not write a paste: {error}");
    }
}

/// A connection to the nested niri at `socket`, its globals, the seat and the data-control manager.
fn connect(socket: &Path) -> SandboxResult<(Connection, EventQueue<State>, WlSeat, ZwlrDataControlManagerV1)> {
    let connection = Connection::from_socket(UnixStream::connect(socket)?)?;
    let (globals, queue): (GlobalList, EventQueue<State>) = registry_queue_init(&connection)?;
    let handle = queue.handle();
    let seat: WlSeat = globals.bind(&handle, 1..=1, ())?;
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&handle, 1..=1, ())
        .map_err(|error| format!("the compositor offers no zwlr_data_control_manager_v1: {error}"))?;
    Ok((connection, queue, seat, manager))
}

/// Checks `offers` before they are put up: at least one, each MIME type `type/subtype` and offered once.
fn check(offers: &Offers) -> SandboxResult {
    if offers.is_empty() {
        return Err("nothing to put on the clipboard".into());
    }
    for (index, (mime, _)) in offers.iter().enumerate() {
        let shaped = mime
            .split_once('/')
            .is_some_and(|(kind, sub)| !kind.is_empty() && !sub.is_empty())
            && !mime.contains(char::is_whitespace);
        if !shaped {
            return Err(format!("{mime:?} is not a MIME type").into());
        }
        if offers[..index].iter().any(|(earlier, _)| earlier == mime) {
            return Err(format!("{mime} is offered twice").into());
        }
    }
    Ok(())
}

/// Content on the sandbox's clipboard, served on its own thread; dropping it stops serving and, when it is still
/// the selection, empties the clipboard.
pub struct Clipboard {
    stop: Arc<AtomicBool>,
    serving: Option<thread::JoinHandle<()>>,
}

impl Clipboard {
    /// Takes the selection of the compositor at `socket` with `offers`, each MIME type answered with its bytes.
    ///
    /// # Errors
    /// `offers` is empty, a MIME type is malformed or offered twice, the compositor can't be reached or offers no
    /// data control, or it cancelled the offer at once.
    pub fn offer(socket: &Path, offers: Offers) -> SandboxResult<Self> {
        check(&offers)?;
        let (connection, mut queue, seat, manager) = connect(socket)?;
        let handle = queue.handle();
        let source = manager.create_data_source(&handle, ());
        for (mime, _) in &offers {
            source.offer(mime.clone());
        }
        let device = manager.get_data_device(&seat, &handle, ());
        device.set_selection(Some(&source));
        let mut state = State {
            served: offers.into_iter().map(|(mime, bytes)| (mime, bytes.into())).collect(),
            ..State::default()
        };
        // the compositor has the selection once this returns; a protocol error shows here
        queue.roundtrip(&mut state)?;
        if state.ended {
            return Err("the compositor cancelled the clipboard offer at once".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let serving = thread::Builder::new()
            .name("sandbox-clipboard".to_owned())
            .spawn(move || {
                serve(&connection, &mut queue, &mut state, &stopped);
                source.destroy();
                device.destroy();
                manager.destroy();
                let _ = connection.flush();
            })?;
        Ok(Self {
            stop,
            serving: Some(serving),
        })
    }

    /// Whether it is still served: no newer selection replaced it and the compositor is there.
    #[must_use]
    pub fn serving(&self) -> bool {
        self.serving.as_ref().is_some_and(|serving| !serving.is_finished())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Answers pastes until the offer ends, `stop` is set or the connection fails.
fn serve(connection: &Connection, queue: &mut EventQueue<State>, state: &mut State, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) && !state.ended {
        if let Err(error) = queue.dispatch_pending(state) {
            log::warn!("the sandbox clipboard lost the compositor: {error}");
            return;
        }
        if let Err(WaylandError::Io(error)) = connection.flush()
            && error.kind() != ErrorKind::WouldBlock
        {
            log::warn!("the sandbox clipboard lost the compositor: {error}");
            return;
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        if !readable(guard.connection_fd(), POLL) {
            continue;
        }
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                log::warn!("the sandbox clipboard lost the compositor: {error}");
                return;
            }
        }
    }
}

/// Whether `fd` has something to read (or hung up) within `wait`.
pub(crate) fn readable(fd: BorrowedFd<'_>, wait: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `poll` is one valid pollfd for this call, on a descriptor `fd` keeps open.
    let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
    ready > 0
}

/// Reads `pipe` to its end, giving up when nothing more comes within `wait`.
fn read_to_end(mut pipe: std::io::PipeReader, wait: Duration) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    let start = Instant::now();
    loop {
        let left = wait
            .checked_sub(start.elapsed())
            .ok_or("the clipboard's owner sent nothing in time")?;
        if !readable(pipe.as_fd(), left) {
            continue;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// What the selection of the compositor at `socket` holds now, each MIME type read in full, in the order its owner
/// offered them; empty when the clipboard is.
///
/// # Errors
/// The compositor can't be reached, offers no data control or never names the selection, or the selection's owner
/// doesn't send a type within [`SOCKET_WAIT`].
pub fn read(socket: &Path) -> SandboxResult<Offers> {
    let (connection, mut queue, seat, manager) = connect(socket)?;
    let handle = queue.handle();
    let device = manager.get_data_device(&seat, &handle, ());
    let mut state = State::default();
    for _ in 0..SELECTION_TRIPS {
        if state.told {
            break;
        }
        queue.roundtrip(&mut state)?;
    }
    if !state.told {
        return Err("the compositor never named the selection".into());
    }
    let mut offers = Vec::new();
    if let Some(offer) = state.selection.take() {
        let types = state.types.remove(&offer.id()).unwrap_or_default();
        for mime in types {
            let (reader, writer) = std::io::pipe()?;
            offer.receive(mime.clone(), writer.as_fd());
            connection.flush()?;
            drop(writer);
            offers.push((mime, read_to_end(reader, SOCKET_WAIT)?));
        }
        offer.destroy();
    }
    device.destroy();
    manager.destroy();
    connection.flush()?;
    Ok(offers)
}

/// The offers `clipboard <mime> <path> [<mime> <path> ...]` names: each file read whole under its MIME type.
///
/// # Errors
/// The words aren't MIME type and path pairs, a path isn't absolute (it would be read from wherever the runner
/// happens to run), or a file can't be read.
pub fn offers_from_files(words: &[&str]) -> SandboxResult<Offers> {
    if words.is_empty() || !words.len().is_multiple_of(2) {
        return Err("clipboard <mime> <path> [<mime> <path> ...]".into());
    }
    let offers = words
        .chunks(2)
        .map(|pair| match pair {
            [_, path] if !Path::new(path).is_absolute() => Err(format!("{path}: not an absolute path").into()),
            [mime, path] => fs::read(path)
                .map(|bytes| ((*mime).to_owned(), bytes))
                .map_err(|error| format!("{path}: {error}").into()),
            _ => Err("clipboard <mime> <path> [<mime> <path> ...]".into()),
        })
        .collect::<SandboxResult<Offers>>()?;
    check(&offers)?;
    Ok(offers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_are_checked_before_they_go_up() {
        let offer = |mime: &str| (mime.to_owned(), b"synthetic".to_vec());
        assert!(check(&vec![offer("image/png"), offer("text/plain;charset=utf-8")]).is_ok());
        for bad in [
            Vec::new(),
            vec![offer("png")],
            vec![offer("/png")],
            vec![offer("image/")],
            vec![offer("text/plain; charset=utf-8")],
            vec![offer("image/png"), offer("image/png")],
        ] {
            assert!(check(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_runner_s_words_name_files_by_type() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("synthetic.png");
        let text = dir.path().join("synthetic.txt");
        fs::write(&png, b"synthetic png bytes").unwrap();
        fs::write(&text, b"synthetic text").unwrap();
        let (png, text) = (png.to_string_lossy(), text.to_string_lossy());
        let offers = offers_from_files(&["image/png", &png, "text/plain", &text]).unwrap();
        assert_eq!(
            offers,
            [
                ("image/png".to_owned(), b"synthetic png bytes".to_vec()),
                ("text/plain".to_owned(), b"synthetic text".to_vec()),
            ]
        );
        let missing = dir.path().join("missing").to_string_lossy().into_owned();
        for bad in [
            vec![],
            vec!["image/png"],
            vec!["image/png", &png, "text/plain"],
            vec!["image/png", &missing],
            vec!["png", &png],
            vec!["image/png", "synthetic.png"],
        ] {
            assert!(offers_from_files(&bad).is_err(), "{bad:?}");
        }
    }
}
