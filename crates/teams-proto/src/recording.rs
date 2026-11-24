//! A meeting recording streamed as Teams' Recap player streams it (`docs/protocol/recap.md`): its DASH presentation
//! from the item's own site, and its parts from the media transform service.
//!
//! - **The presentation** is a read on the personal site that holds the recording ([`RecordingManifest`]):
//!   `GET <site>/_api/v2.1/drives/<drive>/items/<item>/content?format=dash` with that site's token and
//!   `prefer: noredirect` (the manifest itself, not a redirect to it), `enablecdn: 0` (the transform service's own
//!   parts, in the clear, rather than its CDN's, which are encrypted with a key the player's page hands out) and
//!   `applymediaedits: true`, as the web client's player asks it. It works for anyone who may watch the recording:
//!   a recording whose download is blocked (an attendee's view of another's meeting) is refused at its download link
//!   but streamed here.
//! - **The answer** is an MPD ([`read_manifest`]): one period, an audio and a video adaptation set, each a
//!   `SegmentTemplate` with a `SegmentTimeline` and a representation; its `BaseURL` the transform service
//!   (`https://<region>-mediap.svc.ms/transform/`), whose parts' addresses carry their own signed credential. A
//!   presentation that says its parts are encrypted (`ContentProtection`), or whose parts are anywhere else, isn't read.
//! - **A part** (an initialization segment, a media segment: fragmented MP4) is a `GET` of its address with no token of
//!   the account's ([`MediaSegmentLink`], [`crate::HttpTransport::media_segment`]).
//! - **The file** ([`RecordingFile`]): the item read on the same site with that site's token,
//!   `GET <site>/_api/v2.1/drives/<drive>/items/<item>?$select=name,size,accessViewpoint`, as the Recap player reads it
//!   (it selects `accessViewpoint` among the item's other fields): whether the account may download the recording
//!   (`accessViewpoint.canDownload`), its file name and size ([`read_recording_file`]). Its bytes are read at the
//!   download link Graph hands out for its sharing link, as any shared file's ([`crate::FileDownload`]).
//! - **When it expires** is a read of its item on the same site ([`RecordingExpiration`]):
//!   `GET <site>/_api/v2.1/drives/<drive>/items/<item>?$select=expiration`, as the web client reads it for the meeting
//!   chat's recap card ("Expires in 103 days"); its answer's `expiration.expirationDateTime` ([`read_expiration`]).

use std::fmt;

use url::Url;

use crate::attachment::DriveSite;

/// The longest part address taken: the transform service's carry a signed item address and a session's data.
pub const MAX_MEDIA_LINK: usize = 16 * 1024;

/// The most segments one track is read with (about 33 hours of 6 s segments).
pub const MAX_SEGMENTS: usize = 20_000;

/// The suffix of the transform service's hosts (`southcentralus1-mediap.svc.ms`).
pub const MEDIA_HOST_SUFFIX: &str = "-mediap.svc.ms";

/// The headers the presentation's read goes with (see the module docs).
pub const MANIFEST_HEADERS: [(&str, &str); 3] = [
    ("prefer", "noredirect"),
    ("enablecdn", "0"),
    ("applymediaedits", "true"),
];

/// Where a recording's DASH presentation is read: on the personal site that holds its item, with that site's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingManifest {
    site: DriveSite,
    url: String,
}

impl RecordingManifest {
    /// The presentation of item `item` of drive `drive`, on the personal site `link` is on (the recording's sharing
    /// link, `https://<tenant>-my.sharepoint.com/:v:/g/personal/<owner>/<token>`, or any address on that site);
    /// `None` for a link off a personal site, or an id that isn't one.
    #[must_use]
    pub fn of(link: &str, drive: &str, item: &str) -> Option<Self> {
        let (site, item) = item_address(link, drive, item)?;
        let url = format!("{item}/content?format=dash");
        Some(Self { site, url })
    }

    /// The personal site the recording is on: its token's audience.
    #[must_use]
    pub fn site(&self) -> &DriveSite {
        &self.site
    }

    /// The address its presentation is read at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// The address of item `item` of drive `drive` on the personal site `link` is on
/// (`<site>_api/v2.1/drives/<drive>/items/<item>`), with that site; `None` for a link off a personal site, or an id that
/// isn't one.
fn item_address(link: &str, drive: &str, item: &str) -> Option<(DriveSite, String)> {
    let plain = |part: &str| {
        !part.is_empty()
            && part.len() <= 256
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'!' | b'-' | b'_'))
    };
    if !(plain(drive) && plain(item)) {
        return None;
    }
    let site = personal_site(link)?;
    let address = format!("{}_api/v2.1/drives/{drive}/items/{item}", site.base());
    Some((site, address))
}

/// Where a recording's file is read: its item on the personal site that holds it, with that site's token (see the
/// module docs); its answer is read by [`read_recording_file`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingFile {
    site: DriveSite,
    url: String,
}

/// The fields [`RecordingFile`]'s read selects.
pub const RECORDING_FILE_SELECT: &str = "name,size,accessViewpoint";

impl RecordingFile {
    /// The file of item `item` of drive `drive`, on the personal site `link` is on (as [`RecordingManifest::of`]);
    /// `None` for a link off a personal site, or an id that isn't one.
    #[must_use]
    pub fn of(link: &str, drive: &str, item: &str) -> Option<Self> {
        let (site, item) = item_address(link, drive, item)?;
        let url = format!("{item}?$select={RECORDING_FILE_SELECT}");
        Some(Self { site, url })
    }

    /// The personal site the recording is on: its token's audience.
    #[must_use]
    pub fn site(&self) -> &DriveSite {
        &self.site
    }

    /// The address its item is read at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// The longest file name taken from a recording's item (SharePoint's own limit).
pub const MAX_RECORDING_NAME: usize = 400;

/// A recording's file as its item's read answered ([`read_recording_file`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingAccess {
    /// Whether the account may download it: its item's `accessViewpoint.canDownload`; false when the answer doesn't
    /// say (an attendee whose download the meeting's policy blocks is told false).
    pub can_download: bool,
    /// Its file name (`<meeting>-<yyyymmdd_hhmmss>-Meeting Recording.mp4`), when the item gives a plain file name.
    pub name: Option<String>,
    /// Its size in bytes, when the item gives it.
    pub size: Option<u64>,
}

/// The answer to a [`RecordingFile`] read, `{name, size, accessViewpoint: {canRead, canDownload, …}}`; `None` for an
/// answer that isn't a JSON object. A name that isn't a plain file name (empty, too long, a path, a control character)
/// is left out.
#[must_use]
pub fn read_recording_file(body: &[u8]) -> Option<RecordingAccess> {
    let item: serde_json::Value = serde_json::from_slice(body).ok()?;
    let item = item.as_object()?;
    let can_download = item
        .get("accessViewpoint")
        .and_then(|access| access.get("canDownload"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let plain_name = |name: &&str| {
        !name.trim().is_empty()
            && name.len() <= MAX_RECORDING_NAME
            && !matches!(name.trim(), "." | "..")
            && !name
                .chars()
                .any(|character| matches!(character, '/' | '\\') || character.is_control())
    };
    let name = item
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(plain_name)
        .map(str::to_owned);
    let size = item.get("size").and_then(serde_json::Value::as_u64);
    Some(RecordingAccess {
        can_download,
        name,
        size,
    })
}

/// Where a recording's expiry is read: its item, on the personal site that holds it, with that site's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingExpiration {
    site: DriveSite,
    url: String,
}

impl RecordingExpiration {
    /// The expiry of item `item` of drive `drive`, on the personal site `link` is on (as [`RecordingManifest::of`]).
    #[must_use]
    pub fn of(link: &str, drive: &str, item: &str) -> Option<Self> {
        let (site, item) = item_address(link, drive, item)?;
        let url = format!("{item}?$select=expiration");
        Some(Self { site, url })
    }

    /// The personal site the recording is on: its token's audience.
    #[must_use]
    pub fn site(&self) -> &DriveSite {
        &self.site
    }

    /// The address its expiry is read at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// When the recording an item read ([`RecordingExpiration`]) answered for expires, ms since the epoch: its
/// `expiration.expirationDateTime` (RFC 3339 with an offset, as captured). `None` when it says none (a recording set
/// never to expire has no date), or the answer isn't one.
#[must_use]
pub fn read_expiration(body: &[u8]) -> Option<i64> {
    let answer: serde_json::Value = serde_json::from_slice(body).ok()?;
    let text = answer.get("expiration")?.get("expirationDateTime")?.as_str()?;
    let at = time::OffsetDateTime::parse(text.trim(), &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()
}

/// The personal site of `link`: an address on it, or a sharing link to one of its files (`/:<kind>:/<g|r>/personal/…`,
/// or the short `/:<kind>:/p/<owner>/…` of a link for specific people).
fn personal_site(link: &str) -> Option<DriveSite> {
    if let Some(site) = DriveSite::from_web_url(link) {
        return Some(site);
    }
    let url = Url::parse(link).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    let owner = match segments.as_slice() {
        [kind, "g" | "r", "personal", owner, ..] | [kind, "p", owner, ..] if is_link_kind(kind) => owner,
        _ => return None,
    };
    let host = url.host_str()?;
    DriveSite::from_web_url(&format!("https://{host}/personal/{owner}/"))
}

/// A sharing link's kind segment: `:v:`, `:w:`, `:u:` and the like.
fn is_link_kind(segment: &str) -> bool {
    segment.len() == 3 && segment.starts_with(':') && segment.ends_with(':')
}

/// A part of a recording's presentation, at the transform service: read with no token of the account's (its address
/// carries its own credential).
#[derive(Clone, PartialEq, Eq)]
pub struct MediaSegmentLink(Url);

impl MediaSegmentLink {
    /// `url` when it is a part's address ([`is_media_link`]).
    fn parse(url: Url) -> Option<Self> {
        is_media_link(&url).then_some(Self(url))
    }

    /// The address (`pub(crate)`: nothing outside this crate sees its credential).
    pub(crate) const fn url(&self) -> &Url {
        &self.0
    }
}

/// `Debug` leaves the address out: its query carries a credential.
impl fmt::Debug for MediaSegmentLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaSegmentLink").finish_non_exhaustive()
    }
}

/// Whether `url` is a part of a recording's presentation: `https`, no user info, port or fragment, at most
/// [`MAX_MEDIA_LINK`] bytes, on a transform service host (`<region>-mediap.svc.ms`), under `/transform/`.
#[must_use]
pub fn is_media_link(url: &Url) -> bool {
    url.as_str().len() <= MAX_MEDIA_LINK
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some_and(is_media_host)
        && url.path().starts_with("/transform/")
}

/// A transform service host: `<region>-mediap.svc.ms`, its region letters and digits.
#[must_use]
pub fn is_media_host(host: &str) -> bool {
    host.strip_suffix(MEDIA_HOST_SUFFIX)
        .is_some_and(|region| !region.is_empty() && region.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// What a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Video,
}

/// Where a media segment is in its track, in the track's timescale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentTime {
    pub start: u64,
    pub duration: u64,
}

/// One track of a presentation: its initialization segment and its media segments' times and addresses.
#[derive(Clone, PartialEq, Eq)]
pub struct PresentedTrack {
    pub kind: TrackKind,
    /// The units of a second its times count.
    pub timescale: u64,
    pub segments: Vec<SegmentTime>,
    init: MediaSegmentLink,
    base: Url,
    /// The media segments' address, its representation filled in, `$Time$` or `$Number$` still to fill.
    media: String,
    start_number: u64,
}

impl fmt::Debug for PresentedTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedTrack")
            .field("kind", &self.kind)
            .field("timescale", &self.timescale)
            .field("segments", &self.segments.len())
            .finish_non_exhaustive()
    }
}

impl PresentedTrack {
    /// Its initialization segment.
    #[must_use]
    pub fn init(&self) -> &MediaSegmentLink {
        &self.init
    }

    /// Media segment `index`, `None` past the last.
    #[must_use]
    pub fn segment(&self, index: usize) -> Option<MediaSegmentLink> {
        let time = self.segments.get(index)?;
        let number = self.start_number.checked_add(u64::try_from(index).ok()?)?;
        let filled = fill(&self.media, &[("Time", time.start), ("Number", number)])?;
        MediaSegmentLink::parse(self.base.join(&filled).ok()?)
    }
}

/// A recording's DASH presentation, as [`read_manifest`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    /// Its length in ms (`mediaPresentationDuration`, else its longest track's).
    pub duration_ms: u64,
    /// Its audio and video tracks, at most one of each, audio first when it has one.
    pub tracks: Vec<PresentedTrack>,
}

/// Reads a recording's presentation (see the module docs); `None` for an answer that isn't one the player plays: not
/// a static MPD, encrypted, no track, a template or timeline this doesn't read, a part's address off the transform
/// service.
#[must_use]
pub fn read_manifest(body: &[u8]) -> Option<Presentation> {
    let text = std::str::from_utf8(body).ok()?;
    let document = roxmltree::Document::parse(text).ok()?;
    let mpd = document.root_element();
    if mpd.tag_name().name() != "MPD" || mpd.attribute("type").is_some_and(|kind| kind != "static") {
        return None;
    }
    if mpd
        .descendants()
        .any(|node| node.tag_name().name() == "ContentProtection")
    {
        return None;
    }
    let base = based(Url::parse("https://invalid.invalid/").ok()?, mpd)?;
    let period = child(mpd, "Period")?;
    let base = based(base, period)?;
    let mut tracks: Vec<PresentedTrack> = Vec::new();
    for set in period
        .children()
        .filter(|node| node.tag_name().name() == "AdaptationSet")
    {
        let Some(track) = read_track(set, &base) else {
            continue;
        };
        if tracks.iter().all(|kept| kept.kind != track.kind) {
            tracks.push(track);
        }
    }
    if tracks.is_empty() {
        return None;
    }
    tracks.sort_by_key(|track| track.kind == TrackKind::Video);
    let longest = tracks
        .iter()
        .filter_map(|track| {
            let last = track.segments.last()?;
            Some(last.start.checked_add(last.duration)?.checked_mul(1000)? / track.timescale)
        })
        .max()
        .unwrap_or(0);
    let duration_ms = mpd
        .attribute("mediaPresentationDuration")
        .and_then(iso_duration_ms)
        .unwrap_or(longest);
    Some(Presentation { duration_ms, tracks })
}

fn child<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|child| child.tag_name().name() == name)
}

/// `base` with `node`'s own `BaseURL` (when it has one) resolved against it.
fn based(base: Url, node: roxmltree::Node<'_, '_>) -> Option<Url> {
    match child(node, "BaseURL").and_then(|url| url.text()) {
        Some(text) => base.join(text.trim()).ok(),
        None => Some(base),
    }
}

/// One adaptation set's track: its kind, its best representation, its template and timeline.
fn read_track(set: roxmltree::Node<'_, '_>, base: &Url) -> Option<PresentedTrack> {
    let kind_of = |text: &str| {
        if text.starts_with("audio") {
            Some(TrackKind::Audio)
        } else if text.starts_with("video") {
            Some(TrackKind::Video)
        } else {
            None
        }
    };
    let kind = set
        .attribute("contentType")
        .and_then(kind_of)
        .or_else(|| set.attribute("mimeType").and_then(kind_of))?;
    let representation = set
        .children()
        .filter(|node| node.tag_name().name() == "Representation")
        .max_by_key(|node| node.attribute("bandwidth").and_then(|value| value.parse::<u64>().ok()))?;
    let id = representation.attribute("id")?;
    let template = child(representation, "SegmentTemplate").or_else(|| child(set, "SegmentTemplate"))?;
    let base = based(based(base.clone(), set)?, representation)?;
    let timescale = template
        .attribute("timescale")
        .map_or(Some(1), |value| value.parse::<u64>().ok())
        .filter(|scale| *scale > 0)?;
    let start_number = template
        .attribute("startNumber")
        .map_or(Some(1), |value| value.parse::<u64>().ok())?;
    let segments = timeline(child(template, "SegmentTimeline")?)?;
    let init = fill(&fill_id(template.attribute("initialization")?, id)?, &[])?;
    let init = MediaSegmentLink::parse(base.join(&init).ok()?)?;
    let media = fill_id(template.attribute("media")?, id)?;
    let track = PresentedTrack {
        kind,
        timescale,
        segments,
        init,
        base,
        media,
        start_number,
    };
    // every part's address is checked as it is asked for; the first one here, so a presentation of parts the player
    // can't read isn't taken
    track.segment(0)?;
    Some(track)
}

/// A `SegmentTimeline`'s segments: each `S`'s `d`, `r` more times, from its `t` or where the last one ended.
fn timeline(node: roxmltree::Node<'_, '_>) -> Option<Vec<SegmentTime>> {
    let mut segments = Vec::new();
    let mut at = 0_u64;
    for entry in node.children().filter(|node| node.tag_name().name() == "S") {
        let number = |name: &str| entry.attribute(name).map(str::parse::<u64>);
        if let Some(start) = number("t") {
            at = start.ok()?;
        }
        let duration = number("d")?.ok().filter(|duration| *duration > 0)?;
        // a negative repeat ("until the next one, or the end") isn't one a recording's timeline has
        let repeat = number("r").transpose().ok()?.unwrap_or(0);
        for _ in 0..=repeat {
            if segments.len() >= MAX_SEGMENTS {
                return None;
            }
            segments.push(SegmentTime { start: at, duration });
            at = at.checked_add(duration)?;
        }
    }
    (!segments.is_empty()).then_some(segments)
}

/// `template` with `$RepresentationID$` filled with `id` and every other identifier left as it is.
fn fill_id(template: &str, id: &str) -> Option<String> {
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(template.replace("$RepresentationID$", id))
}

/// `template` with each `$<name>$` of `values` filled in (`$$` a dollar); `None` for an identifier it doesn't know,
/// a width format (`$Number%05d$`) among them.
fn fill(template: &str, values: &[(&str, u64)]) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('$') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('$')?;
        let name = &after[..close];
        if name.is_empty() {
            out.push('$');
        } else {
            let value = values.iter().find(|(known, _)| *known == name)?.1;
            out.push_str(&value.to_string());
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// An ISO 8601 duration as DASH writes one (`PT0H0M112.960S`, `P1DT2H`), in ms.
fn iso_duration_ms(text: &str) -> Option<u64> {
    let rest = text.strip_prefix('P')?;
    let (days, time) = match rest.split_once('T') {
        Some((days, time)) => (days, time),
        None => (rest, ""),
    };
    let mut ms = 0_f64;
    if !days.is_empty() {
        ms += days.strip_suffix('D')?.parse::<f64>().ok()? * 86_400_000.0;
    }
    let mut number = String::new();
    for character in time.chars() {
        let unit = match character {
            'H' => 3_600_000.0,
            'M' => 60_000.0,
            'S' => 1000.0,
            _ => {
                number.push(character);
                continue;
            }
        };
        ms += number.parse::<f64>().ok()? * unit;
        number.clear();
    }
    if !number.is_empty() || !ms.is_finite() || ms < 0.0 {
        return None;
    }
    // a duration of a recording: well within u64's ms
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and not negative above"
    )]
    Some(ms.round() as u64)
}
