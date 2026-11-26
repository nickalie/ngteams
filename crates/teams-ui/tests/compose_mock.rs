//! Compose and the writes from the UI on two accounts' engines on the mock (`teams-mock`, reached through the
//! `test-support` mock origins), in process (M2 Task 13): account a's chat view of the a–b chat over its engine (the
//! bridge, the models, GPUI's test context), account b's engine headless. Each write a makes through the view (a post
//! with lines and a link, sent key for key as the capture's link inserted with its text, typing, a mention picked
//! after `@`, a reply, an edit by Up, a reaction, a delete confirmed, a read receipt) reaches b over its push channel
//! through a's engine and outbox; the view builds no request. Nothing goes out that a didn't ask for: no read receipt
//! for a hidden chat, no obsolete edit sent again, and no delete of another selected message.
//!
//! Every id and token is the mock's made-up one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, MessageChange, Parts, Push};
use teams_core::{Action, Outbox};
use teams_mock::{AB_CHAT, AMS_BASE, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, TrouterMock};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::{Message, OutboxKind, OutboxState, Store};
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use teams_ui::chat_view::{ChatView, MessageAction, READ_DEBOUNCE};
use teams_ui::compose::Compose;
use teams_ui::sending::Again;
use teams_ui::settings::SettingsModel;
use teams_ui::{Bridge, Core, Link, Models, SettingsFile, ShellParts, ViewTrace};
use tokio::sync::broadcast;

/// How long anything is waited for.
const WAIT: Duration = Duration::from_secs(30);

// --- the accounts ---

/// One account's mock token, for its transport and its push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock and its Trouter, on a runtime of their own.
struct Lab {
    runtime: tokio::runtime::Runtime,
    mock: ChatServiceMock,
    trouter: TrouterMock,
}

impl Lab {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let trouter = mock.start_trouter().unwrap();
        Self { runtime, mock, trouter }
    }

    /// The bodies of the message posts the mock got.
    fn posted(&self) -> Vec<String> {
        self.bodies("POST", "/messages")
    }

    /// The bodies of the requests of `method` the mock got whose path ends with `end`, in order.
    fn bodies(&self, method: &str, end: &str) -> Vec<String> {
        let requests = self.runtime.block_on(self.mock.received_requests());
        requests
            .iter()
            .filter(|request| request.method.as_str() == method && request.url.path().ends_with(end))
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect()
    }
}

/// `lines.json` (synthetic-from-capture), its placeholders filled with the mock's made-up ids.
/// The synthetic, marked `.webm` video sticker of the fixtures.
fn synthetic_webm() -> Vec<u8> {
    let hex = include_str!("../../../tests/fixtures/telegram/synthetic-sticker.webm.hex");
    let digits: String = hex.lines().filter(|line| !line.starts_with('#')).collect();
    digits
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn lines_fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc/lines.json");
    let oid = |mri: &str| mri.trim_start_matches("8:orgid:").to_owned();
    let text = std::fs::read_to_string(path)
        .unwrap()
        .replace("<region>", "amer")
        .replace("<guid:a>", &oid(LAB_A))
        .replace("<guid:b>", &oid(LAB_B));
    serde_json::from_str(&text).unwrap()
}

/// What `lines.json`'s message was typed as, line by line.
const LINES_TYPED: [&str; 3] = [
    "a made-up first line",
    "a second line",
    "a link https://example.com/made-up/path on the third line",
];

/// `me`'s engine on the mock and the temporary directory holding its store.
fn engine(lab: &Lab, me: &str) -> (EngineHandle, tempfile::TempDir) {
    let tokens = Arc::new(Tokens {
        token: lab.mock.token(me).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let origin = lab.mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_ams(AMS_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());

    let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), lab.trouter.origin())
        .unwrap()
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
    let dir = tempfile::tempdir().unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(transport, Author::new(Mri::new(me), "Lab account")),
        push: Some(Push::new(trouter)),
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
            ..EngineOptions::default()
        },
        known_names: vec![(LAB_B.to_owned(), "Lab Bee".to_owned())],
    })
    .unwrap();
    (handle, dir)
}

/// b, headless: its engine and what it was told.
struct Headless {
    handle: EngineHandle,
    seen: Vec<String>,
}

impl Headless {
    /// The next of b's events `pick` takes, a's view kept running meanwhile; fails after [`WAIT`].
    fn event<T>(&mut self, cx: &mut VisualTestContext, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            while let Some(event) = self.handle.try_next_event() {
                self.seen.push(format!("{event:?}"));
                if let Some(picked) = pick(&event) {
                    return picked;
                }
            }
            assert!(
                start.elapsed() < WAIT,
                "b wasn't told in time; seen:\n{}",
                self.seen.join("\n")
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The next message event of the a–b chat: what changed, and the message as b stored it.
    fn message(&mut self, cx: &mut VisualTestContext) -> (MessageChange, Message) {
        self.event(cx, |event| match event {
            Event::Message {
                conversation,
                change,
                message: Some(message),
                ..
            } if conversation == AB_CHAT && message.sender.as_deref() == Some(LAB_A) => {
                Some((*change, *message.clone()))
            }
            _ => None,
        })
    }

    /// Until b's push channel is registered and its first catch-up ran.
    fn ready(&mut self, cx: &mut VisualTestContext) {
        let (mut registered, mut caught_up) = (false, false);
        while !(registered && caught_up) {
            self.event(cx, |event| {
                match event {
                    Event::Registration(RegistrationReport::Registered) => registered = true,
                    Event::CaughtUp { .. } => caught_up = true,
                    _ => return None,
                }
                Some(())
            });
        }
    }
}

/// Steps a's view until `done` holds: the engines answer on threads of their own.
#[track_caller]
fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        if done(cx) {
            return;
        }
        assert!(start.elapsed() < WAIT, "not settled in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// a's chat view of the a–b chat, over a's engine through the bridge, in an active window of its own, its compose
/// box focused; the bridge (which owns a's engine) and the models.
fn a_s_view(
    handle: EngineHandle,
    settings: &std::path::Path,
    app: &mut TestAppContext,
) -> (Entity<ChatView>, &'static mut VisualTestContext, Entity<Bridge>, Models) {
    let models = app.update(Models::new);
    let core: Arc<dyn Core> = Arc::new(handle.requests());
    let drained = models.clone();
    let bridge = app.update(|cx| cx.new(|cx| Bridge::start(handle, Arc::clone(&core), drained, cx)));
    app.update(teams_ui::window::init);
    let parts = ShellParts {
        core,
        models: models.clone(),
        settings: app.update(|cx| cx.new(|_| SettingsModel::load(SettingsFile::new(settings.join("a.toml"))))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(app.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB_CHAT.to_owned(), window, cx));
    cx.simulate_resize(size(px(900.0), px(700.0)));
    cx.update(|window, _| window.activate_window());
    (view, cx, bridge, models)
}

fn compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    cx.update(|_, cx| view.read(cx).compose().clone())
}

fn focus_compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let compose = compose(view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
}

fn act(view: &Entity<ChatView>, cx: &mut VisualTestContext, action: MessageAction, message: &str) {
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(action, message, window, cx)));
    cx.run_until_parked();
}

/// The newest message of the a–b chat a's view holds from `sender`.
fn newest_from(view: &Entity<ChatView>, cx: &mut VisualTestContext, sender: &str) -> Option<String> {
    cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .filter(|item| item.sender.as_deref() == Some(sender) && item.actions().contains(&MessageAction::Reply))
            .last()
            .map(|item| item.id.clone())
    })
}

/// The walk through a's writes: the mock, b, a's view and what holds it.
struct Walk {
    lab: Lab,
    b: Headless,
    view: Entity<ChatView>,
    cx: &'static mut VisualTestContext,
    models: Models,
    bridge: Entity<Bridge>,
    /// a's settings, and its sticker packs beside them.
    settings: tempfile::TempDir,
    _dirs: [tempfile::TempDir; 2],
}

impl Walk {
    /// Both engines on the mock, a's view open, b ready, a connected and caught up.
    fn start() -> Self {
        let lab = Lab::start();
        let (a_engine, a_dir) = engine(&lab, LAB_A);
        let (b_engine, b_dir) = engine(&lab, LAB_B);
        let mut b = Headless {
            handle: b_engine,
            seen: Vec::new(),
        };
        let mut app = TestAppContext::single();
        let settings = tempfile::tempdir().unwrap();
        let (view, cx, bridge, models) = a_s_view(a_engine, settings.path(), &mut app);
        b.ready(cx);
        settle(cx, |cx| {
            cx.update(|_, cx| {
                let status = models.status.read(cx);
                status.link == Link::Connected && status.catch_ups > 0
            })
        });
        Self {
            lab,
            b,
            view,
            cx,
            models,
            bridge,
            settings,
            _dirs: [a_dir, b_dir],
        }
    }

    /// A sticker of a pack a imported, picked in the picker: sent to the mock's media service at once, then posted as
    /// Teams' web client posts one when a presses Enter. b gets the sticker, and its picture is the bytes a kept.
    fn a_sticker_from_the_picker(&mut self) {
        use teams_ui::fun_picker::Item;
        use teams_ui::stickers::StickerRef;
        use teams_ui::stickers::import::{self, ImportKind};

        let source = tempfile::tempdir().unwrap();
        let folder = source.path().join("Lab cats");
        std::fs::create_dir_all(&folder).unwrap();
        let mut picture = Vec::new();
        image::RgbaImage::from_pixel(120, 80, image::Rgba([200, 40, 40, 255]))
            .write_to(&mut std::io::Cursor::new(&mut picture), image::ImageFormat::Png)
            .unwrap();
        std::fs::write(folder.join("happy_cat.png"), &picture).unwrap();
        let object = "0-eus-d1-0000000000000000000000000000c47a";
        self.lab
            .runtime
            .block_on(self.lab.mock.serve_picture_sends(object, picture.clone()));
        let stickers = self.settings.path().join("stickers");
        import::import(&folder, ImportKind::Folder, &stickers, &mut |_| {}).unwrap();

        focus_compose(&self.view, self.cx);
        let compose = compose(&self.view, self.cx);
        self.cx
            .update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
        let picker = self
            .cx
            .update(|_, cx| compose.read(cx).emoji_picker().cloned())
            .expect("the picker is open");
        let sticker = StickerRef {
            pack: "lab-cats".to_owned(),
            file: "001.png".to_owned(),
        };
        self.cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.activate(&Item::Sticker(sticker), window, cx));
        });
        self.until_the_sticker_is_sent(&compose);
        self.b_gets_the_sticker(object, &stickers);
    }

    /// a imports a Signal pack from its link (the mock CDN) in the picker, then sends its animated sticker (an APNG in
    /// the pack): b gets Teams' sticker element around the media object, and the media service got the GIF made of it.
    fn an_animated_signal_sticker_from_its_link(&mut self) {
        use teams_mock::signal::{MockPack, MockSticker, SignalCdnMock, animated_png};
        use teams_ui::fun_picker::Item;
        use teams_ui::stickers::signal::SignalCdns;
        use teams_ui::stickers::{ImportState, Shelf, StickerRef};

        let cdn = SignalCdnMock::start(vec![MockPack::synthetic(
            "Signal lab cats",
            vec![MockSticker {
                emoji: "\u{1f63c}".to_owned(),
                content_type: "image/apng".to_owned(),
                bytes: animated_png(64, &[[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]]),
            }],
        )])
        .unwrap();
        let origin = cdn.origin();
        self.cx.update(|_, cx| {
            cx.set_global(SignalCdns::new(Arc::new(move || {
                teams_core::signal::SignalCdn::with_mock_origin(&origin)
            })));
        });
        let link = MockPack::synthetic("", Vec::new()).link();

        focus_compose(&self.view, self.cx);
        let compose = compose(&self.view, self.cx);
        self.cx
            .update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
        let picker = self
            .cx
            .update(|_, cx| compose.read(cx).emoji_picker().cloned())
            .expect("the picker is open");
        self.cx
            .update(|window, cx| picker.update(cx, |picker, cx| picker.open_signal_import(window, cx)));
        self.cx.run_until_parked();
        let panel = self
            .cx
            .update(|_, cx| picker.read(cx).signal_import().cloned())
            .unwrap();
        self.cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.set_link(&link, window, cx);
                panel.start(window, cx);
            });
        });
        let stickers = self.settings.path().join("stickers");
        let held = self.cx.update(|_, cx| Shelf::of(Some(stickers.clone()), cx));
        let start = Instant::now();
        while !matches!(
            self.cx.update(|_, cx| held.read(cx).import_state().cloned()),
            Some(ImportState::Done(_))
        ) {
            assert!(start.elapsed() < WAIT, "the pack wasn't imported");
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        let sticker = StickerRef {
            pack: "signal-lab-cats".to_owned(),
            file: "001.gif".to_owned(),
        };
        self.cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.activate(&Item::Sticker(sticker), window, cx));
        });
        self.until_the_sticker_is_sent(&compose);
        self.b_gets_the_animated_signal_sticker(&stickers);
    }

    /// a presses Enter: b gets the sticker called by its emoji, and the media service got the GIF made of the APNG.
    fn b_gets_the_animated_signal_sticker(&mut self, stickers: &std::path::Path) {
        self.cx.simulate_keystrokes("enter");
        let (_, posted) = self.b.message(self.cx);
        let text = teams_proto::chat::parse_html(&posted.content);
        let got = text
            .nodes
            .iter()
            .find_map(|node| match node {
                teams_proto::chat::Node::Sticker(sticker) => Some(sticker.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("b got a sticker: {}", posted.content));
        assert_eq!(got.label.as_deref(), Some("\u{1f63c}"));
        assert_eq!(got.image.size, Some((64, 64)));
        let gif = std::fs::read(stickers.join("signal-lab-cats/001.gif")).unwrap();
        let uploaded = self
            .lab
            .runtime
            .block_on(self.lab.mock.received_requests())
            .into_iter()
            .filter(|request| request.method.as_str() == "PUT" && request.url.path().ends_with("/content/imgpsh"))
            .last()
            .expect("its picture went to the media service");
        assert_eq!(uploaded.body, gif, "the GIF made of the APNG");
        let frames = image::AnimationDecoder::into_frames(
            image::codecs::gif::GifDecoder::new(std::io::Cursor::new(gif)).unwrap(),
        );
        assert_eq!(frames.count(), 3, "an animated sticker");
    }

    /// a imports a Telegram pack from its link (the mock Bot API) in the picker, then sends its animated sticker and its
    /// video sticker: b gets Teams' sticker element around the media object, and the media service got the GIF each
    /// was made (the `.tgs`'s, the `.webm`'s).
    fn an_animated_telegram_sticker_from_its_link(&mut self) {
        use std::io::Write as _;
        use teams_mock::telegram::{BotApiMock, MockKind, MockSet, MockSticker, SYNTHETIC_BOT_TOKEN};
        use teams_ui::stickers::telegram::{BotApis, BotToken};
        use teams_ui::stickers::{ImportState, Shelf};

        let json = include_str!("../../../tests/fixtures/telegram/synthetic-sticker.json");
        let mut tgs = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        tgs.write_all(json.as_bytes()).unwrap();
        let bot = BotApiMock::start(vec![MockSet {
            name: "SyntheticCats".to_owned(),
            title: "Synthetic cats".to_owned(),
            stickers: vec![
                MockSticker {
                    kind: MockKind::Animated,
                    emoji: "\u{1f638}".to_owned(),
                    bytes: tgs.finish().unwrap(),
                },
                MockSticker {
                    kind: MockKind::Video,
                    emoji: "\u{1f639}".to_owned(),
                    bytes: synthetic_webm(),
                },
            ],
        }])
        .unwrap();
        let origin = bot.origin();
        self.cx.update(|_, cx| {
            cx.set_global(BotApis::new(Arc::new(move |token| {
                teams_core::telegram::BotApi::with_mock_origin(token, &origin)
            })));
            let tokens = BotToken::default();
            tokens.put(&Secret::new(SYNTHETIC_BOT_TOKEN)).unwrap();
            cx.set_global(tokens);
        });

        focus_compose(&self.view, self.cx);
        let compose = compose(&self.view, self.cx);
        self.cx
            .update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
        let picker = self
            .cx
            .update(|_, cx| compose.read(cx).emoji_picker().cloned())
            .expect("the picker is open");
        self.cx
            .update(|window, cx| picker.update(cx, |picker, cx| picker.open_telegram_import(window, cx)));
        self.cx.run_until_parked();
        let panel = self
            .cx
            .update(|_, cx| picker.read(cx).telegram_import().cloned())
            .unwrap();
        self.cx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.set_link("https://t.me/addstickers/SyntheticCats", window, cx);
                panel.start(window, cx);
            });
        });
        let stickers = self.settings.path().join("stickers");
        let held = self.cx.update(|_, cx| Shelf::of(Some(stickers.clone()), cx));
        let start = Instant::now();
        while !matches!(
            self.cx.update(|_, cx| held.read(cx).import_state().cloned()),
            Some(ImportState::Done(_))
        ) {
            assert!(start.elapsed() < WAIT, "the pack wasn't imported");
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.sends_the_animated_and_the_video_sticker(&compose, &stickers);
    }

    /// a sends the imported pack's animated sticker, then its video sticker, from the picker; b gets each.
    fn sends_the_animated_and_the_video_sticker(&mut self, compose: &Entity<Compose>, stickers: &std::path::Path) {
        use teams_ui::fun_picker::Item;
        use teams_ui::stickers::StickerRef;

        for (file, label, frames, shown) in [
            ("001.gif", "\u{1f638}", 30..=30, (180, 180)),
            // the alpha decoder often gives no frame for the video's last block
            ("002.gif", "\u{1f639}", 14..=15, (64, 64)),
        ] {
            focus_compose(&self.view, self.cx);
            if self.cx.update(|_, cx| compose.read(cx).emoji_picker().is_none()) {
                self.cx
                    .update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
            }
            let picker = self
                .cx
                .update(|_, cx| compose.read(cx).emoji_picker().cloned())
                .expect("the picker is open");
            let sticker = StickerRef {
                pack: "synthetic-cats".to_owned(),
                file: file.to_owned(),
            };
            self.cx.update(|window, cx| {
                picker.update(cx, |picker, cx| picker.activate(&Item::Sticker(sticker), window, cx));
            });
            self.until_the_sticker_is_sent(compose);
            let gif = stickers.join("synthetic-cats").join(file);
            self.b_gets_the_animated_sticker(&gif, label, frames, shown);
        }
    }

    /// a presses Enter: b gets the sticker called by its `label`, `shown` in size, and the media service got the GIF of
    /// a count of frames in `frames`, kept at `gif` (made from the `.tgs` or the `.webm`).
    fn b_gets_the_animated_sticker(
        &mut self,
        gif: &std::path::Path,
        label: &str,
        frames: std::ops::RangeInclusive<usize>,
        shown: (u32, u32),
    ) {
        self.cx.simulate_keystrokes("enter");
        let (_, posted) = self.b.message(self.cx);
        let text = teams_proto::chat::parse_html(&posted.content);
        let got = text
            .nodes
            .iter()
            .find_map(|node| match node {
                teams_proto::chat::Node::Sticker(sticker) => Some(sticker.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("b got a sticker: {}", posted.content));
        assert_eq!(got.label.as_deref(), Some(label));
        assert_eq!(got.image.size, Some(shown));
        let gif = std::fs::read(gif).unwrap();
        let uploaded = self
            .lab
            .runtime
            .block_on(self.lab.mock.received_requests())
            .into_iter()
            .filter(|request| request.method.as_str() == "PUT" && request.url.path().ends_with("/content/imgpsh"))
            .last()
            .expect("its picture went to the media service");
        assert_eq!(uploaded.body, gif, "the GIF made from the sticker's file");
        let decoded = image::AnimationDecoder::into_frames(
            image::codecs::gif::GifDecoder::new(std::io::Cursor::new(gif)).unwrap(),
        );
        let count = decoded.count();
        assert!(frames.contains(&count), "an animated sticker: {count} frames");
    }

    /// Until the one picture in a's box is ready to send: sent to the media service, or one of Teams' stickers read.
    fn until_the_sticker_is_sent(&mut self, compose: &Entity<Compose>) {
        let start = Instant::now();
        loop {
            self.cx.run_until_parked();
            let (shown, state) = self.cx.update(|_, cx| {
                let editor = compose.read(cx).editor().read(cx);
                let shown = editor.shown_pictures();
                let state = shown
                    .first()
                    .and_then(|id| editor.picture(*id))
                    .map(|picture| picture.state.clone());
                (shown.len(), state)
            });
            if shown == 1
                && matches!(
                    state,
                    Some(
                        teams_ui::editor::pictures::PictureState::Sent(_)
                            | teams_ui::editor::pictures::PictureState::Hosted(_)
                    )
                )
            {
                break;
            }
            assert!(
                start.elapsed() < WAIT,
                "the sticker wasn't sent: {shown} shown, {state:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// a presses Enter: the post is Teams' sticker element around `object`, and b gets the sticker.
    fn b_gets_the_sticker(&mut self, object: &str, stickers: &std::path::Path) {
        self.cx.simulate_keystrokes("enter");
        let (change, posted) = self.b.message(self.cx);
        assert_eq!(change, MessageChange::New);
        let text = teams_proto::chat::parse_html(&posted.content);
        let sticker = text
            .nodes
            .iter()
            .find_map(|node| match node {
                teams_proto::chat::Node::Sticker(sticker) => Some(sticker.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("b got a sticker: {}", posted.content));
        assert_eq!(sticker.label.as_deref(), Some("Happy cat"));
        assert_eq!(sticker.image.size, Some((120, 80)));
        assert_eq!(sticker.image.item_id.as_deref(), Some(object), "its media object");
        let sent = self
            .lab
            .posted()
            .into_iter()
            .find(|body| body.contains(object))
            .unwrap();
        let sent: Value = serde_json::from_str(&sent).unwrap();
        let content = sent["content"].as_str().unwrap();
        assert!(
            content.starts_with("<p>&nbsp;</p><readonly aria-label=\"Happy cat\" contenteditable=\"false\" itemtype=\"http://schema.skype.com/Sticker\""),
            "{content}"
        );
        assert!(content.ends_with("</readonly><p>&nbsp;</p>"), "{content}");
        let uploaded = self
            .lab
            .runtime
            .block_on(self.lab.mock.received_requests())
            .into_iter()
            .find(|request| request.method.as_str() == "PUT" && request.url.path().contains(object))
            .expect("its picture went to the media service");
        assert_eq!(uploaded.body, std::fs::read(stickers.join("lab-cats/001.png")).unwrap());
    }

    /// One of Teams' stickers picked in the picker: its picture read from the mock's sticker CDN (with no token) into
    /// the box, then posted by its address there when a presses Enter, as Teams' web client posts one, with nothing
    /// sent to the media service. b gets the sticker.
    fn a_teams_sticker_from_the_picker(&mut self) {
        use teams_ui::fun_picker::Item;
        use teams_ui::stickers::StickerRef;

        const PICTURE: &str = "/evergreen-assets/stickerassets/teamsquatch-250x250/Teamsquatch_BugSquash.png";
        let mut picture = Vec::new();
        image::RgbaImage::from_pixel(250, 250, image::Rgba([40, 160, 40, 255]))
            .write_to(&mut std::io::Cursor::new(&mut picture), image::ImageFormat::Png)
            .unwrap();
        self.lab
            .runtime
            .block_on(self.lab.mock.serve_bytes(PICTURE, "image/png", picture));
        let before = self.lab.runtime.block_on(self.lab.mock.received_requests()).len();

        focus_compose(&self.view, self.cx);
        let compose = compose(&self.view, self.cx);
        self.cx
            .update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
        let picker = self
            .cx
            .update(|_, cx| compose.read(cx).emoji_picker().cloned())
            .expect("the picker is open");
        let sticker = Item::Sticker(StickerRef::teams("Teamsquatch_BugSquash"));
        self.cx
            .update(|window, cx| picker.update(cx, |picker, cx| picker.activate(&sticker, window, cx)));
        self.until_the_sticker_is_sent(&compose);
        self.cx.simulate_keystrokes("enter");
        let (change, posted) = self.b.message(self.cx);
        assert_eq!(change, MessageChange::New);
        let text = teams_proto::chat::parse_html(&posted.content);
        let got = text
            .nodes
            .iter()
            .find_map(|node| match node {
                teams_proto::chat::Node::Sticker(sticker) => Some(sticker.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("b got a sticker: {}", posted.content));
        assert_eq!(got.label.as_deref(), Some("teamsquatch squashing a bug"));
        assert_eq!(
            got.image.src,
            format!("https://teams.public.onecdn.static.microsoft{PICTURE}?v=5")
        );
        let requests = self.lab.runtime.block_on(self.lab.mock.received_requests());
        let read = requests[before..]
            .iter()
            .find(|request| request.url.path() == PICTURE)
            .expect("its picture was read from the CDN");
        assert!(read.headers.get("authorization").is_none(), "with no token");
        assert!(
            requests[before..]
                .iter()
                .all(|request| request.method.as_str() == "GET" || !request.url.path().contains("/v1/objects")),
            "nothing was sent to the media service"
        );
        let sent = self
            .lab
            .posted()
            .into_iter()
            .rfind(|body| body.contains("Teamsquatch_BugSquash"))
            .unwrap();
        let sent: Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(sent["amsreferences"], serde_json::json!([]));
    }

    /// b posts `text`, and a's view holds it. Its id.
    fn b_posts(&mut self, text: &str) -> String {
        let row = self
            .b
            .handle
            .write(Action::Send {
                conversation: AB_CHAT.to_owned(),
                text: MessageText::plain(text).unwrap(),
            })
            .blocking_wait()
            .unwrap();
        let theirs = row.message_id.unwrap();
        let view = self.view.clone();
        settle(self.cx, |cx| {
            newest_from(&view, cx, LAB_B).as_deref() == Some(theirs.as_str())
        });
        theirs
    }

    /// b's message comes in and is read: a's receipt goes once its wait is over, through a's outbox, and reaches the
    /// mock. Its id.
    fn b_s_message_is_read(&mut self) -> String {
        let theirs = self.b_posts("hi a, read me");
        let horizons = self.lab.bodies("PUT", "/properties").len();
        self.cx.executor().advance_clock(READ_DEBOUNCE);
        let lab = &self.lab;
        // Wait for the receipt itself so the next step starts after this send.
        settle(self.cx, |_| lab.bodies("PUT", "/properties").len() > horizons);
        theirs
    }

    /// `lines.json`'s three lines, the last with a link, typing before them. The post as b stored it.
    fn a_post_with_lines_and_a_link(&mut self) -> Message {
        focus_compose(&self.view, self.cx);
        for (index, line) in LINES_TYPED.iter().enumerate() {
            if index > 0 {
                self.cx.simulate_keystrokes("shift-enter");
            }
            self.cx.simulate_input(line);
        }
        let typing = self.b.event(self.cx, |event| match event {
            Event::Typing {
                conversation,
                from,
                typing: true,
            } if conversation == AB_CHAT => Some(from.clone()),
            _ => None,
        });
        assert_eq!(typing.as_deref(), Some(LAB_A));
        self.cx.simulate_keystrokes("enter");
        let (change, posted) = self.b.message(self.cx);
        assert_eq!(change, MessageChange::New);
        let fixture = lines_fixture();
        assert_eq!(posted.content, fixture["unlinked_content"].as_str().unwrap());
        // as a sent it: the capture's link inserted with its text, key for key (plain text, `links` "[]"), the values
        // that differ per post (its times, its id, the account's name) aside
        let posts = self.lab.posted();
        let sent = posts.iter().find(|body| body.contains(LINES_TYPED[0])).unwrap();
        let sent: Value = serde_json::from_str(sent).unwrap();
        let mut expected = fixture["request"]["body"].clone();
        expected["content"] = fixture["unlinked_content"].clone();
        expected["properties"]["links"] = Value::from("[]");
        for own in ["composetime", "originalarrivaltime", "clientmessageid", "imdisplayname"] {
            expected[own] = sent[own].clone();
        }
        assert_eq!(sent, expected);
        posted
    }

    /// A mention picked after `@` from the chat's members reaches b naming b by MRI.
    fn a_mention_picked_after_at(&mut self) {
        self.cx.simulate_input("for @");
        let view = self.view.clone();
        settle(self.cx, |cx| {
            let compose = compose(&view, cx);
            cx.update(|_, cx| {
                compose
                    .read(cx)
                    .picker()
                    .is_some_and(|picker| !picker.matches.is_empty())
            })
        });
        self.cx.simulate_keystrokes("enter");
        self.cx.simulate_keystrokes("enter");
        let (_, mention) = self.b.message(self.cx);
        let properties = mention.properties.unwrap_or_default();
        assert!(properties.contains(LAB_B), "b is mentioned by MRI: {properties}");
    }

    /// A reply to b's message `theirs` reaches b with its quote. The reply as b stored it.
    fn a_reply(&mut self, theirs: &str) -> Message {
        act(&self.view, self.cx, MessageAction::Reply, theirs);
        self.cx.simulate_input("my answer");
        self.cx.simulate_keystrokes("enter");
        let (_, reply) = self.b.message(self.cx);
        let quote = format!("itemtype=\"http://schema.skype.com/Reply\" itemid=\"{theirs}\"");
        assert!(reply.content.contains(&quote), "{}", reply.content);
        reply
    }

    /// Up in the empty box edits a's last message: the newest by arrival. On the mock a new message arrives before
    /// the fixture's, so that is a fixture message, a quoted reply whose quote the edit keeps.
    fn an_edit_by_up(&mut self, reply: &Message) {
        let view = self.view.clone();
        settle(self.cx, |cx| {
            cx.update(|_, cx| view.read(cx).message_items().any(|item| item.id == reply.id))
        });
        let last = newest_from(&self.view, self.cx, LAB_A).unwrap();
        self.cx.simulate_keystrokes("up");
        self.cx.simulate_input(" (edited)");
        self.cx.simulate_keystrokes("enter");
        let (change, edited) = self.b.message(self.cx);
        assert_eq!((change, edited.id.as_str()), (MessageChange::Edit, last.as_str()));
        assert!(edited.content.contains(" (edited)</p>"), "{}", edited.content);
        assert!(
            edited.content.starts_with("<blockquote"),
            "the quote kept: {}",
            edited.content
        );
    }

    /// Waits until b is told of a change of message `id` that `wanted` takes.
    fn b_told(&mut self, id: &str, wanted: fn(MessageChange) -> bool) {
        self.b.event(self.cx, |event| match event {
            Event::Message {
                conversation,
                id: changed,
                change,
                ..
            } if conversation == AB_CHAT && changed == id && wanted(*change) => Some(()),
            _ => None,
        });
    }

    /// A quick reaction on b's message `theirs`, then a delete of a's `posted`, confirmed.
    fn a_reaction_and_a_delete(&mut self, theirs: &str, posted: &Message) {
        act(&self.view, self.cx, MessageAction::React, theirs);
        self.cx.simulate_keystrokes("2");
        self.b_told(theirs, |change| {
            matches!(change, MessageChange::Reaction | MessageChange::Other)
        });
        act(&self.view, self.cx, MessageAction::Delete, &posted.id);
        self.cx.simulate_keystrokes("enter");
        self.b_told(&posted.id, |change| change == MessageChange::Delete);
    }

    /// The chat hidden (the shell shows the settings): b's message comes in and nothing is asked of a's engine, so no
    /// receipt reaches the mock; shown again, the receipt goes.
    fn no_receipt_for_the_chat_while_it_isn_t_shown(&mut self) {
        let view = self.view.clone();
        let horizons = self.lab.bodies("PUT", "/properties").len();
        let receipts = self.cx.update(|_, cx| view.read(cx).receipts());
        self.cx
            .update(|_, cx| view.update(cx, |view, cx| view.set_shown(false, cx)));
        self.b_posts("hi a, while you look elsewhere");
        for _ in 0..3 {
            self.cx.executor().advance_clock(READ_DEBOUNCE);
            settle(self.cx, |_| true);
        }
        assert_eq!(
            self.cx.update(|_, cx| view.read(cx).receipts()),
            receipts,
            "none asked for"
        );
        assert_eq!(
            self.lab.bodies("PUT", "/properties").len(),
            horizons,
            "none reached the service"
        );

        self.cx
            .update(|_, cx| view.update(cx, |view, cx| view.set_shown(true, cx)));
        self.cx.executor().advance_clock(READ_DEBOUNCE);
        let lab = &self.lab;
        settle(self.cx, |_| lab.bodies("PUT", "/properties").len() > horizons);
    }

    /// A delete asked on a's newest message, then the selection moved up: Enter deletes nothing.
    fn a_delete_asked_on_one_message_never_deletes_another(&mut self) {
        let mine = newest_from(&self.view, self.cx, LAB_A).unwrap();
        act(&self.view, self.cx, MessageAction::Delete, &mine);
        self.cx.simulate_keystrokes("up");
        self.cx.simulate_keystrokes("enter");
        settle(self.cx, |_| true);
        let view = self.view.clone();
        assert_eq!(
            self.cx.update(|_, cx| view.read(cx).deleting().map(str::to_owned)),
            None
        );
        assert!(self.lab.bodies("DELETE", "").is_empty(), "none reached the service");
        self.cx.simulate_keystrokes("escape");
    }

    /// An edit of a's newest message the service refuses, then a newer one that goes: the refused one is out of date,
    /// never offered or sent again; a later write on the message (which would go after anything sent again before it)
    /// finds only the two edits sent.
    fn an_edit_out_of_date_is_never_sent_again(&mut self) {
        let mine = newest_from(&self.view, self.cx, LAB_A).unwrap();
        let edit_path = format!("/messages/{mine}");
        self.lab
            .runtime
            .block_on(self.lab.mock.refuse_next("PUT", &edit_path, 400));
        act(&self.view, self.cx, MessageAction::Edit, &mine);
        self.cx.simulate_input(" (one)");
        self.cx.simulate_keystrokes("enter");
        let models = self.models.clone();
        let failed = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                models
                    .chats
                    .read(cx)
                    .outbox_rows(AB_CHAT)
                    .find(|row| row.kind == OutboxKind::Edit && row.state == OutboxState::Failed)
                    .map(|row| row.id)
            })
        };
        settle(self.cx, |cx| failed(cx).is_some());
        let stale = failed(self.cx).unwrap();
        let view = self.view.clone();
        settle(self.cx, |cx| {
            notes_on(&view, cx, &mine).contains(&(stale, Again::AfterWarning))
        });

        act(&self.view, self.cx, MessageAction::Edit, &mine);
        self.cx.simulate_input(" (two)");
        self.cx.simulate_keystrokes("enter");
        let (change, edited) = self.b.message(self.cx);
        assert_eq!((change, edited.id.as_str()), (MessageChange::Edit, mine.as_str()));
        assert!(
            edited.content.contains(" (two)") && !edited.content.contains(" (one)"),
            "{}",
            edited.content
        );
        let newer_sent = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                models
                    .chats
                    .read(cx)
                    .outbox_rows(AB_CHAT)
                    .any(|row| row.kind == OutboxKind::Edit && row.id > stale && row.state == OutboxState::Sent)
            })
        };
        settle(self.cx, newer_sent);
        assert_eq!(notes_on(&view, self.cx, &mine), [], "the refused edit is out of date");
        let again: &'static str = Box::leak(format!("send-again-{stale}").into_boxed_str());
        assert!(self.cx.debug_bounds(again).is_none());
        self.cx
            .update(|_, cx| view.update(cx, |view, cx| view.send_again(stale, cx)));

        act(&self.view, self.cx, MessageAction::React, &mine);
        self.cx.simulate_keystrokes("1");
        self.b_told(&mine, |change| {
            matches!(change, MessageChange::Reaction | MessageChange::Other)
        });
        let edits = self.lab.bodies("PUT", &edit_path);
        assert_eq!(edits.len(), 2, "the refused edit and the newer one, nothing again");
        assert!(edits[0].contains(" (one)") && edits[1].contains(" (two)"));
    }

    /// a's bridge shut down (a's engine joined), then b's engine.
    fn stop(self) {
        let bridge = self.bridge.clone();
        let stopping = self.cx.update(|_, cx| bridge.update(cx, Bridge::shutdown));
        drop(stopping);
        settle(self.cx, |cx| cx.update(|_, cx| !bridge.read(cx).running()));
        self.b.handle.shutdown();
    }
}

/// The notes on message `id` in a's view: each row and whether "send again" is offered.
fn notes_on(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Vec<(i64, Again)> {
    cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .find(|item| item.id == id)
            .map(|item| item.outbox.iter().map(|note| (note.row, note.again)).collect())
            .unwrap_or_default()
    })
}

#[test]
fn nothing_goes_out_from_a_s_view_that_a_didn_t_ask_for() {
    let mut walk = Walk::start();
    walk.b_s_message_is_read();
    walk.no_receipt_for_the_chat_while_it_isn_t_shown();
    walk.a_delete_asked_on_one_message_never_deletes_another();
    walk.an_edit_out_of_date_is_never_sent_again();
    walk.stop();
}

#[test]
fn each_write_from_a_s_view_reaches_b_through_the_mock() {
    let mut walk = Walk::start();
    let theirs = walk.b_s_message_is_read();
    let posted = walk.a_post_with_lines_and_a_link();
    walk.a_mention_picked_after_at();
    let reply = walk.a_reply(&theirs);
    walk.an_edit_by_up(&reply);
    walk.a_reaction_and_a_delete(&theirs, &posted);
    walk.a_sticker_from_the_picker();
    walk.a_teams_sticker_from_the_picker();
    walk.an_animated_telegram_sticker_from_its_link();
    walk.an_animated_signal_sticker_from_its_link();
    walk.stop();
}
