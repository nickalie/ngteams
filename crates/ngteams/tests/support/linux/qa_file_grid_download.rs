//! QA of task 991 (round 1, task 1009), in the same walk: "Download" on the card of a Microsoft file, as a user uses it.
//!
//! Each file is alone in a chat of its own. The parent reads where each download stands from the child (the app's
//! own download of the file's link), the files in a's downloads folder, the links the sandbox's URI opener got and
//! the requests the mock got. The card shows its download line when its height in a shot is more than before its
//! first Download.
//!
//! - **The user's case**: a `.md` file sent with no sharing link (only its own address in a OneDrive). "More options"
//!   shows "Download" (Teams Light and Dark shots). The file's host cuts the first stream and answers the next one
//!   slowly: the card shows "Downloading … %", then the exact bytes are in the downloads folder and the card shows
//!   "Saved to <path>" (Teams Light and Dark shots). The browser opens nothing. No request to the file's host
//!   carries a token or a cookie.
//! - **A second Download** of that file saves `<name> (1).md` and keeps the first file.
//! - **A name in the folder**: a file with a sharing link whose name a different file has in the folder. Download
//!   saves `<name> (1)` and keeps that file as it was.
//! - **A team site's file**: the entry of a channel's file (no sharing link, a team site's address), here in a chat.
//! - **Failures**: Graph does not find the file's item (404); the file's host refuses each read (403); the host cuts
//!   each stream. The card shows why the download failed, no file or `.part` file stays, and a second Download, once
//!   the host answers again, saves the file.
//! - **The keyboard**: Tab to the card, Tab to "More options", Enter, Down twice and Enter: Download saves the file.
//!
//! Every name, id, link and byte is made up.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_mock::{ChatServiceMock, LAB_A, LAB_C};
use ui_sandbox::SandboxResult;
use ui_sandbox::input::keys;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use super::{
    DARK, LIGHT, Look, MENU_REACH, Posted, Rect, SETTLE, Walk, card_keys, cards_in, changed_box, dialog_shown, load,
    more_at, note, post_files, row_at, step,
};

/// How long the host holds back its answer after it cut a stream: time to see "Downloading … %".
const SLOW: Duration = Duration::from_secs(5);

/// How long a download gets to end: a host that cuts each stream fails it only after the app's retries (15 s).
const LONG_WAIT: Duration = Duration::from_secs(60);

/// How much taller a card is with its download line than before its first Download, at the least.
const LINE: u32 = 8;

/// A user's OneDrive, where a file sent in a chat is.
const PERSONAL: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";

/// A team's site, where a channel's file is.
const TEAM_SITE: &str = "https://contoso.sharepoint.com/sites/MadeUpRehearsalTeam/";

/// How the host of a file's bytes answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// The part asked for.
    Whole,
    /// Half of the first stream, then each later part after [`SLOW`].
    CutThenSlow,
    /// Half of the first stream, then nothing of each later part.
    Cut,
    /// This status and no body.
    Status(u16),
}

/// How Graph answers a read of the file's item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Graph {
    Item,
    Status(u16),
}

/// A made-up file of this walk, alone in a chat.
struct Case {
    key: &'static str,
    topic: &'static str,
    name: &'static str,
    extension: &'static str,
    /// Its sharing link, if it has one.
    share: Option<&'static str>,
    /// The site that holds it.
    site: &'static str,
    /// Its own address, percent-encoded as Teams posts it.
    object: String,
    bytes: Vec<u8>,
    graph: Arc<Mutex<Graph>>,
    host: Arc<Mutex<Host>>,
    /// The height of its card before its first download, with no download line.
    bare: Cell<Option<u32>>,
}

impl Case {
    fn new(
        key: &'static str,
        topic: &'static str,
        name: &'static str,
        share: Option<&'static str>,
        site: &'static str,
    ) -> Self {
        let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
        let object = format!(
            "{site}Shared%20Documents/Microsoft%20Teams%20Chat%20Files/{}",
            encoded(name)
        );
        Self {
            key,
            topic,
            name,
            extension,
            share,
            site,
            object,
            bytes: made_up_bytes(key, 640 * 1024),
            graph: Arc::new(Mutex::new(Graph::Item)),
            host: Arc::new(Mutex::new(Host::Whole)),
            bare: Cell::new(None),
        }
    }

    /// The link the app reads it by, and knows its download by: its sharing link, else its own address.
    fn link(&self) -> String {
        self.share.map_or_else(|| self.object.clone(), str::to_owned)
    }

    /// Its entry in `properties.files`, in the shape `tests/fixtures/chatsvc/files.json` has; with no `shareUrl` when
    /// it has no sharing link.
    fn entry(&self) -> Value {
        let mut info = json!({"fileUrl": self.object, "siteUrl": self.site});
        if let Some(share) = self.share {
            info["shareUrl"] = json!(share);
        }
        json!({
            "itemid": format!("made-up-item-{}", self.key),
            "fileName": self.name,
            "fileType": self.extension,
            "fileInfo": info,
            "@type": "http://schema.skype.com/File",
            "version": 2,
            "objectUrl": self.object,
            "type": self.extension,
            "title": self.name,
            "state": "active",
            "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
        })
    }

    /// The path of its bytes on its host, as its item's download link names it.
    fn download_path(&self) -> String {
        format!(
            "/personal/made_up_user_contoso_test/_layouts/15/download-qa1009-{}.aspx",
            self.key
        )
    }

    fn set_host(&self, host: Host) {
        *self.host.lock().unwrap_or_else(PoisonError::into_inner) = host;
    }

    fn set_graph(&self, graph: Graph) {
        *self.graph.lock().unwrap_or_else(PoisonError::into_inner) = graph;
    }
}

/// The files of this walk.
pub(super) struct Cases {
    answers: Case,
    named: Case,
    team: Case,
    gone: Case,
    refused: Case,
    cut: Case,
    keys: Case,
}

impl Cases {
    fn all(&self) -> [&Case; 7] {
        [
            &self.answers,
            &self.named,
            &self.team,
            &self.gone,
            &self.refused,
            &self.cut,
            &self.keys,
        ]
    }
}

/// `%`-encodes `name` as a path's part.
fn encoded(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(byte).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// `len` made-up bytes of a markdown file (synthetic), different for each `key`.
fn made_up_bytes(key: &str, len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    let mut line = 0_u32;
    while bytes.len() < len {
        bytes.extend_from_slice(format!("- made-up answer {line} of {key}, every byte synthetic\n").as_bytes());
        line += 1;
    }
    bytes.truncate(len);
    bytes
}

// --- the service ---

/// c's files of this walk, each alone in a chat with a, and the mock's item and bytes of each.
pub(super) fn post(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult<(Vec<Posted>, Cases)> {
    let cases = Cases {
        answers: Case::new(
            "dl-answers",
            "Made-up answers",
            "Made-up answers — 23–27 November.md",
            None,
            PERSONAL,
        ),
        named: Case::new(
            "dl-named",
            "Made-up named twice",
            "made-up-minutes.txt",
            Some("https://contoso-my.sharepoint.com/:t:/p/made_up_user_contoso_test/MadeUpShareLinkMinutes1009"),
            PERSONAL,
        ),
        team: Case::new(
            "dl-team",
            "Made-up team site",
            "made-up-rehearsal-plan.md",
            None,
            TEAM_SITE,
        ),
        gone: Case::new("dl-gone", "Made-up item gone", "made-up-gone.md", None, PERSONAL),
        refused: Case::new(
            "dl-refused",
            "Made-up host refuses",
            "made-up-refused.md",
            None,
            PERSONAL,
        ),
        cut: Case::new("dl-cut", "Made-up host cuts", "made-up-cut.md", None, PERSONAL),
        keys: Case::new(
            "dl-keys",
            "Made-up keyboard download",
            "made-up-keys.md",
            None,
            PERSONAL,
        ),
    };
    let mut posted = Vec::new();
    for case in cases.all() {
        let chat = mock.add_group(case.topic, &[LAB_C, LAB_A]).ok_or("no group made")?;
        let files = Value::String(Value::Array(vec![case.entry()]).to_string());
        let id = post_files(mock, &chat, "", files)?;
        posted.push(Posted {
            key: case.key,
            chat,
            id,
            cards: 1,
        });
        serve(mock, runtime, case)?;
    }
    Ok((posted, cases))
}

/// The mock's item of `case` (as its [`Graph`] says) and its bytes at the download link the item gives (as its [`Host`]
/// says).
fn serve(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime, case: &Case) -> SandboxResult {
    let token = teams_proto::FileDownload::of(&case.link())
        .ok_or("a made-up link no read names")?
        .share()
        .to_owned();
    let download = case.download_path();
    let item = json!({
        "id": format!("made-up-item-{}", case.key),
        "name": case.name,
        "size": case.bytes.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{download}?UniqueId=made-up-{}", case.key),
    });
    let graph = Arc::clone(&case.graph);
    runtime.block_on(
        mock.mount(
            Mock::given(method("GET"))
                .and(path(format!("/v1.0/shares/{token}/driveItem")))
                .and(header("prefer", "redeemSharingLink"))
                .respond_with(
                    move |_: &Request| match *graph.lock().unwrap_or_else(PoisonError::into_inner) {
                        Graph::Item => ResponseTemplate::new(200).set_body_json(&item),
                        Graph::Status(status) => ResponseTemplate::new(status).set_body_json(json!({
                            "error": {"code": "itemNotFound", "message": "made-up: the item is not there"}
                        })),
                    },
                )
                .with_priority(1),
        ),
    );
    // Graph refuses a share's read that does not redeem the link
    runtime.block_on(
        mock.mount(
            Mock::given(method("GET"))
                .and(path(format!("/v1.0/shares/{token}/driveItem")))
                .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
                    "code": "accessDenied",
                    "message": "made-up: the sharing link no longer exists, or you do not have permission to access it.",
                    "innerError": {"code": "SharesAccessDeniedVroomException"},
                }})))
                .with_priority(2),
        ),
    );
    let host = Arc::clone(&case.host);
    let bytes = Arc::new(case.bytes.clone());
    runtime.block_on(
        mock.mount(
            Mock::given(method("GET"))
                .and(path(download))
                .respond_with(move |request: &Request| {
                    host_answer(request, &bytes, *host.lock().unwrap_or_else(PoisonError::into_inner))
                })
                .with_priority(1),
        ),
    );
    Ok(())
}

/// The host's answer to `request` for `bytes`, as `host` says.
fn host_answer(request: &Request, bytes: &[u8], host: Host) -> ResponseTemplate {
    let total = bytes.len();
    let (first, last) = request
        .headers
        .get("range")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.split_once('-'))
        .and_then(|(first, last)| Some((first.parse::<usize>().ok()?, last.parse::<usize>().ok()?)))
        .unwrap_or((0, total - 1));
    if first >= total || last < first {
        return ResponseTemplate::new(416);
    }
    let last = last.min(total - 1);
    let part = |end: usize| {
        ResponseTemplate::new(206)
            .insert_header("content-range", format!("bytes {first}-{last}/{total}"))
            .set_body_raw(bytes[first..end].to_vec(), "application/octet-stream")
    };
    let half = first + (last + 1 - first) / 2;
    match host {
        Host::Status(status) => ResponseTemplate::new(status),
        Host::Whole => part(last + 1),
        Host::CutThenSlow | Host::Cut if first == 0 => part(half),
        Host::CutThenSlow => part(last + 1).set_delay(SLOW),
        Host::Cut => part(first),
    }
}

// --- where a download stands ---

/// A download as the child tells it.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct Standing {
    progress: String,
    running: bool,
    fraction: Option<f32>,
    target: String,
}

impl Standing {
    fn saved(&self) -> bool {
        self.progress == "Saved"
    }

    fn failed(&self) -> bool {
        self.progress.starts_with("Failed")
    }
}

// --- the walk ---

impl Walk<'_> {
    pub(super) fn download_cases(&mut self) -> SandboxResult {
        self.run.theme(false)?;
        self.users_case()?;
        self.second_download()?;
        self.named_twice()?;
        self.team_site()?;
        self.failures()?;
        self.keyboard_download()?;
        self.no_token_to_the_host();
        self.run.theme(false)
    }

    fn cases(&self) -> &Cases {
        &self.service.cases
    }

    fn case(&self, key: &str) -> &Case {
        self.cases()
            .all()
            .into_iter()
            .find(|case| case.key == key)
            .unwrap_or(&self.cases().answers)
    }

    /// The card of `key`'s chat in `look`, after a shot named `name`: notes when it shows no download line.
    fn line_shown(&mut self, key: &str, name: &str, look: Look) -> SandboxResult {
        let card = self.lone_card(name, look)?;
        let bare = self.case(key).bare.get();
        if let (Some(card), Some(bare)) = (card, bare)
            && card.height() < bare + LINE
        {
            note(format!(
                "{name}: the card shows no download line: {} px high, {bare} px before the Download",
                card.height()
            ));
        }
        Ok(())
    }

    fn downloads_dir(&self) -> PathBuf {
        self.run.dir.join("downloads")
    }

    /// Where the app's download of `link` stands; `None` without one.
    fn standing(&mut self, link: &str) -> SandboxResult<Option<Standing>> {
        let answer = self.run.question(&format!("download {link}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the download {answer}: {error}").into())
    }

    /// Waits up to `wait` for a standing of `link`'s download that `wanted` takes; the last one seen.
    fn standing_until(
        &mut self,
        link: &str,
        wait: Duration,
        wanted: impl Fn(&Standing) -> bool,
    ) -> SandboxResult<Option<Standing>> {
        let start = Instant::now();
        loop {
            let standing = self.standing(link)?;
            if standing.as_ref().is_some_and(&wanted) || start.elapsed() > wait {
                return Ok(standing);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The card of `key`'s chat, alone on the screen in `look`, after a shot named `name`.
    fn lone_card(&mut self, name: &str, look: Look) -> SandboxResult<Option<Rect>> {
        self.run.theme(look.dark)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, name)?;
        let cards = cards_in(&self.run.last, look);
        if let [card] = cards.as_slice() {
            return Ok(Some(*card));
        }
        note(format!("{name}: one card expected on the screen, found {cards:?}"));
        Ok(None)
    }

    /// "More options" → "Download" (row 2 of 4) on the card of `key`'s chat, with the menu's shot `menu_shot`; whether
    /// the menu opened.
    fn click_download(&mut self, key: &str, menu_shot: &str) -> SandboxResult<bool> {
        let Some(card) = self.lone_card(&format!("qa1009-{key}-card-light.png"), LIGHT)? else {
            return Ok(false);
        };
        let bare = &self.case(key).bare;
        if bare.get().is_none() {
            bare.set(Some(card.height()));
        }
        let Some(menu) = self.run.open_menu(self.sandbox, more_at(card), menu_shot)? else {
            note(format!("{key}: More options opens no menu"));
            return Ok(false);
        };
        let (x, y) = row_at(menu, 1, 4);
        self.run.click(x, y)?;
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        Ok(true)
    }

    /// Checks that the download of `case` to `target` saved its exact bytes and the app says so.
    fn expect_saved(&mut self, case_key: &str, link: &str, bytes: &[u8], target: &Path) -> SandboxResult<bool> {
        // an earlier failed download of the same link stays the app's until the new one starts: wait for a saved one
        let standing = self.standing_until(link, LONG_WAIT, |standing| {
            standing.saved() && standing.target == target.display().to_string()
        })?;
        let read = fs::read(target).ok();
        let ok = match (&standing, &read) {
            (Some(standing), Some(read)) if standing.saved() && read.as_slice() == bytes => true,
            _ => {
                note(format!(
                    "{case_key}: Download does not save the exact {} bytes at {}: the app says {standing:?}, the file \
                     has {:?} bytes; the folder holds {:?}",
                    bytes.len(),
                    target.display(),
                    read.as_ref().map(Vec::len),
                    listing(&self.downloads_dir())
                ));
                false
            }
        };
        if part_of(target).exists() {
            note(format!("{case_key}: a .part file stays beside {}", target.display()));
        }
        Ok(ok)
    }

    /// The user's case: a file with no sharing link, its menu, the progress line and the "Saved to" line.
    fn users_case(&mut self) -> SandboxResult {
        let (key, link, name, bytes) = {
            let case = &self.cases().answers;
            case.set_host(Host::CutThenSlow);
            (case.key, case.link(), case.name, case.bytes.clone())
        };
        self.open_message(key)?;
        if let Some(card) = self.lone_card("qa1009-answers-card-dark.png", DARK)?
            && self
                .run
                .open_menu(self.sandbox, more_at(card), "qa1009-answers-menu-dark.png")?
                .is_none()
        {
            note("the user's case: More options opens no menu in Teams Dark".to_owned());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let opened_before = self.sandbox.opened()?.len();
        if !self.click_download(key, "qa1009-answers-menu-light.png")? {
            return Ok(());
        }
        let running = self.standing_until(&link, super::WAIT, |standing| {
            standing.running && standing.fraction.is_some_and(|part| part > 0.0 && part < 1.0)
        })?;
        self.line_shown(key, "qa1009-answers-downloading-light.png", LIGHT)?;
        match &running {
            Some(standing) if standing.running && standing.fraction.is_some() => step(&format!(
                "the user's case: Downloading at {:?} while the host is slow",
                standing.fraction
            )),
            other => note(format!(
                "the user's case: no \"Downloading … %\" while the host is slow: the app says {other:?}"
            )),
        }
        let target = self.downloads_dir().join(name);
        if self.expect_saved(key, &link, &bytes, &target)? {
            step("the user's case: Download saves the exact bytes in the downloads folder");
        }
        thread::sleep(SETTLE);
        self.line_shown(key, "qa1009-answers-saved-light.png", LIGHT)?;
        self.line_shown(key, "qa1009-answers-saved-dark.png", DARK)?;
        self.run.theme(false)?;
        if self.sandbox.opened()?.len() != opened_before {
            note(format!(
                "the user's case: Download opened the browser: {:?}",
                self.sandbox.opened()?
            ));
        }
        self.cases().answers.set_host(Host::Whole);
        Ok(())
    }

    /// A second Download of the user's file saves `<name> (1).md` and keeps the first file.
    fn second_download(&mut self) -> SandboxResult {
        let (key, link, name, bytes) = {
            let case = &self.cases().answers;
            (case.key, case.link(), case.name, case.bytes.clone())
        };
        let first = self.downloads_dir().join(name);
        let before = fs::read(&first).ok();
        if !self.click_download(key, "qa1009-answers-again-menu-light.png")? {
            return Ok(());
        }
        let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
        let second = self.downloads_dir().join(format!("{stem} (1).{extension}"));
        if self.expect_saved("a second Download", &link, &bytes, &second)? {
            step("a second Download saves \"(1)\" beside the first file");
        }
        if fs::read(&first).ok() != before {
            note("a second Download changes the first file".to_owned());
        }
        self.line_shown(key, "qa1009-answers-again-saved-light.png", LIGHT)?;
        Ok(())
    }

    /// A file with a sharing link whose name a different file has in the folder: saved as `<name> (1)`, that file
    /// kept.
    fn named_twice(&mut self) -> SandboxResult {
        let (key, link, name, bytes) = {
            let case = &self.cases().named;
            (case.key, case.link(), case.name, case.bytes.clone())
        };
        let other = b"a made-up file the user had, not the sent one".to_vec();
        let taken = self.downloads_dir().join(name);
        fs::write(&taken, &other)?;
        self.open_message(key)?;
        let opened_before = self.sandbox.opened()?.len();
        if !self.click_download(key, "qa1009-named-menu-light.png")? {
            return Ok(());
        }
        let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
        let target = self.downloads_dir().join(format!("{stem} (1).{extension}"));
        if self.expect_saved("a name in the folder", &link, &bytes, &target)? {
            step("a file with a sharing link and a name in the folder: saved as \"(1)\"");
        }
        if fs::read(&taken).ok().as_deref() != Some(other.as_slice()) {
            note(format!("Download overwrote the different file at {}", taken.display()));
        }
        if self.sandbox.opened()?.len() != opened_before {
            note("Download of a file with a sharing link opened the browser".to_owned());
        }
        self.line_shown(key, "qa1009-named-saved-light.png", LIGHT)?;
        Ok(())
    }

    /// A channel's file entry (a team site's address, no sharing link): Download saves it.
    fn team_site(&mut self) -> SandboxResult {
        let (key, link, name, bytes) = {
            let case = &self.cases().team;
            (case.key, case.link(), case.name, case.bytes.clone())
        };
        self.open_message(key)?;
        if !self.click_download(key, "qa1009-team-menu-light.png")? {
            return Ok(());
        }
        let target = self.downloads_dir().join(name);
        if self.expect_saved("a team site's file", &link, &bytes, &target)? {
            step("a team site's file: Download saves it");
        }
        thread::sleep(SETTLE);
        self.line_shown(key, "qa1009-team-saved-light.png", LIGHT)?;
        Ok(())
    }

    /// Graph 404, the host's 403 and a host that cuts each stream: each fails with no file left, and a second Download
    /// works once the service answers again.
    fn failures(&mut self) -> SandboxResult {
        let failures = [
            ("gone", "Graph does not find the item (404)"),
            ("refused", "the file's host refuses each read (403)"),
            ("cut", "the file's host cuts each stream"),
        ];
        for (which, what) in failures {
            let (key, link, name, bytes) = {
                let case = self.failure_case(which);
                match which {
                    "gone" => case.set_graph(Graph::Status(404)),
                    "refused" => case.set_host(Host::Status(403)),
                    _ => case.set_host(Host::Cut),
                }
                (case.key, case.link(), case.name, case.bytes.clone())
            };
            self.open_message(key)?;
            if !self.click_download(key, &format!("qa1009-{which}-menu-light.png"))? {
                continue;
            }
            let standing = self.standing_until(&link, LONG_WAIT, |standing| !standing.running)?;
            thread::sleep(SETTLE);
            self.line_shown(key, &format!("qa1009-{which}-failed-light.png"), LIGHT)?;
            let target = self.downloads_dir().join(name);
            if !standing.as_ref().is_some_and(Standing::failed) {
                note(format!("{what}: the download does not fail: the app says {standing:?}"));
            }
            if target.exists() || part_of(&target).exists() {
                note(format!(
                    "{what}: a file stays after the failed download: {:?}",
                    listing(&self.downloads_dir())
                ));
            }
            step(&format!("{what}: the app says {standing:?}"));
            {
                let case = self.failure_case(which);
                case.set_graph(Graph::Item);
                case.set_host(Host::Whole);
            }
            if !self.click_download(key, &format!("qa1009-{which}-again-menu-light.png"))? {
                continue;
            }
            if self.expect_saved(&format!("{what}, then a second Download"), &link, &bytes, &target)? {
                step(&format!("{what}: a second Download saves the file"));
            }
            self.line_shown(key, &format!("qa1009-{which}-again-saved-light.png"), LIGHT)?;
        }
        Ok(())
    }

    fn failure_case(&self, which: &str) -> &Case {
        match which {
            "gone" => &self.cases().gone,
            "refused" => &self.cases().refused,
            _ => &self.cases().cut,
        }
    }

    /// Tab to the card, Tab to "More options", Enter, Down twice and Enter: Download saves the file.
    fn keyboard_download(&mut self) -> SandboxResult {
        let (key, link, name, bytes) = {
            let case = &self.cases().keys;
            (case.key, case.link(), case.name, case.bytes.clone())
        };
        self.open_message("dl-answers")?;
        self.open_message(key)?;
        self.key_press(keys::ESC)?;
        self.key_press(keys::ENTER)?;
        let mut ringed = None;
        // Enter into the list can already put the keys on the chat's only card
        for presses in 0..=card_keys::STEPS {
            if presses > 0 {
                self.key_press(keys::TAB)?;
            }
            self.run.shot(self.sandbox, "qa1009-keys-scan.png")?;
            if let [ring] = card_keys::rings(&self.run.last, LIGHT).as_slice() {
                ringed = Some((presses, *ring));
                break;
            }
        }
        let Some((presses, ring)) = ringed else {
            note("the keyboard: Tab never rings the file's card".to_owned());
            return Ok(());
        };
        self.run.shot(self.sandbox, "qa1009-keys-card-light.png")?;
        let opened_before = self.sandbox.opened()?.len();
        let mut menu = None;
        for more in 1..=3 {
            self.key_press(keys::TAB)?;
            thread::sleep(SETTLE);
            let before = load(&self.run.shot(self.sandbox, "qa1009-keys-on-button-light.png")?)?;
            self.key_press(keys::ENTER)?;
            thread::sleep(SETTLE);
            let after = load(&self.run.shot(self.sandbox, "qa1009-keys-menu-light.png")?)?;
            if dialog_shown(&after) {
                step(&format!("the keyboard: Tab {more} after the card is Share"));
                self.key_press(keys::ESC)?;
                thread::sleep(SETTLE);
                continue;
            }
            let corner = Rect {
                x0: ring.x1.saturating_sub(60),
                y0: ring.y0,
                x1: ring.x1,
                y1: ring.y0 + 64,
            };
            menu = changed_box(&before, &after, corner.grown(MENU_REACH), corner)
                .filter(|menu| menu.height() > 20 && menu.width() > 60);
            if menu.is_some() {
                step(&format!(
                    "the keyboard: Tab {more} after the card opens More options with Enter"
                ));
                break;
            }
        }
        if menu.is_none() {
            note(format!(
                "the keyboard: after {presses} Tab presses to the card, no Tab and Enter opens More options"
            ));
            return Ok(());
        }
        // the menu opens with no row chosen: Down chooses "Open in browser", a second Down "Download"
        self.key_press(keys::DOWN)?;
        self.key_press(keys::DOWN)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa1009-keys-menu-download-light.png")?;
        self.key_press(keys::ENTER)?;
        let target = self.downloads_dir().join(name);
        if self.expect_saved("the keyboard", &link, &bytes, &target)? {
            step("the keyboard: Down twice and Enter on More options' Download saves the file");
        }
        if self.sandbox.opened()?.len() != opened_before {
            note(format!(
                "the keyboard: the menu's Enter opened {:?}",
                self.sandbox.opened()?.last()
            ));
        }
        self.run.shot(self.sandbox, "qa1009-keys-saved-light.png")?;
        Ok(())
    }

    fn key_press(&mut self, key: u32) -> SandboxResult {
        self.run.input.key(key)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// No request to a file's host carries a token or a cookie; each Graph read of an item does carry the token.
    fn no_token_to_the_host(&self) {
        let paths: Vec<String> = self.cases().all().iter().map(|case| case.download_path()).collect();
        let requests = self.service.runtime.block_on(self.service.mock.received_requests());
        let to_hosts: Vec<_> = requests
            .iter()
            .filter(|request| paths.iter().any(|path| request.url.path() == path))
            .collect();
        if to_hosts.is_empty() {
            note("no request reached a file's host".to_owned());
        }
        for request in &to_hosts {
            for header in ["authorization", "cookie", "x-skypetoken"] {
                if request.headers.contains_key(header) {
                    note(format!("a request to {} carries {header}", request.url.path()));
                }
            }
        }
        let to_graph = requests
            .iter()
            .filter(|request| request.url.path().starts_with("/v1.0/shares/"))
            .filter(|request| !request.headers.contains_key("authorization"))
            .count();
        if to_graph > 0 {
            note(format!("{to_graph} Graph reads of a file went with no token"));
        }
        step(&format!(
            "{} requests to the files' hosts, none with a token",
            to_hosts.len()
        ));
    }
}

/// `<target>.part`, as the app names a download's file until it is whole.
fn part_of(target: &Path) -> PathBuf {
    teams_ui::recording_download::part_path(target)
}

fn listing(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The child's answer to `download <link>`: where the app's download of `link` stands, `null` without one.
pub(super) fn standing_of(link: &str, cx: &gpui::App) -> Value {
    teams_ui::recording_download::download_of(link, cx).map_or(Value::Null, |download| {
        let download = download.read(cx);
        json!({
            "progress": format!("{:?}", download.progress()),
            "running": download.progress().running(),
            "fraction": download.progress().fraction(),
            "target": download.target().display().to_string(),
        })
    })
}
