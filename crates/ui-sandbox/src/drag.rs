//! A file manager's drag in the sandbox: a small surface on the nested niri's overlay layer that, pressed with the
//! sandbox's pointer ([`crate::input`]), starts a compositor drag offering the given MIME types and actions, as a
//! file manager's file does (Dolphin's URI lists, GTK's portal key beside its list, a sandboxed program's portal key
//! alone). Each type is answered with its bytes, after a delay a slow source has. What the drop target and the
//! compositor tell the source is recorded in order ([`DragSource::events`]): the type the target accepted, the
//! action, the reads, the drop and its end.
//!
//! The surface is a layer surface (`zwlr_layer_shell_v1`), so no window rule places it: it stays at its margins over
//! whatever window is under it, and takes no keyboard focus.

use std::{
    fs::File,
    io::{ErrorKind, Write as _},
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
    backend::WaylandError,
    delegate_noop, event_created_child,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::WlDataOffer,
        wl_data_source::{self, WlDataSource},
        wl_pointer::{self, ButtonState, WlPointer},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

pub use wayland_client::protocol::wl_data_device_manager::DndAction;

use crate::{SandboxResult, clipboard::Offers};

/// How often the serving thread looks whether it was stopped.
const POLL: Duration = Duration::from_millis(50);
/// Round trips the compositor gets to configure the surface.
const CONFIGURE_TRIPS: usize = 10;
/// The surface's colour: a light grey, opaque (ARGB).
const FILL: u32 = 0xffdd_dddd;

/// What a drag offers and where its surface is.
#[derive(Clone, Debug)]
pub struct DragOptions {
    /// Each MIME type with the bytes a read of it gets, in the order offered.
    pub offers: Offers,
    /// The actions the source allows (`wl_data_source.set_actions`).
    pub actions: DndAction,
    /// The surface's top-left corner on the output (its margins), and its size, in output pixels.
    pub at: (u32, u32),
    pub size: (u32, u32),
    /// How long the source takes before it answers a read, as a source that serializes its files slowly does.
    pub send_delay: Duration,
}

/// What the source was told, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragEvent {
    /// The press started the drag.
    Started,
    /// The target accepted this type (`None`: none).
    Target(Option<String>),
    /// The action the compositor chose (`wl_data_source.action`).
    Action(u32),
    /// The target read this type.
    Sent(String),
    /// The drop happened on a target that took it.
    DropPerformed,
    /// The target finished with the drop.
    Finished,
    /// The drag ended without a drop the target took.
    Cancelled,
}

/// The client's state.
struct State {
    offers: Arc<[(String, Arc<[u8]>)]>,
    actions: DndAction,
    send_delay: Duration,
    events: Arc<Mutex<Vec<DragEvent>>>,
    configured: Option<(u32, u32)>,
    closed: bool,
    surface: Option<WlSurface>,
    over_surface: bool,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    source: Option<WlDataSource>,
}

impl State {
    fn record(&self, event: DragEvent) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push(event);
    }

    /// Starts the drag the press with `serial` began, unless one runs.
    fn start(&mut self, serial: u32, handle: &QueueHandle<Self>) {
        let (Some(manager), Some(device), Some(surface)) = (&self.manager, &self.device, &self.surface) else {
            return;
        };
        if self.source.is_some() {
            return;
        }
        let source = manager.create_data_source(handle, ());
        for (mime, _) in self.offers.iter() {
            source.offer(mime.clone());
        }
        source.set_actions(self.actions);
        device.start_drag(Some(&source), surface, None, serial);
        self.source = Some(source);
        self.record(DragEvent::Started);
    }

    fn ended(&mut self, event: DragEvent) {
        self.record(event);
        if let Some(source) = self.source.take() {
            source.destroy();
        }
    }
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
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlDataOffer);
delegate_noop!(State: WlCompositor);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: WlDataDeviceManager);
delegate_noop!(State: ZwlrLayerShellV1);

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                state.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { surface, .. } => {
                state.over_surface = state.surface.as_ref() == Some(&surface);
            }
            wl_pointer::Event::Leave { .. } => state.over_surface = false,
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(ButtonState::Pressed),
                ..
            } if state.over_surface => state.start(serial, handle),
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // the clipboard's offers come here too; the source has no use for them
        if let wl_data_device::Event::DataOffer { id } = event {
            id.destroy();
        }
    }

    event_created_child!(State, WlDataDevice, [
        wayland_client::protocol::wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Target { mime_type } => state.record(DragEvent::Target(mime_type)),
            wl_data_source::Event::Action { dnd_action } => {
                let bits = match dnd_action {
                    WEnum::Value(action) => action.bits(),
                    WEnum::Unknown(bits) => bits,
                };
                state.record(DragEvent::Action(bits));
            }
            wl_data_source::Event::Send { mime_type, fd } => {
                state.record(DragEvent::Sent(mime_type.clone()));
                // a type never offered gets nothing: the descriptor closes at once
                if let Some((_, bytes)) = state.offers.iter().find(|(offered, _)| *offered == mime_type) {
                    let bytes = Arc::clone(bytes);
                    let delay = state.send_delay;
                    thread::spawn(move || {
                        thread::sleep(delay);
                        write_all(fd, &bytes);
                    });
                }
            }
            wl_data_source::Event::DndDropPerformed => state.record(DragEvent::DropPerformed),
            wl_data_source::Event::DndFinished => state.ended(DragEvent::Finished),
            wl_data_source::Event::Cancelled => state.ended(DragEvent::Cancelled),
            _ => {}
        }
    }
}

/// Writes `bytes` to a reader's `fd`, then closes it.
fn write_all(fd: OwnedFd, bytes: &[u8]) {
    if let Err(error) = File::from(fd).write_all(bytes) {
        log::warn!("the sandbox drag source could not answer a read: {error}");
    }
}

/// The source's surface on the overlay layer, at its margins and size, taking no keyboard focus; committed.
fn layer_surface(
    layers: &ZwlrLayerShellV1,
    surface: &WlSurface,
    options: &DragOptions,
    handle: &QueueHandle<State>,
) -> SandboxResult<ZwlrLayerSurfaceV1> {
    let layer = layers.get_layer_surface(
        surface,
        None,
        Layer::Overlay,
        "ngteams-file-source".to_owned(),
        handle,
        (),
    );
    let (width, height) = options.size;
    let (left, top) = options.at;
    layer.set_size(width, height);
    layer.set_anchor(Anchor::Top | Anchor::Left);
    layer.set_margin(i32::try_from(top)?, 0, 0, i32::try_from(left)?);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(-1);
    surface.commit();
    Ok(layer)
}

/// The size the compositor configured the surface to, `asked` where it left the choice to the source.
fn configured(queue: &mut EventQueue<State>, state: &mut State, asked: (u32, u32)) -> SandboxResult<(u32, u32)> {
    for _ in 0..CONFIGURE_TRIPS {
        if state.configured.is_some() || state.closed {
            break;
        }
        queue.roundtrip(state)?;
    }
    let Some((width, height)) = state.configured.filter(|_| !state.closed) else {
        return Err("the compositor never configured the drag source's surface".into());
    };
    Ok((
        if width == 0 { asked.0 } else { width },
        if height == 0 { asked.1 } else { height },
    ))
}

/// A shared-memory buffer of `width` by `height` pixels, all [`FILL`].
fn buffer(shm: &WlShm, (width, height): (u32, u32), handle: &QueueHandle<State>) -> SandboxResult<WlBuffer> {
    let stride = width.checked_mul(4).ok_or("the drag source is too wide")?;
    let len = stride.checked_mul(height).ok_or("the drag source is too large")?;
    let pixels: Vec<u8> = FILL.to_le_bytes().repeat(usize::try_from(width * height)?);
    // SAFETY: the name is a valid NUL-terminated string; memfd_create only makes a new descriptor.
    let raw = unsafe { libc::memfd_create(c"ngteams-drag-source".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `raw` is a new descriptor this call owns from here.
    let mut file = unsafe { <File as std::os::fd::FromRawFd>::from_raw_fd(raw) };
    file.write_all(&pixels)?;
    let pool = shm.create_pool(file.as_fd(), i32::try_from(len)?, handle, ());
    let buffer = pool.create_buffer(
        0,
        i32::try_from(width)?,
        i32::try_from(height)?,
        i32::try_from(stride)?,
        Format::Argb8888,
        handle,
        (),
    );
    pool.destroy();
    Ok(buffer)
}

/// Checks `offers` before a drag: at least one, each type a name without spaces (a MIME type, or a legacy name such as
/// GTK's `UTF8_STRING`), offered once.
fn check(offers: &Offers) -> SandboxResult {
    if offers.is_empty() {
        return Err("a drag offers nothing".into());
    }
    for (index, (mime, _)) in offers.iter().enumerate() {
        if mime.is_empty() || mime.contains(char::is_whitespace) {
            return Err(format!("{mime:?} is not a type a drag can offer").into());
        }
        if offers[..index].iter().any(|(earlier, _)| earlier == mime) {
            return Err(format!("{mime} is offered twice").into());
        }
    }
    Ok(())
}

/// A file manager's drag source on the sandbox's screen, served on its own thread until it is dropped.
pub struct DragSource {
    stop: Arc<AtomicBool>,
    serving: Option<thread::JoinHandle<()>>,
    events: Arc<Mutex<Vec<DragEvent>>>,
}

impl DragSource {
    /// Shows the source's surface on the compositor at `socket`; a press on it starts the drag.
    ///
    /// # Errors
    /// The offers are malformed, the compositor can't be reached, lacks a global the source needs, or never
    /// configures the surface.
    pub fn show(socket: &Path, options: DragOptions) -> SandboxResult<Self> {
        check(&options.offers)?;
        let connection = Connection::from_socket(UnixStream::connect(socket)?)?;
        let (globals, mut queue): (GlobalList, EventQueue<State>) = registry_queue_init(&connection)?;
        let handle = queue.handle();
        let compositor: WlCompositor = globals.bind(&handle, 4..=6, ())?;
        let shm: WlShm = globals.bind(&handle, 1..=1, ())?;
        let seat: WlSeat = globals.bind(&handle, 5..=7, ())?;
        let manager: WlDataDeviceManager = globals.bind(&handle, 3..=3, ())?;
        let layers: ZwlrLayerShellV1 = globals
            .bind(&handle, 1..=4, ())
            .map_err(|error| format!("the compositor offers no zwlr_layer_shell_v1: {error}"))?;
        let surface = compositor.create_surface(&handle, ());
        let layer = layer_surface(&layers, &surface, &options, &handle)?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut state = State {
            offers: options
                .offers
                .into_iter()
                .map(|(mime, bytes)| (mime, bytes.into()))
                .collect(),
            actions: options.actions,
            send_delay: options.send_delay,
            events: Arc::clone(&events),
            configured: None,
            closed: false,
            surface: Some(surface.clone()),
            over_surface: false,
            device: Some(manager.get_data_device(&seat, &handle, ())),
            manager: Some(manager),
            source: None,
        };
        let _pointer = seat.get_pointer(&handle, ());
        let size = configured(&mut queue, &mut state, options.size)?;
        let buffer = buffer(&shm, size, &handle)?;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, i32::try_from(size.0)?, i32::try_from(size.1)?);
        surface.commit();
        queue.roundtrip(&mut state)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let serving = thread::Builder::new()
            .name("sandbox-drag-source".to_owned())
            .spawn(move || {
                serve(&connection, &mut queue, &mut state, &stopped);
                if let Some(source) = state.source.take() {
                    source.destroy();
                }
                layer.destroy();
                surface.destroy();
                buffer.destroy();
                let _ = connection.flush();
            })?;
        Ok(Self {
            stop,
            serving: Some(serving),
            events,
        })
    }

    /// What the source was told so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<DragEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for DragSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Answers the compositor until `stop` is set, the surface is closed or the connection fails.
fn serve(connection: &Connection, queue: &mut EventQueue<State>, state: &mut State, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) && !state.closed {
        if let Err(error) = queue.dispatch_pending(state) {
            log::warn!("the sandbox drag source lost the compositor: {error}");
            return;
        }
        if let Err(WaylandError::Io(error)) = connection.flush()
            && error.kind() != ErrorKind::WouldBlock
        {
            log::warn!("the sandbox drag source lost the compositor: {error}");
            return;
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        if !crate::clipboard::readable(guard.connection_fd(), POLL) {
            continue;
        }
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                log::warn!("the sandbox drag source lost the compositor: {error}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_offers_each_type_once_with_no_spaces() {
        let offer = |mime: &str| (mime.to_owned(), b"synthetic".to_vec());
        assert!(check(&vec![offer("text/uri-list"), offer("UTF8_STRING")]).is_ok());
        for bad in [
            Vec::new(),
            vec![offer("")],
            vec![offer("text/plain; charset=utf-8")],
            vec![offer("text/uri-list"), offer("text/uri-list")],
        ] {
            assert!(check(&bad).is_err(), "{bad:?}");
        }
    }
}
