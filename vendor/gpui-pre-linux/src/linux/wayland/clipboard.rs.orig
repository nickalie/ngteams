use std::{
    collections::hash_map::RandomState,
    fs::File,
    hash::BuildHasher as _,
    io::{ErrorKind, Write},
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
};

use calloop::{LoopHandle, PostAction};
use filedescriptor::Pipe;
use strum::IntoEnumIterator;
use wayland_client::{Connection, protocol::wl_data_offer::WlDataOffer};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;

use crate::linux::platform::{PIPE_READ_TIMEOUT, read_fd_with_timeout};
use gpui::{ClipboardEntry, ClipboardItem, ExternalPaths, Image, ImageFormat, hash};
use http_client::Url;
use smallvec::SmallVec;
use std::path::PathBuf;

/// Text mime types that we'll offer to other programs.
pub(crate) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
pub(crate) const FILE_LIST_MIME_TYPE: &str = "text/uri-list";

/// Text mime types that we'll accept from other programs.
pub(crate) const ALLOWED_TEXT_MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];

/// ngteams: the HTML a rich-text paste reads beside the text.
const HTML_MIME_TYPE: &str = "text/html";

pub(crate) struct Clipboard {
    connection: Connection,
    loop_handle: LoopHandle<'static, ()>,

    selection: Selection<WlDataOffer>,
    primary: Selection<ZwpPrimarySelectionOfferV1>,
}

/// A local write is immediately readable, before the compositor echoes its offer. Authoritative replacements and
/// cancellation revoke that shortcut. Reads are withheld while unfocused, without dropping an offer between this
/// client's surfaces. Per-copy markers allow a manager's returned text to recover private rich metadata safely.
struct Selection<T: ReceiveData> {
    /// Last local snapshot, retained privately until the next copy; a cancelled source cannot serve it.
    contents: Option<ClipboardItem>,
    cached_read: Option<ClipboardItem>,
    current_offer: Option<DataOffer<T>>,
    mime_prefix: String,
    local_mime: Option<String>,
    generation: u64,
    local_source: Option<u64>,
    pending: bool,
}

impl<T: ReceiveData> Selection<T> {
    fn new(mime_prefix: String) -> Self {
        Self {
            contents: None,
            cached_read: None,
            current_offer: None,
            mime_prefix,
            local_mime: None,
            generation: 0,
            local_source: None,
            pending: false,
        }
    }
    fn set(&mut self, item: ClipboardItem) -> (u64, String) {
        self.generation = self.generation.wrapping_add(1);
        let mime = format!("{}-{}", self.mime_prefix, self.generation);
        self.local_mime = Some(mime.clone());
        self.contents = Some(item);
        self.cached_read = None;
        self.local_source = Some(self.generation);
        self.pending = true;
        (self.generation, mime)
    }

    fn set_offer(&mut self, offer: Option<DataOffer<T>>) {
        if offer.is_none() {
            self.clear_offer();
            return;
        }
        // Several copies can be queued before their echoes arrive. An older echo from our own selection must
        // not discard the immediate-read shortcut for the newest request.
        let older_echo = offer.as_ref().is_some_and(|offer| {
            !self.local_mime.as_ref().is_some_and(|mime| offer.has_mime_type(mime))
                && offer.mime_types.iter().any(|mime| {
                    mime.strip_prefix(&self.mime_prefix)
                        .and_then(|suffix| suffix.strip_prefix('-'))
                        .and_then(|generation| generation.parse::<u64>().ok())
                        .is_some_and(|generation| generation < self.generation)
                })
        });
        self.pending &= older_echo;
        self.cached_read = None;
        self.current_offer = offer;
    }

    fn clear_offer(&mut self) {
        self.pending = false;
        self.cached_read = None;
        self.current_offer = None;
    }

    fn cancelled(&mut self, source: u64) {
        if source == self.generation {
            self.pending = false;
            self.local_source = None;
            self.cached_read = None;
            // Clipboard managers may re-offer our private MIME along with the copied text. The current offer
            // can already be their replacement, so revoke the local shortcut without discarding that offer.
        }
    }

    fn contents_for_source(&self, source: u64) -> Option<&ClipboardItem> {
        (self.local_source == Some(source))
            .then_some(self.contents.as_ref())
            .flatten()
    }

    fn read(&mut self, connection: &Connection, focused: bool) -> Option<ClipboardItem> {
        if !focused {
            return None;
        }
        if self.pending {
            return self.contents.clone();
        }
        let offer = self.current_offer.as_ref()?;
        let exact_local_marker = self.local_mime.as_ref().is_some_and(|mime| offer.has_mime_type(mime));
        if exact_local_marker && self.local_source == Some(self.generation) {
            if let Some(contents) = self.contents.clone() {
                return Some(contents);
            }
        }
        if let Some(cached) = self.cached_read.clone() {
            return Some(cached);
        }
        // ngteams: every form the offer holds, so a paste can choose among them: its text (with its HTML), its
        // picture and the files it names (a file manager's copy)
        let mut entries = Vec::new();
        if let Some(text) = offer.read_text(connection) {
            entries.extend(text.entries);
        }
        if let Some(image) = offer.read_image(connection) {
            entries.extend(image.entries);
        }
        if let Some(paths) = offer.read_paths(connection) {
            entries.push(ClipboardEntry::ExternalPaths(paths));
        }
        if entries.is_empty() {
            return None;
        }
        let mut item = ClipboardItem { entries };
        // A manager may persist all advertised MIME types. Keep rich metadata private, but recover the last
        // local snapshot only for its exact per-copy marker and the text actually supplied by that offer.
        if exact_local_marker {
            if let Some(local) = self.contents.as_ref() {
                if item.text().is_some_and(|text| Some(text) == local.text()) {
                    item = local.clone();
                }
            }
        }
        self.cached_read = Some(item.clone());
        Some(item)
    }
}

pub(crate) trait ReceiveData {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>);
}

impl ReceiveData for WlDataOffer {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

impl ReceiveData for ZwpPrimarySelectionOfferV1 {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

#[derive(Clone, Debug)]
/// Wrapper for `WlDataOffer` and `ZwpPrimarySelectionOfferV1`, used to help track mime types.
pub(crate) struct DataOffer<T: ReceiveData> {
    pub inner: T,
    mime_types: Vec<String>,
}

impl<T: ReceiveData> DataOffer<T> {
    pub fn new(offer: T) -> Self {
        Self {
            inner: offer,
            mime_types: Vec::new(),
        }
    }

    pub fn add_mime_type(&mut self, mime_type: String) {
        self.mime_types.push(mime_type)
    }

    fn has_mime_type(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|t| t == mime_type)
    }

    fn read_bytes(&self, connection: &Connection, mime_type: &str) -> Option<Vec<u8>> {
        let pipe = Pipe::new().unwrap();
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);

        connection.flush().unwrap();

        match read_fd_with_timeout(fd, PIPE_READ_TIMEOUT) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    fn read_text(&self, connection: &Connection) -> Option<ClipboardItem> {
        let mime_type = self.mime_types.iter().find(|&mime_type| {
            ALLOWED_TEXT_MIME_TYPES
                .iter()
                .any(|&allowed| allowed == mime_type)
        })?;
        let bytes = self.read_bytes(connection, mime_type)?;
        let text_content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(e) => {
                log::error!("Failed to convert clipboard content to UTF-8: {}", e);
                return None;
            }
        };

        // Normalize the text to unix line endings, otherwise
        // copying from eg: firefox inserts a lot of blank
        // lines, and that is super annoying.
        let result = text_content.replace("\r\n", "\n");
        // ngteams: the HTML another program offers beside its text, kept as the item's metadata
        // (`{"text/html": "<html>"}`) for a rich-text editor's paste
        if let Some(html) = self.read_html(connection) {
            return Some(ClipboardItem::new_string_with_json_metadata(
                result,
                std::collections::HashMap::from([(HTML_MIME_TYPE, html)]),
            ));
        }
        Some(ClipboardItem::new_string(result))
    }

    /// The `text/html` the offer holds, as UTF-8 (a UTF-16 one, with its byte order mark, decoded).
    fn read_html(&self, connection: &Connection) -> Option<String> {
        if !self.has_mime_type(HTML_MIME_TYPE) {
            return None;
        }
        let bytes = self.read_bytes(connection, HTML_MIME_TYPE)?;
        match bytes.as_slice() {
            [0xff, 0xfe, rest @ ..] => {
                let units: Vec<u16> = rest
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16(&units).ok()
            }
            [0xfe, 0xff, rest @ ..] => {
                let units: Vec<u16> = rest
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16(&units).ok()
            }
            _ => String::from_utf8(bytes).ok(),
        }
    }

    /// ngteams: the local files a `text/uri-list` names (`file://` URIs; comments and other schemes left out).
    fn read_paths(&self, connection: &Connection) -> Option<ExternalPaths> {
        if !self.has_mime_type(FILE_LIST_MIME_TYPE) {
            return None;
        }
        let bytes = self.read_bytes(connection, FILE_LIST_MIME_TYPE)?;
        let list = String::from_utf8(bytes).ok()?;
        let paths: SmallVec<[PathBuf; 2]> = list
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| Url::parse(line).ok())
            .filter(|url| url.scheme() == "file")
            .filter_map(|url| url.to_file_path().ok())
            .collect();
        (!paths.is_empty()).then(|| ExternalPaths(paths))
    }

    fn read_image(&self, connection: &Connection) -> Option<ClipboardItem> {
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }

            if let Some(bytes) = self.read_bytes(connection, mime_type) {
                let id = hash(&bytes);
                return Some(ClipboardItem {
                    entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
                });
            }
        }
        None
    }
}

impl Clipboard {
    pub fn new(connection: Connection, loop_handle: LoopHandle<'static, ()>) -> Self {
        // This public association token contains no clipboard payload. RandomState's instance salt avoids
        // confusing a manager's history from a prior process with a new copy after PID reuse.
        let random = RandomState::new();
        let prefix = format!(
            "application/x-gpui-{:016x}{:016x}",
            random.hash_one(0_u8),
            random.hash_one(1_u8)
        );
        Self {
            connection,
            loop_handle,
            selection: Selection::new(format!("{prefix}-clipboard")),
            primary: Selection::new(format!("{prefix}-primary")),
        }
    }

    pub fn set(&mut self, item: ClipboardItem) -> (u64, String) {
        self.selection.set(item)
    }

    pub fn set_primary(&mut self, item: ClipboardItem) -> (u64, String) {
        self.primary.set(item)
    }

    pub fn set_offer(&mut self, data_offer: Option<DataOffer<WlDataOffer>>) {
        self.selection.set_offer(data_offer);
    }

    pub fn set_primary_offer(&mut self, data_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>) {
        self.primary.set_offer(data_offer);
    }

    pub fn cancelled(&mut self, source: u64) {
        self.selection.cancelled(source);
    }

    pub fn primary_cancelled(&mut self, source: u64) {
        self.primary.cancelled(source);
    }

    pub fn send(&self, source: u64, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self
            .selection
            .contents_for_source(source)
            .and_then(|contents| contents.text())
        {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn send_primary(&self, source: u64, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self
            .primary
            .contents_for_source(source)
            .and_then(|contents| contents.text())
        {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn read(&mut self, focused: bool) -> Option<ClipboardItem> {
        self.selection.read(&self.connection, focused)
    }

    pub fn read_primary(&mut self, focused: bool) -> Option<ClipboardItem> {
        self.primary.read(&self.connection, focused)
    }

    pub fn send_bytes(&self, fd: OwnedFd, bytes: Vec<u8>) {
        let mut written = 0;
        self.loop_handle
            .insert_source(
                calloop::generic::Generic::new(
                    File::from(fd),
                    calloop::Interest::WRITE,
                    calloop::Mode::Level,
                ),
                move |_, file, _| {
                    let file = unsafe { file.get_mut() };
                    loop {
                        match file.write(&bytes[written..]) {
                            Ok(n) if written + n == bytes.len() => {
                                written += n;
                                break Ok(PostAction::Remove);
                            }
                            Ok(n) => written += n,
                            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                                break Ok(PostAction::Continue);
                            }
                            Err(_) => break Ok(PostAction::Remove),
                        }
                    }
                },
            )
            .unwrap();
    }
}

#[cfg(test)]
mod local_selection_tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    const PREFIX: &str = "application/x-gpui-synthetic-clipboard";
    const OLD_METADATA: &str = r#"{"synthetic":true,"rich":"bold","mention":"synthetic-old"}"#;
    const NEW_METADATA: &str = r#"{"synthetic":true,"rich":"italic","mention":"synthetic-new"}"#;

    #[derive(Clone)]
    struct Offer(&'static str);

    impl ReceiveData for Offer {
        fn receive_data(&self, _: String, fd: BorrowedFd<'_>) {
            let mut file = File::from(fd.try_clone_to_owned().unwrap());
            file.write_all(self.0.as_bytes()).unwrap();
        }
    }

    fn connection() -> (Connection, UnixStream) {
        // No compositor or GUI: the fake offer writes a real pipe synchronously, and this socket only supplies
        // the Connection required by the shared read path's flush.
        let (client, server) = UnixStream::pair().unwrap();
        (Connection::from_socket(client).unwrap(), server)
    }

    fn selection() -> Selection<Offer> {
        Selection::new(PREFIX.to_owned())
    }

    fn offer(text: &'static str, marker: Option<&str>) -> DataOffer<Offer> {
        let mut offer = DataOffer::new(Offer(text));
        offer.add_mime_type(TEXT_MIME_TYPES[0].to_owned());
        if let Some(marker) = marker {
            offer.add_mime_type(marker.to_owned());
        }
        offer
    }

    fn text(selection: &mut Selection<Offer>, connection: &Connection) -> Option<String> {
        selection.read(connection, true).and_then(|item| item.text())
    }

    #[test]
    fn a_copy_is_readable_before_the_compositor_echoes_its_offer() {
        let (connection, _server) = connection();
        let mut selection = selection();
        assert_eq!(text(&mut selection, &connection), None);
        let (first, first_marker) = selection.set(ClipboardItem::new_string("first".into()));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("first"));
        let (second, second_marker) = selection.set(ClipboardItem::new_string("second".into()));
        selection.set_offer(Some(offer("first", Some(&first_marker))));
        selection.cancelled(first);
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("second"));
        selection.set_offer(Some(offer("unused own bytes", Some(&second_marker))));
        selection.cancelled(first);
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("second"));
        selection.cancelled(second);
        selection.set_offer(None);
        assert_eq!(text(&mut selection, &connection), None);
    }

    #[test]
    fn a_copy_overrides_a_cached_external_read_until_an_authoritative_replacement() {
        let (connection, _server) = connection();
        let mut selection = selection();
        selection.set_offer(Some(offer("outside", None)));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("outside"));
        let (source, _) = selection.set(ClipboardItem::new_string("inside".into()));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("inside"));
        selection.set_offer(Some(offer("new outside", None)));
        selection.cancelled(source);
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("new outside"));
        assert!(selection.contents_for_source(source).is_none());
        selection.set_offer(None);
        assert_eq!(text(&mut selection, &connection), None);
    }

    #[test]
    fn unfocused_reads_are_denied_without_losing_a_same_client_surface_transition() {
        let (connection, _server) = connection();
        let mut selection = selection();
        let (source, marker) = selection.set(ClipboardItem::new_string("local".into()));
        assert!(selection.read(&connection, false).is_none());
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("local"));
        selection.set_offer(Some(offer("unused own bytes", Some(&marker))));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("local"));
        assert!(selection.read(&connection, false).is_none());
        selection.cancelled(source);
        selection.set_offer(Some(offer("persisted local", Some(&marker))));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("persisted local"));
        selection.set_offer(Some(offer("foreign", None)));
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("foreign"));
        assert!(selection.read(&connection, false).is_none());
        assert_eq!(text(&mut selection, &connection).as_deref(), Some("foreign"));
    }

    #[test]
    fn an_old_source_cannot_send_the_next_copys_contents() {
        let mut selection = selection();
        let (first, _) = selection.set(ClipboardItem::new_string("first".into()));
        let (second, _) = selection.set(ClipboardItem::new_string("second".into()));
        assert!(selection.contents_for_source(first).is_none());
        assert_eq!(
            selection
                .contents_for_source(second)
                .and_then(|item| item.text())
                .as_deref(),
            Some("second")
        );
        selection.cancelled(first);
        assert!(selection.contents_for_source(second).is_some());
        selection.cancelled(second);
        assert!(selection.contents_for_source(second).is_none());
    }

    #[test]
    fn a_clipboard_manager_can_reoffer_the_private_mime_before_or_after_cancellation() {
        let (connection, _server) = connection();
        for replacement_first in [true, false] {
            let mut selection = selection();
            let (source, marker) = selection.set(ClipboardItem::new_string("original local".into()));
            selection.set_offer(Some(offer("unused own bytes", Some(&marker))));
            if replacement_first {
                selection.set_offer(Some(offer("manager's changed text", Some(&marker))));
            }
            selection.cancelled(source);
            if !replacement_first {
                selection.set_offer(Some(offer("manager's changed text", Some(&marker))));
            }
            assert_eq!(
                text(&mut selection, &connection).as_deref(),
                Some("manager's changed text")
            );
            assert!(selection.contents_for_source(source).is_none());
            selection.set_offer(None);
            assert_eq!(text(&mut selection, &connection), None);
        }
    }

    #[test]
    fn the_latest_manager_copy_keeps_private_rich_metadata_after_validating_its_text() {
        let (connection, _server) = connection();
        for replacement_first in [true, false] {
            let mut selection = selection();
            let (source, marker) = selection.set(ClipboardItem::new_string_with_metadata(
                "copied text".into(),
                NEW_METADATA.into(),
            ));
            selection.set_offer(Some(offer("unused own bytes", Some(&marker))));
            if replacement_first {
                selection.set_offer(Some(offer("copied text", Some(&marker))));
            }
            selection.cancelled(source);
            if !replacement_first {
                selection.set_offer(Some(offer("copied text", Some(&marker))));
            }
            let item = selection.read(&connection, true).unwrap();
            assert_eq!(item.text().as_deref(), Some("copied text"));
            assert_eq!(item.metadata().map(String::as_str), Some(NEW_METADATA));
            assert!(
                selection.contents_for_source(source).is_none(),
                "the retained snapshot is never served after cancellation"
            );
            assert!(!marker.contains("mention"));
        }
    }

    #[test]
    fn history_changed_text_another_selection_or_process_cannot_recover_the_latest_metadata() {
        let (connection, _server) = connection();
        let mut selection = selection();
        let (first, old_marker) = selection.set(ClipboardItem::new_string_with_metadata(
            "same text".into(),
            OLD_METADATA.into(),
        ));
        selection.cancelled(first);
        let (second, new_marker) = selection.set(ClipboardItem::new_string_with_metadata(
            "same text".into(),
            NEW_METADATA.into(),
        ));
        selection.cancelled(second);
        selection.set_offer(Some(offer("same text", Some(&old_marker))));
        let old = selection.read(&connection, true).unwrap();
        assert_eq!(old.text().as_deref(), Some("same text"));
        assert!(
            old.metadata().is_none(),
            "an old copy with identical text must not acquire the newer mention mapping"
        );
        selection.set_offer(Some(offer("changed text", Some(&new_marker))));
        let changed = selection.read(&connection, true).unwrap();
        assert_eq!(changed.text().as_deref(), Some("changed text"));
        assert!(changed.metadata().is_none());
        for prefix in [
            "application/x-gpui-synthetic-primary",
            "application/x-gpui-another-instance-clipboard",
        ] {
            let mut other: Selection<Offer> = Selection::new(prefix.into());
            let (_, marker) = other.set(ClipboardItem::new_string_with_metadata(
                "same text".into(),
                OLD_METADATA.into(),
            ));
            selection.set_offer(Some(offer("same text", Some(&marker))));
            assert!(selection.read(&connection, true).unwrap().metadata().is_none());
        }
        selection.set_offer(Some(offer("same text", Some(&new_marker))));
        assert_eq!(
            selection
                .read(&connection, true)
                .unwrap()
                .metadata()
                .map(String::as_str),
            Some(NEW_METADATA)
        );
    }
}
