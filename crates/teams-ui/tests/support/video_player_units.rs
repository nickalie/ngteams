//! Offline unit scenarios for the parent module; kept outside runtime coverage sources.

use super::*;
use gpui::AppContext as _;
use gpui::size;

#[test]
fn times_are_written_as_the_player_writes_them() {
    assert_eq!(clock(Duration::from_secs(5)), "0:05");
    assert_eq!(clock(Duration::from_secs(754)), "12:34");
    assert_eq!(clock(Duration::from_secs(3_723)), "1:02:03");
}

#[test]
fn a_picture_is_fitted_inside_its_box() {
    let bounds = Bounds {
        origin: point(px(10.0), px(20.0)),
        size: size(px(320.0), px(180.0)),
    };
    let fit = fitted(bounds, 640, 480);
    assert_eq!(fit.size, size(px(240.0), px(180.0)));
    assert_eq!(fit.origin, point(px(50.0), px(20.0)));
}

include!("recording_parts.rs");

#[derive(Default)]
struct Files(Mutex<Vec<(String, u64, u64)>>);

/// A fake engine that streams the made-up recording (item `ITEM1`), and refuses any other one's; counts the parts
/// and the presentations read, and reads files as [`Files`] does. While `expired`, its parts are refused, as an expired
/// address is, until the presentation is read again; while `refusing`, whatever the presentation.
#[derive(Default)]
struct Recordings {
    files: Files,
    parts: Mutex<usize>,
    presentations: Mutex<usize>,
    presentation: Mutex<Option<Arc<teams_proto::Presentation>>>,
    expired: Mutex<bool>,
    refusing: Mutex<bool>,
}

impl crate::inline::PictureEngine for Recordings {
    fn me(&self) -> &'static str {
        "8:orgid:00000000-0000-4000-8000-00000000000a"
    }

    fn message_picture(&self, _: String) -> teams_core::Reply<Option<teams_core::PictureBytes>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn reaction_animation(&self, _: String) -> teams_core::Reply<Option<teams_core::Strip>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn file_stream(&self, share: String, offset: u64, len: u64) -> teams_core::Reply<FileStream> {
        self.files.file_stream(share, offset, len)
    }

    fn recording_presentation(
        &self,
        _: String,
        drive: String,
        item: String,
    ) -> teams_core::Reply<Arc<teams_proto::Presentation>> {
        if (drive.as_str(), item.as_str()) != ("b!drive1", "ITEM1") {
            return teams_core::Reply::ready(Err(EngineError::FileUnavailable));
        }
        let presentation = made_up_presentation();
        *self.presentation.lock().unwrap() = Some(Arc::clone(&presentation));
        *self.presentations.lock().unwrap() += 1;
        *self.expired.lock().unwrap() = false;
        teams_core::Reply::ready(Ok(presentation))
    }

    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> teams_core::Reply<Vec<u8>> {
        *self.parts.lock().unwrap() += 1;
        if *self.expired.lock().unwrap() || *self.refusing.lock().unwrap() {
            return teams_core::Reply::ready(Err(EngineError::FileUnavailable));
        }
        let presentation = self.presentation.lock().unwrap().clone().unwrap();
        teams_core::Reply::ready(made_up_part(&presentation, &part).ok_or(EngineError::FileUnavailable))
    }

    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

fn recording(item: &str) -> Recording {
    Recording {
        drive: "b!drive1".to_owned(),
        item: item.to_owned(),
    }
}

#[test]
fn a_recording_is_streamed_through_the_engine_and_played() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let engine = Arc::new(Recordings::default());
    let source = PictureSource::of(engine.clone());
    let (view, cx) = app.add_window_view(|_, cx| {
        VideoPlayer::new("recap".to_owned(), movie_file(), source, cx)
            .filling()
            .streaming(recording("ITEM1"))
    });
    draw(cx);
    click(cx, "video-play-recap");
    wait_state(&view, cx, State::Playing);
    let until = Instant::now() + WAIT;
    while cx.update(|_, cx| view.read(cx).position()).unwrap_or_default() < Duration::from_millis(500) {
        assert!(Instant::now() < until, "it doesn't move on");
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|_, cx| {
        view.update(cx, |view, _| {
            assert!(view.frame().is_some_and(|(_, shape)| shape == (320, 180)));
            assert!((view.status.unwrap().duration.as_secs_f32() - 3.0).abs() < 0.1);
            assert!(!view.streamed_failed);
        });
    });
    // its parts came from the engine's stream, never from the file's download link
    assert!(*engine.parts.lock().unwrap() >= 4);
    assert!(engine.files.0.lock().unwrap().is_empty());
    // paused, unless its 3 s already played to their end (a loaded machine)
    click(cx, "video-play-recap");
    let until = Instant::now() + WAIT;
    while !matches!(
        cx.update(|_, cx| view.read(cx).state()),
        Some(State::Paused | State::Ended)
    ) {
        assert!(Instant::now() < until, "it doesn't pause");
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_recording_that_can_t_be_streamed_plays_from_its_download_link() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let engine = Arc::new(Recordings::default());
    let source = PictureSource::of(engine.clone());
    let (view, cx) = app.add_window_view(|_, cx| {
        VideoPlayer::new("fallback".to_owned(), movie_file(), source, cx).streaming(recording("ITEM2"))
    });
    draw(cx);
    click(cx, "video-play-fallback");
    wait_state(&view, cx, State::Playing);
    assert!(cx.update(|_, cx| view.read(cx).streamed_failed));
    assert!(!engine.files.0.lock().unwrap().is_empty());
    assert_eq!(*engine.parts.lock().unwrap(), 0);
}

#[test]
fn a_recording_paused_while_its_stream_opens_stays_paused_from_its_download_link() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let engine = Arc::new(Recordings::default());
    let source = PictureSource::of(engine.clone());
    let (view, cx) = app.add_window_view(|_, cx| {
        VideoPlayer::new("put-away".to_owned(), movie_file(), source, cx).streaming(recording("ITEM2"))
    });
    draw(cx);
    // played, then put away (its part or tab left) before its stream's refusal was heard
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.toggle(cx);
            view.pause(cx);
        });
    });
    wait_state(&view, cx, State::Paused);
    assert!(cx.update(|_, cx| view.read(cx).streamed_failed));
    assert!(!engine.files.0.lock().unwrap().is_empty());
}

#[test]
fn a_recording_s_parts_are_asked_of_the_engine_only_once_its_presentation_came() {
    let engine = Arc::new(Recordings::default());
    let parts = EngineParts {
        source: PictureSource::of(engine.clone()),
        link: "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/ITEM1".to_owned(),
        recording: recording("ITEM1"),
        read: Mutex::new(None),
    };
    let abandon = Abandon::new();
    assert_eq!(parts.part(0, Part::Init, &abandon), Err(SourceError::Unavailable));
    let presentation = parts.presentation(&abandon).unwrap();
    assert_eq!(presentation.duration, Duration::from_secs(3));
    assert_eq!(presentation.tracks[0].kind, TrackKind::Audio);
    assert_eq!(presentation.tracks[1].segments.len(), 3);
    assert!(!parts.part(1, Part::Media(2), &abandon).unwrap().is_empty());
    assert_eq!(parts.part(1, Part::Media(3), &abandon), Err(SourceError::Unavailable));
    assert_eq!(parts.part(5, Part::Init, &abandon), Err(SourceError::Unavailable));
    abandon.set();
    assert_eq!(parts.part(1, Part::Init, &abandon), Err(SourceError::Stopped));
    assert!(matches!(parts.presentation(&abandon), Err(SourceError::Stopped)));
    let refused = EngineParts {
        recording: recording("ITEM2"),
        read: Mutex::new(None),
        ..parts
    };
    assert!(matches!(
        refused.presentation(&Abandon::new()),
        Err(SourceError::Unavailable)
    ));
}

#[test]
fn a_part_refused_as_its_address_expired_is_read_again_at_the_presentation_s_new_one() {
    let engine = Arc::new(Recordings::default());
    let parts = EngineParts {
        source: PictureSource::of(engine.clone()),
        link: "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/ITEM1".to_owned(),
        recording: recording("ITEM1"),
        read: Mutex::new(None),
    };
    let abandon = Abandon::new();
    parts.presentation(&abandon).unwrap();
    assert!(!parts.part(1, Part::Media(0), &abandon).unwrap().is_empty());
    assert_eq!(*engine.presentations.lock().unwrap(), 1);
    // played or paused past its addresses' expiry: the presentation read again, the part asked for once more
    *engine.expired.lock().unwrap() = true;
    assert!(!parts.part(1, Part::Media(1), &abandon).unwrap().is_empty());
    assert_eq!(*engine.presentations.lock().unwrap(), 2);
    assert_eq!(*engine.parts.lock().unwrap(), 3);
    // a part refused at any address: the presentation read again once, then the part can't be had
    *engine.refusing.lock().unwrap() = true;
    assert_eq!(parts.part(0, Part::Init, &abandon), Err(SourceError::Unavailable));
    assert_eq!(*engine.presentations.lock().unwrap(), 3);
    assert_eq!(*engine.parts.lock().unwrap(), 5);
    // a presentation that no longer plays as the one played did isn't taken
    let other = Arc::new(teams_proto::Presentation {
        duration_ms: 1,
        tracks: Vec::new(),
    });
    *parts.read.lock().unwrap() = Some(Arc::clone(&other));
    assert_eq!(parts.read_again(&other, &abandon), Err(SourceError::Unavailable));
    assert!(Arc::ptr_eq(parts.read.lock().unwrap().as_ref().unwrap(), &other));
}

impl crate::inline::PictureEngine for Files {
    fn me(&self) -> &'static str {
        "8:orgid:00000000-0000-4000-8000-00000000000a"
    }

    fn message_picture(&self, _: String) -> teams_core::Reply<Option<teams_core::PictureBytes>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn reaction_animation(&self, _: String) -> teams_core::Reply<Option<teams_core::Strip>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn file_stream(&self, share: String, offset: u64, len: u64) -> teams_core::Reply<FileStream> {
        self.0.lock().unwrap().push((share.clone(), offset, len));
        teams_core::Reply::ready(match share.as_str() {
            "missing" => Err(EngineError::FileUnavailable),
            "stopped" => Err(EngineError::Stopped),
            "failed" => Err(EngineError::PictureUnread),
            "https://files.example.test/synthetic.mp4" => {
                let start = usize::try_from(offset).unwrap().min(MOVIE.len());
                let end = start.saturating_add(usize::try_from(len).unwrap()).min(MOVIE.len());
                Ok(FileStream::of(offset, &MOVIE[start..end], MOVIE.len() as u64, 4096))
            }
            _ => Ok(FileStream::of(offset, b"synthetic bytes", 100, 4)),
        })
    }

    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

/// The longest a player is waited for: it opens and decodes on threads of its own, which a loaded machine can starve
/// for well over ten seconds.
const WAIT: Duration = Duration::from_secs(60);

/// Taken by each test that opens a player, so they play one at a time. Each player is a `GStreamer` pipeline with
/// threads of its own: the crate's tests playing all at once on a loaded machine starved some for over a minute (still
/// loading, or not yet paused), and four at a time still now and then; one at a time, under the same load, never.
static PLAYING: Mutex<()> = Mutex::new(());

/// This test's turn to play, held until it ends.
fn playing_slot() -> std::sync::MutexGuard<'static, ()> {
    PLAYING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

const MOVIE: &[u8] = include_bytes!("../../../teams-media/tests/fixtures/media/made-up-test-pattern.mp4");

fn movie_file() -> SharedFile {
    SharedFile {
        name: "synthetic.mp4".to_owned(),
        extension: "mp4".to_owned(),
        share_url: Some("https://files.example.test/synthetic.mp4".to_owned()),
        object_url: Some("https://files.example.test/synthetic.mp4".to_owned()),
        preview: None,
        location: None,
        entry: None,
    }
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .center();
    cx.simulate_click(at, gpui::Modifiers::none());
    draw(cx);
}

fn wait_state(view: &Entity<VideoPlayer>, cx: &mut gpui::VisualTestContext, wanted: State) {
    let until = Instant::now() + WAIT;
    loop {
        draw(cx);
        let state = cx.update(|_, cx| view.read(cx).state());
        if state == Some(wanted) {
            return;
        }
        assert!(Instant::now() < until, "wanted {wanted:?}, last state {state:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_synthetic_movie_plays_pauses_and_seeks_without_opening_any_devices() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let files = Arc::new(Files::default());
    let source = PictureSource::of(files.clone());
    let (view, cx) = app.add_window_view(|_, cx| VideoPlayer::new("movie".to_owned(), movie_file(), source, cx));
    draw(cx);
    click(cx, "video-mute-movie");
    click(cx, "video-play-movie");
    wait_state(&view, cx, State::Playing);
    click(cx, "video-play-movie");
    wait_state(&view, cx, State::Paused);
    assert!(!files.0.lock().unwrap().is_empty());
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            assert!(view.muted);
            assert!(view.frame().is_some(), "the synthetic movie produced a decoded frame");
            view.status = Some(Status {
                state: State::Paused,
                position: Duration::from_secs(5),
                duration: Duration::from_secs(30),
                sound: false,
                rate: 1.0,
            });
            view.skip(true, cx);
            assert_eq!(view.status.unwrap().position, Duration::ZERO);
            view.skip(false, cx);
            assert_eq!(view.status.unwrap().position, SKIP);
            view.seek(Command::Seek(Duration::from_secs(29)), cx);
            view.skip(false, cx);
            assert_eq!(view.status.unwrap().position, Duration::from_secs(30));
            view.seek(Command::Pause, cx);
            assert_eq!(view.status.unwrap().position, Duration::from_secs(30));
        });
    });
    click(cx, "video-fullscreen-movie");
    assert!(cx.update(|_, cx| view.read(cx).full_screen.on));
    cx.simulate_keystrokes("m left right escape");
    assert!(!cx.update(|_, cx| view.read(cx).full_screen.on));
    assert!(!cx.update(|_, cx| view.read(cx).muted));
    draw(cx);
    let bar = cx.debug_bounds("video-seek-movie").unwrap();
    let from = bar.origin + point(bar.size.width * 0.2, bar.size.height * 0.5);
    let to = bar.origin + point(bar.size.width * 0.8, bar.size.height * 0.5);
    cx.simulate_mouse_down(from, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, gpui::Modifiers::none());
    assert!(cx.update(|_, cx| view.read(cx).dragging.is_some()));
    cx.simulate_mouse_up(to, MouseButton::Left, gpui::Modifiers::none());
    assert!(cx.update(|_, cx| view.read(cx).dragging.is_none()));
}

#[test]
fn the_volume_slider_sets_how_loud_it_plays_and_unmutes_it() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (view, cx) = app.add_window_view(|_, cx| VideoPlayer::new("loud".to_owned(), movie_file(), source, cx));
    draw(cx);
    click(cx, "video-play-loud");
    wait_state(&view, cx, State::Playing);
    assert!(
        cx.debug_bounds("video-time-loud").is_some(),
        "the time shows once the length is known"
    );
    click(cx, "video-mute-loud");
    assert!(cx.update(|_, cx| view.read(cx).muted));
    // the slider shows while the pointer is on the sound's button, or it is dragged
    cx.update(|_, cx| view.update(cx, |view, _| view.volume.adjusting = true));
    draw(cx);
    let bar = cx.debug_bounds("video-volume-loud").unwrap();
    let at = |fraction: f32| bar.origin + point(bar.size.width * fraction, bar.size.height * 0.5);
    cx.simulate_mouse_down(at(0.25), MouseButton::Left, gpui::Modifiers::none());
    let (level, muted) = cx.update(|_, cx| (view.read(cx).volume.level, view.read(cx).muted));
    assert!((level - 0.25).abs() < 0.05, "{level}");
    assert!(!muted, "a sound turned up is unmuted");
    cx.simulate_mouse_move(at(0.75), MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(at(0.75), MouseButton::Left, gpui::Modifiers::none());
    let (level, adjusting) = cx.update(|_, cx| (view.read(cx).volume.level, view.read(cx).volume.adjusting));
    assert!((level - 0.75).abs() < 0.05, "{level}");
    assert!(!adjusting);
    // a player opened again takes the volume set
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.player = None;
            view.open(Asked::Play, cx);
            assert!(view.player.is_some());
        });
    });
}

#[test]
fn video_actions_open_only_the_mock_browser_and_copy_the_original_link() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (_, cx) = app.add_window_view(|_, cx| VideoPlayer::new("menu".to_owned(), movie_file(), source, cx));
    draw(cx);
    click(cx, "video-open-menu");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-copy");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-download");
    assert!(
        cx.did_prompt_for_new_path(),
        "saved through the engine, not the browser"
    );
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("synthetic.mp4");
    let place = target.clone();
    cx.simulate_new_path_selection(move |_| Some(place));
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), MOVIE);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4"),
        "no download link opened"
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-open");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
}

#[test]
fn starting_a_second_movie_pauses_the_first() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    // the movie is 3 s long: on a slow machine the first can run out before the second starts, and a movie that
    // ended has nothing left to pause; then it is tried again
    for _ in 0..3 {
        let (first, cx) =
            app.add_window_view(|_, cx| VideoPlayer::new("first".to_owned(), movie_file(), source.clone(), cx));
        draw(cx);
        click(cx, "video-play-first");
        wait_state(&first, cx, State::Playing);
        let second =
            cx.update(|_, cx| cx.new(|cx| VideoPlayer::new("second".to_owned(), movie_file(), source.clone(), cx)));
        cx.update(|_, cx| second.update(cx, VideoPlayer::toggle));
        if wait_either(&first, cx, State::Paused, State::Ended) == State::Ended {
            continue;
        }
        assert!(cx.update(|_, cx| second.read(cx).player.is_some()));
        let live = cx.update(|_, cx| players(cx));
        assert!(live.contains(&first) && live.contains(&second), "both players stay");
        return;
    }
    panic!("the first movie ran out before the second started, every time");
}

/// Waits until `view` is `one` or `other`, and says which.
fn wait_either(view: &Entity<VideoPlayer>, cx: &mut gpui::VisualTestContext, one: State, other: State) -> State {
    let until = Instant::now() + WAIT;
    loop {
        draw(cx);
        let state = cx.update(|_, cx| view.read(cx).state());
        if let Some(state) = state.filter(|state| *state == one || *state == other) {
            return state;
        }
        assert!(
            Instant::now() < until,
            "wanted {one:?} or {other:?}, last state {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn playback_reads_through_the_engine_in_pieces_and_cancellation_stops_a_wait() {
    let files = Arc::new(Files::default());
    let source = PictureSource::of(files.clone());
    let bytes = EngineBytes {
        source: source.clone(),
        share_url: "synthetic-file".to_owned(),
    };
    let abandon = Abandon::new();
    let mut stream = bytes.open(10, 15, &abandon).unwrap();
    assert_eq!(stream.total(), 100);
    let mut read = Vec::new();
    while let Some(piece) = stream.next(&abandon).unwrap() {
        read.extend(piece);
    }
    assert_eq!(read, b"synthetic bytes");
    assert_eq!(
        files.0.lock().unwrap().as_slice(),
        [("synthetic-file".to_owned(), 10, 15)]
    );
    abandon.set();
    assert_eq!(stream.next(&abandon).unwrap_err(), SourceError::Stopped);
    assert!(matches!(bytes.open(0, 1, &abandon), Err(SourceError::Stopped)));
    for (share_url, expected) in [
        ("missing", SourceError::Unavailable),
        ("stopped", SourceError::Stopped),
        ("failed", SourceError::Failed),
    ] {
        let bytes = EngineBytes {
            source: source.clone(),
            share_url: share_url.to_owned(),
        };
        assert!(matches!(bytes.open(0, 1, &Abandon::new()), Err(error) if error == expected));
    }
}

#[test]
fn the_frame_sink_accepts_only_packed_bgra_and_keeps_the_latest_frame_and_status() {
    let (wake, woken) = async_channel::bounded(1);
    let screen = Screen {
        picture: Mutex::new(None),
        status: Mutex::new(None),
        serial: AtomicU64::new(0),
        wake,
    };
    let frame = VideoFrame {
        width: 2,
        height: 1,
        stride: 8,
        format: PixelFormat::Bgra,
        data: vec![1; 8],
    };
    screen.frame(VideoFrame {
        format: PixelFormat::Rgba,
        ..frame.clone()
    });
    screen.frame(VideoFrame {
        stride: 12,
        ..frame.clone()
    });
    assert!(screen.picture.lock().unwrap().is_none());
    assert!(woken.try_recv().is_err());
    screen.frame(frame.clone());
    screen.frame(VideoFrame {
        data: vec![2; 8],
        ..frame
    });
    let picture = screen.picture.lock().unwrap().clone().unwrap();
    assert_eq!((picture.width, picture.height, picture.serial), (2, 1, 2));
    assert_eq!(*picture.bgra, vec![2; 8]);
    let status = Status {
        state: State::Buffering,
        position: Duration::from_secs(1),
        duration: Duration::from_secs(10),
        sound: false,
        rate: 1.0,
    };
    screen.status(status);
    assert_eq!(*screen.status.lock().unwrap(), Some(status));
    assert!(woken.try_recv().is_ok());
    assert!(woken.try_recv().is_err(), "a pending wake coalesces frames and status");
}

#[test]
fn player_statuses_render_their_loading_and_failure_states_and_fullscreen_is_reversible() {
    use gpui::TestAppContext;

    let _playing = playing_slot();
    let mut app = TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let file = SharedFile {
        name: "synthetic.mp4".to_owned(),
        extension: "mp4".to_owned(),
        share_url: None,
        object_url: None,
        preview: None,
        location: None,
        entry: None,
    };
    let (view, cx) = app.add_window_view(|_, cx| VideoPlayer::new("status".to_owned(), file, source, cx));
    let output = cx.update(|_, cx| view.read(cx).screen.clone());
    for state in [
        State::Loading,
        State::Buffering,
        State::Paused,
        State::Playing,
        State::Ended,
        State::Failed(Failure::Unavailable),
        State::Failed(Failure::Network),
        State::Failed(Failure::Unsupported),
        State::Failed(Failure::NoCodec),
        State::Failed(Failure::Internal),
    ] {
        output.status(Status {
            state,
            position: Duration::from_secs(5),
            duration: Duration::from_secs(30),
            sound: false,
            rate: 1.0,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(cx.update(|_, cx| view.read(cx).state()), Some(state));
        assert_eq!(
            cx.debug_bounds("video-loading-status").is_some(),
            matches!(state, State::Loading | State::Buffering)
        );
        assert_eq!(
            cx.debug_bounds("video-failed-status").is_some(),
            matches!(state, State::Failed(_))
        );
        let icon = ["video-icon-status teams", "video-icon-status glyph"]
            .iter()
            .any(|selector| cx.debug_bounds(selector).is_some());
        assert_eq!(
            icon,
            !matches!(state, State::Failed(_)),
            "the error replaces the play mark: {state:?}"
        );
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.toggle(cx); // no sharing link: never starts a media thread
            assert!(view.player.is_none());
            view.toggle_mute(cx);
            assert!(view.muted);
            view.toggle_fullscreen(window, cx);
            assert!(view.full_screen.on);
            view.toggle_fullscreen(window, cx);
            assert!(!view.full_screen.on);
        });
    });
}

/// Made-up captions over the made-up recording's 3 s: two cues, the second from 2 s.
fn made_up_captions() -> Captions {
    let cues = teams_proto::transcript::read_vtt(
        "WEBVTT\n\nu/1-0\n00:00:00.000 --> 00:00:01.500\n<v Ada Lab>Hello everyone.</v>\n\n\
         u/2-0\n00:00:02.000 --> 00:00:03.000\n<v Ada Lab>The second one.</v>",
    )
    .unwrap();
    Captions::new(&cues, Some("en-US".to_owned()))
}

#[test]
fn captions_follow_the_cues_and_are_named_by_their_language() {
    let _playing = playing_slot();
    let captions = made_up_captions();
    assert_eq!(captions.at(Duration::from_millis(400)), Some("Hello everyone."));
    assert_eq!(captions.at(Duration::from_millis(1_700)), None);
    assert_eq!(captions.at(Duration::from_millis(2_500)), Some("The second one."));
    assert_eq!(captions.at(Duration::from_secs(4)), None);
    assert_eq!(captions.label(), "English (United States)");
    assert_eq!(Captions::new(&[], Some("xx-YY".to_owned())).label(), "xx-YY");
    assert_eq!(Captions::new(&[], None).label(), "On");
    assert_eq!(language_name("de-de"), Some("German (Germany)"));
    assert_eq!(rate_label(1.0), "1x");
    assert_eq!(rate_label(1.5), "1.5x");
    assert_eq!(rate_label(0.8), "0.8x");
    let mut style = CaptionStyle {
        on: true,
        size: CaptionSize::Large,
        color: CaptionColor::Purple,
        transparent: true,
    };
    style.revert();
    assert_eq!(
        style,
        CaptionStyle {
            on: true,
            ..CaptionStyle::default()
        }
    );
    for color in CaptionColor::ALL {
        let (words, background) = color.colors();
        assert_ne!(words, background, "{}", color.label());
    }
}

/// A recap's player (filling, streaming) with made-up captions, its caption style Teams' default.
fn captioned(app: &mut gpui::TestAppContext) -> (Entity<VideoPlayer>, &mut gpui::VisualTestContext) {
    app.update(crate::window::init);
    app.update(|cx| cx.set_global(CaptionStyle::default()));
    let engine = Arc::new(Recordings::default());
    let source = PictureSource::of(engine);
    let (view, cx) = app.add_window_view(|_, cx| {
        let mut player = VideoPlayer::new("recap".to_owned(), movie_file(), source, cx)
            .filling()
            .streaming(recording("ITEM1"));
        player.set_captions(Some(made_up_captions()), cx);
        player
    });
    draw(cx);
    (view, cx)
}

/// Where it plays, as its player would tell it.
fn playing_at(view: &Entity<VideoPlayer>, position: Duration, cx: &mut gpui::VisualTestContext) {
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.status = Some(Status {
                state: State::Playing,
                position,
                duration: Duration::from_secs(3),
                sound: false,
                rate: 1.0,
            });
            cx.notify();
        });
    });
    draw(cx);
}

/// Waits until its player tells it plays at `rate`.
fn wait_rate(view: &Entity<VideoPlayer>, cx: &mut gpui::VisualTestContext, rate: f64) {
    let until = Instant::now() + WAIT;
    loop {
        draw(cx);
        let told = cx.update(|_, cx| view.read(cx).status.map(|status| status.rate));
        if told.is_some_and(|told| (told - rate).abs() < f64::EPSILON) {
            return;
        }
        assert!(Instant::now() < until, "told {told:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Turns captions on through the CC menu.
fn turn_captions_on(view: &Entity<VideoPlayer>, cx: &mut gpui::VisualTestContext) {
    click(cx, "video-captions-recap");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), Some(Menu::Captions));
    assert!(cx.debug_bounds("video-captions-off").is_some());
    click(cx, "video-captions-on");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None);
    assert!(cx.update(|_, cx| CaptionStyle::current(cx).on));
}

#[test]
fn a_recap_s_captions_show_where_it_plays_once_turned_on() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, cx) = captioned(&mut app);
    // the recap's bar: captions, speed and settings in place of "More options"
    for selector in ["video-captions-recap", "video-speed-recap", "video-settings-recap"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(cx.debug_bounds("video-more-recap").is_none());
    turn_captions_on(&view, cx);
    // where it plays, as its player tells it: shown while the first cue is spoken
    let at = |position: Duration, cx: &mut gpui::VisualTestContext| playing_at(&view, position, cx);
    let caption = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| view.read(cx).caption(cx).map(str::to_owned));
    at(Duration::from_millis(500), cx);
    assert!(cx.debug_bounds("video-caption-recap").is_some());
    assert_eq!(caption(cx), Some("Hello everyone.".to_owned()));
    // between cues, none
    at(Duration::from_millis(1_700), cx);
    assert!(cx.debug_bounds("video-caption-recap").is_none());
    // a seek: the cue spoken there, as soon as the bar shows its place
    at(Duration::from_millis(2_200), cx);
    assert_eq!(caption(cx), Some("The second one.".to_owned()));
    // C turns them off, and on again
    cx.simulate_keystrokes("c");
    assert!(!cx.update(|_, cx| CaptionStyle::current(cx).on));
    assert_eq!(caption(cx), None);
    cx.simulate_keystrokes("c");
    assert!(cx.update(|_, cx| CaptionStyle::current(cx).on));
    // the menu's Off turns them off
    click(cx, "video-captions-recap");
    click(cx, "video-captions-off");
    assert!(!cx.update(|_, cx| CaptionStyle::current(cx).on));
}

#[test]
fn a_recap_plays_at_the_speed_chosen_and_keeps_it() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, cx) = captioned(&mut app);
    let rate = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| view.read(cx).rate());
    // chosen before it plays: kept for its first play
    click(cx, "video-speed-recap");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), Some(Menu::Speed));
    for selector in ["video-rate-0.8x", "video-rate-1x", "video-rate-1.2x", "video-rate-1.8x"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    click(cx, "video-rate-2x");
    assert!((rate(cx) - 2.0).abs() < f64::EPSILON);
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None);
    click(cx, "video-play-recap");
    // its player is told the speed when it opens, and tells it back (teams-media's tests time the speed itself)
    wait_rate(&view, cx, 2.0);
    // and while it plays
    wait_state(&view, cx, State::Playing);
    click(cx, "video-speed-recap");
    click(cx, "video-rate-1.5x");
    wait_rate(&view, cx, 1.5);
    cx.update(|_, cx| view.update(cx, VideoPlayer::pause));
    // X and Z step through the speeds, none past the fastest or the slowest
    cx.simulate_keystrokes("x x x");
    assert!((rate(cx) - 2.0).abs() < f64::EPSILON);
    for _ in 0..6 {
        cx.simulate_keystrokes("z");
    }
    assert!((rate(cx) - 0.8).abs() < f64::EPSILON);
    cx.simulate_keystrokes("x");
    assert!((rate(cx) - 1.0).abs() < f64::EPSILON);
    // its button opens and closes its menu; Esc closes it too
    click(cx, "video-speed-recap");
    click(cx, "video-speed-recap");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None);
    click(cx, "video-speed-recap");
    cx.simulate_keystrokes("escape");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None);
}

#[test]
fn the_settings_set_the_captions_look_and_show_the_quality_and_keys() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, cx) = captioned(&mut app);
    let style = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| CaptionStyle::current(cx));
    let menu = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| view.read(cx).menu());
    click(cx, "video-settings-recap");
    assert_eq!(menu(cx), Some(Menu::Settings(Page::Main)));
    // no quality before a frame came
    assert!(cx.debug_bounds("video-settings-quality").is_none());
    click(cx, "video-settings-captions");
    click(cx, "video-settings-size");
    click(cx, "video-caption-size-large");
    assert_eq!(style(cx).size, CaptionSize::Large);
    assert_eq!(menu(cx), Some(Menu::Settings(Page::Captions)));
    click(cx, "video-settings-color");
    click(cx, "video-caption-color-purple-reverse");
    assert_eq!(style(cx).color, CaptionColor::PurpleReverse);
    click(cx, "video-settings-transparency");
    assert!(style(cx).transparent);
    click(cx, "video-settings-revert");
    assert_eq!(style(cx), CaptionStyle::default());
    click(cx, "video-menu-back");
    click(cx, "video-settings-keys");
    assert_eq!(menu(cx), Some(Menu::Settings(Page::Keys)));
    // the gear closes its menu
    click(cx, "video-settings-recap");
    assert_eq!(menu(cx), None);
    // a click on the picture closes a menu, and doesn't play
    click(cx, "video-settings-recap");
    click(cx, "video-screen-recap");
    assert_eq!(menu(cx), None);
    assert_eq!(cx.update(|_, cx| view.read(cx).state()), None);
    // once a frame came, its quality: the picture's height
    cx.update(|_, cx| {
        view.read(cx).screen.frame(VideoFrame {
            width: 320,
            height: 180,
            format: PixelFormat::Bgra,
            stride: 1_280,
            data: vec![0; 1_280 * 180],
        });
    });
    click(cx, "video-settings-recap");
    click(cx, "video-settings-quality");
    click(cx, "video-quality-180");
    assert_eq!(menu(cx), None);
}

#[test]
fn a_chat_s_video_keeps_its_own_bar() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (_view, cx) = app.add_window_view(|_, cx| {
        let mut player = VideoPlayer::new("card".to_owned(), movie_file(), source, cx);
        // captions alone don't make a chat's card a recap's player
        player.set_captions(Some(made_up_captions()), cx);
        player
    });
    draw(cx);
    assert!(cx.debug_bounds("video-more-card").is_some());
    for selector in ["video-captions-card", "video-speed-card", "video-settings-card"] {
        assert!(cx.debug_bounds(selector).is_none(), "{selector}");
    }
}

/// A page under a chat's video player: counts the clicks that reach it.
struct UnderPage {
    player: Entity<VideoPlayer>,
    clicks: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Render for UnderPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clicks = std::rc::Rc::clone(&self.clicks);
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .id("under-page")
                    .absolute()
                    .size_full()
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
            )
            .child(div().w(px(320.0)).child(self.player.clone()))
    }
}

#[test]
fn the_full_screen_video_takes_the_window_s_clicks_alone() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let file = SharedFile {
        share_url: None,
        object_url: None,
        ..movie_file()
    };
    let clicks = std::rc::Rc::new(std::cell::Cell::new(0));
    let page_clicks = std::rc::Rc::clone(&clicks);
    let (page, cx) = app.add_window_view(move |_, cx| UnderPage {
        player: cx.new(|cx| VideoPlayer::new("under".to_owned(), file, source, cx)),
        clicks: page_clicks,
    });
    draw(cx);
    let far = cx.update(|window, _| {
        let viewport = window.viewport_size();
        point(viewport.width - px(8.0), viewport.height * 0.5)
    });
    cx.simulate_click(far, gpui::Modifiers::none());
    assert_eq!(clicks.get(), 1, "the page under the player takes a click");
    let player = cx.update(|_, cx| page.read(cx).player.clone());
    cx.update(|window, cx| player.update(cx, |player, cx| player.toggle_fullscreen(window, cx)));
    draw(cx);
    assert!(cx.update(|_, cx| player.read(cx).full_screen.on));
    cx.simulate_click(far, gpui::Modifiers::none());
    draw(cx);
    assert_eq!(
        clicks.get(),
        1,
        "a click on the full-screen video never reaches the page under it"
    );
}

gpui::actions!(units, [ListKey]);

/// A message's file card around a chat's player: the focus that the player's Escape gives back. Its list takes Left
/// and Right as the chat's does, and keeps them.
struct CardPage {
    card: FocusHandle,
    player: Entity<VideoPlayer>,
}

impl Render for CardPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("card-page")
            .key_context("CardPage")
            .on_action(|_: &ListKey, _, _| {})
            .track_focus(&self.card)
            .size_full()
            .child(div().w(px(320.0)).child(self.player.clone()))
    }
}

/// A chat's player in its card, the card's focus given to it as the card does.
fn in_card(app: &mut gpui::TestAppContext) -> (Entity<VideoPlayer>, FocusHandle, &mut gpui::VisualTestContext) {
    app.update(|cx| {
        crate::window::init(cx);
        cx.bind_keys(["left", "right"].map(|key| gpui::KeyBinding::new(key, ListKey, Some("CardPage"))));
    });
    let source = PictureSource::of(Arc::new(Files::default()));
    let (page, cx) = app.add_window_view(|_, cx| {
        let player = cx.new(|cx| VideoPlayer::new("card".to_owned(), movie_file(), source, cx));
        let card = cx.focus_handle();
        player.update(cx, |player, _| player.return_to(&card));
        CardPage { card, player }
    });
    draw(cx);
    let (player, card) = cx.update(|_, cx| {
        let page = page.read(cx);
        (page.player.clone(), page.card.clone())
    });
    (player, card, cx)
}

#[test]
fn escape_in_a_card_s_player_closes_its_menu_then_pauses_it_and_gives_the_keys_back_to_the_card() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, card, cx) = in_card(&mut app);
    cx.update(|window, cx| view.update(cx, |player, cx| player.play_from_keys(window, cx)));
    wait_state(&view, cx, State::Playing);
    assert!(
        cx.debug_bounds("video-controls-card shown").is_some(),
        "the keyboard brought the keys"
    );
    cx.update(|_, cx| view.update(cx, |player, cx| player.show_menu(Some(Menu::Speed), cx)));
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None, "the menu closes first");
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "the keys stay in the player"
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).asked), Asked::Play);
    // one update, so the 2 s movie can't end before the key comes
    let asked = cx.update(|window, cx| {
        view.update(cx, |player, _| {
            if let Some(status) = player.status.as_mut() {
                status.state = State::Playing;
            }
        });
        window.dispatch_keystroke(gpui::Keystroke::parse("escape").unwrap(), cx);
        view.read(cx).asked
    });
    draw(cx);
    assert_eq!(asked, Asked::Pause, "Escape pauses it");
    assert!(
        cx.update(|window, _| card.is_focused(window)),
        "the keys go back to the card"
    );
}

#[test]
fn a_pointer_s_click_on_a_card_s_video_leaves_its_controls_under_the_pointer_only() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, _card, cx) = in_card(&mut app);
    // the keyboard's keys pin them until the pointer presses in the player
    cx.update(|window, cx| view.update(cx, |player, cx| player.play_from_keys(window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("video-controls-card shown").is_some());
    let screen = cx.debug_bounds("video-screen-card").unwrap();
    cx.simulate_click(screen.origin + point(px(10.0), px(10.0)), gpui::Modifiers::none());
    draw(cx);
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "the click gave it the keys"
    );
    cx.simulate_mouse_move(point(px(600.0), px(600.0)), None, gpui::Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("video-controls-card on-hover").is_some(),
        "the pointer's keys don't pin them"
    );
}

#[test]
fn left_and_right_skip_in_a_card_s_player_before_the_list_around_it_takes_them() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, _card, cx) = in_card(&mut app);
    cx.update(|window, cx| view.update(cx, |player, cx| player.play_from_keys(window, cx)));
    wait_state(&view, cx, State::Playing);
    cx.update(|_, cx| view.update(cx, VideoPlayer::pause));
    wait_state(&view, cx, State::Paused);
    // one update, so the player's report of the movie's real length can't overwrite the status between keys
    let positions = cx.update(|window, cx| {
        view.update(cx, |player, _| {
            player.status = Some(Status {
                state: State::Paused,
                position: Duration::from_secs(15),
                duration: Duration::from_secs(30),
                sound: false,
                rate: 1.0,
            });
        });
        let mut after = |key: &'static str| {
            window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), cx);
            view.read(cx).status.map(|status| status.position)
        };
        [after("left"), after("right")]
    });
    assert_eq!(positions, [Some(Duration::from_secs(5)), Some(Duration::from_secs(15))]);
}

/// A chat's player's controls (its own `div`s) in the order Tab reaches them: the kit's "More options" comes between
/// mute and full screen.
const CARD_CONTROLS: [&str; 6] = ["play", "back", "forward", "mute", "fullscreen", "open"];

/// Which of `controls` has the keys: `Some("")` when another place in the player has them.
fn focused_control(
    view: &Entity<VideoPlayer>,
    controls: &[&'static str],
    cx: &mut gpui::VisualTestContext,
) -> Option<&'static str> {
    cx.update(|window, cx| {
        let player = view.read(cx);
        let named = controls
            .iter()
            .find(|what| player.control_focus(what, cx).is_focused(window))
            .copied();
        named.or_else(|| player.holds_keys(window, cx).then_some(""))
    })
}

/// Tab (or Shift+Tab) as the kit's root moves it in the app's window, which the test's has not.
fn tab(cx: &mut gpui::VisualTestContext, back: bool) {
    cx.update(|window, cx| {
        if back {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
    });
    draw(cx);
}

/// `key` down and up, as a person presses it: a focused control is clicked on its release.
fn press(cx: &mut gpui::VisualTestContext, key: &'static str) {
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    draw(cx);
}

/// `key` down and up in one update, so a player's status can't change between them.
fn press_now(window: &mut Window, cx: &mut App, key: &'static str) {
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    window.dispatch_event(
        gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
        cx,
    );
    window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
}

/// The ring marker on the card's control `what`, with the tip it shows (`what: tip`).
fn ring(cx: &mut gpui::VisualTestContext, what: &str) -> Option<String> {
    let names = [
        "Play",
        "Pause",
        BACK,
        FORWARD,
        "Mute",
        "Unmute",
        "Full screen",
        "Exit full screen",
        OPEN_IN_BROWSER,
    ];
    let rings: Vec<String> = names
        .into_iter()
        .map(|name| format!("{what}: {name}"))
        .filter(|ring| {
            cx.debug_bounds(Box::leak(format!("video-ring-card {ring}").into_boxed_str()))
                .is_some()
        })
        .collect();
    (rings.len() == 1).then(|| rings.concat())
}

/// The keys on control `what` of `view`.
fn focus_control(view: &Entity<VideoPlayer>, what: &str, cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| view.read(cx).control_focus(what, cx).focus(window, cx));
    draw(cx);
}

#[test]
fn tab_goes_through_each_control_of_a_card_s_player_in_their_order_and_back() {
    let mut app = gpui::TestAppContext::single();
    let (view, _card, cx) = in_card(&mut app);
    // the keys in its picture, where the card's Enter puts them, without playing it
    cx.update(|window, cx| {
        view.update(cx, |player, _| player.keys.from_keyboard = true);
        view.read(cx).focus.clone().focus(window, cx);
    });
    draw(cx);
    let mut reached = Vec::new();
    let mut ringed = Vec::new();
    for _ in 0..=CARD_CONTROLS.len() {
        tab(cx, false);
        let control = focused_control(&view, &CARD_CONTROLS, cx);
        reached.push(control);
        if let Some(what) = control.filter(|what| !what.is_empty()) {
            ringed.push(ring(cx, what));
        }
    }
    let names = ["Play", BACK, FORWARD, "Mute", "Full screen", OPEN_IN_BROWSER];
    let wanted: Vec<_> = CARD_CONTROLS
        .iter()
        .zip(names)
        .map(|(what, name)| Some(format!("{what}: {name}")))
        .collect();
    assert_eq!(
        ringed, wanted,
        "each control has the ring marker with its tip while it has the keys"
    );
    let more = Some("");
    let expected = [
        Some("play"),
        Some("back"),
        Some("forward"),
        Some("mute"),
        more,
        Some("fullscreen"),
        Some("open"),
    ];
    assert_eq!(reached, expected, "\"More options\" is the kit's button");
    assert!(
        cx.debug_bounds("video-controls-card shown").is_some(),
        "the controls show while the keyboard is on them"
    );
    let mut back = Vec::new();
    for _ in 0..CARD_CONTROLS.len() {
        tab(cx, true);
        back.push(focused_control(&view, &CARD_CONTROLS, cx));
    }
    let mut reversed = expected[..CARD_CONTROLS.len()].to_vec();
    reversed.reverse();
    assert_eq!(back, reversed, "Shift+Tab goes back the same way");
}

#[test]
fn enter_or_space_on_play_and_the_skips_press_only_that_control() {
    let _playing = playing_slot();
    let mut app = gpui::TestAppContext::single();
    let (view, _card, cx) = in_card(&mut app);
    focus_control(&view, "play", cx);
    press(cx, "space");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).asked),
        Asked::Play,
        "Space on Play plays it"
    );
    press(cx, "enter");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).asked),
        Asked::Pause,
        "Enter on Pause pauses it"
    );
    assert_eq!(focused_control(&view, &CARD_CONTROLS, cx), Some("play"));
    wait_state(&view, cx, State::Paused);
    // one update, so the player's report of the movie's real length can't overwrite the status between keys
    let positions = cx.update(|window, cx| {
        view.update(cx, |player, _| {
            player.status = Some(Status {
                state: State::Paused,
                position: Duration::from_secs(15),
                duration: Duration::from_secs(30),
                sound: false,
                rate: 1.0,
            });
        });
        let mut after = |what: &str, key: &'static str| {
            view.read(cx).control_focus(what, cx).focus(window, cx);
            press_now(window, cx, key);
            view.read(cx).status.map(|status| status.position)
        };
        [after("back", "space"), after("forward", "enter")]
    });
    assert_eq!(positions, [Some(Duration::from_secs(5)), Some(Duration::from_secs(15))]);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).asked),
        Asked::Pause,
        "the skips leave it paused"
    );
}

#[test]
fn enter_or_space_on_mute_full_screen_and_the_browser_press_only_that_control() {
    let mut app = gpui::TestAppContext::single();
    let (view, card, cx) = in_card(&mut app);
    focus_control(&view, "mute", cx);
    press(cx, "space");
    assert!(cx.update(|_, cx| view.read(cx).muted));
    press(cx, "enter");
    assert!(!cx.update(|_, cx| view.read(cx).muted));
    focus_control(&view, "fullscreen", cx);
    press(cx, "enter");
    assert!(cx.update(|_, cx| view.read(cx).full_screen.on));
    assert_eq!(
        focused_control(&view, &CARD_CONTROLS, cx),
        Some("fullscreen"),
        "the keys stay on the button, over the whole window"
    );
    press(cx, "escape");
    assert!(
        !cx.update(|_, cx| view.read(cx).full_screen.on),
        "Escape ends full screen first"
    );
    assert_eq!(focused_control(&view, &CARD_CONTROLS, cx), Some("fullscreen"));
    assert!(cx.opened_url().is_none());
    focus_control(&view, "open", cx);
    press(cx, "space");
    assert_eq!(cx.opened_url().as_deref(), movie_file().open_link());
    assert!(cx.update(|_, cx| view.read(cx).player.is_none()), "no key played it");
    press(cx, "escape");
    assert!(
        cx.update(|window, _| card.is_focused(window)),
        "then Escape gives the keys back to the card"
    );
}

#[test]
fn a_pointer_s_click_on_a_control_leaves_the_keys_where_they_are() {
    let mut app = gpui::TestAppContext::single();
    let (view, card, cx) = in_card(&mut app);
    cx.update(|window, cx| card.focus(window, cx));
    click(cx, "video-mute-card");
    assert!(cx.update(|_, cx| view.read(cx).muted), "the click pressed it");
    assert!(
        cx.update(|window, _| card.is_focused(window)),
        "the keys stayed on the card"
    );
    assert_eq!(focused_control(&view, &CARD_CONTROLS, cx), None);
}

#[test]
fn tab_goes_through_each_control_of_a_recap_s_player_in_their_order() {
    let mut app = gpui::TestAppContext::single();
    let (view, cx) = captioned(&mut app);
    let controls = [
        "play",
        "back",
        "forward",
        "mute",
        "captions",
        "speed",
        "settings",
        "fullscreen",
        "open",
    ];
    cx.update(|window, cx| view.read(cx).focus.clone().focus(window, cx));
    draw(cx);
    let mut reached = Vec::new();
    for _ in 0..controls.len() {
        tab(cx, false);
        reached.push(focused_control(&view, &controls, cx));
    }
    assert_eq!(reached, controls.map(Some));
    focus_control(&view, "speed", cx);
    press(cx, "enter");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).menu()),
        Some(Menu::Speed),
        "Enter opens its menu"
    );
    assert_eq!(
        focused_control(&view, &controls, cx),
        Some("speed"),
        "the keys stay on its button"
    );
    press(cx, "escape");
    assert_eq!(cx.update(|_, cx| view.read(cx).menu()), None, "Escape closes it");
}

#[test]
fn full_screen_from_the_pointer_leaves_the_keys_on_another_control_and_escape_ends_it() {
    let mut app = gpui::TestAppContext::single();
    let (view, _card, cx) = in_card(&mut app);
    focus_control(&view, "mute", cx);
    click(cx, "video-fullscreen-card");
    assert!(cx.update(|_, cx| view.read(cx).full_screen.on));
    assert_eq!(
        focused_control(&view, &CARD_CONTROLS, cx),
        Some("mute"),
        "the keys stay on Mute, which full screen draws again"
    );
    press(cx, "escape");
    assert!(
        !cx.update(|_, cx| view.read(cx).full_screen.on),
        "Escape ends full screen"
    );
    assert_eq!(focused_control(&view, &CARD_CONTROLS, cx), Some("mute"));
}

#[test]
fn full_screen_from_the_pointer_brings_the_keys_from_outside_into_the_player() {
    let mut app = gpui::TestAppContext::single();
    let (view, card, cx) = in_card(&mut app);
    cx.update(|window, cx| card.focus(window, cx));
    click(cx, "video-fullscreen-card");
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "the keys go to its picture, for its Escape"
    );
    press(cx, "escape");
    assert!(!cx.update(|_, cx| view.read(cx).full_screen.on));
}

/// A chat's player in its card in the kit's root, which takes Tab, as the app's window has it: the card is a Tab stop
/// before the player's controls.
fn in_root(app: &mut gpui::TestAppContext) -> (Entity<VideoPlayer>, &mut gpui::VisualTestContext) {
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let page = cx.new(|cx| {
            let player = cx.new(|cx| VideoPlayer::new("card".to_owned(), movie_file(), source, cx));
            let card = cx.focus_handle().tab_stop(true);
            player.update(cx, |player, _| player.return_to(&card));
            CardPage { card, player }
        });
        crate::window::KitRoot::new(page, window, cx)
    });
    draw(cx);
    let player = cx.update(|_, cx| {
        let page = root.read(cx).view().clone().downcast::<CardPage>().unwrap();
        page.read(cx).player.clone()
    });
    (player, cx)
}

#[test]
fn tab_and_shift_tab_stay_in_a_full_screen_video() {
    let mut app = gpui::TestAppContext::single();
    let (view, cx) = in_root(&mut app);
    focus_control(&view, "fullscreen", cx);
    press(cx, "enter");
    assert!(cx.update(|_, cx| view.read(cx).full_screen.on));
    focus_control(&view, "open", cx);
    press(cx, "tab");
    assert_eq!(
        focused_control(&view, &CARD_CONTROLS, cx),
        Some("play"),
        "Tab from the last control goes round to the first, not to the hidden page"
    );
    press(cx, "shift-tab");
    assert_eq!(
        focused_control(&view, &CARD_CONTROLS, cx),
        Some("open"),
        "Shift+Tab from the first goes round to the last"
    );
    press(cx, "escape");
    assert!(
        !cx.update(|_, cx| view.read(cx).full_screen.on),
        "Escape still ends full screen"
    );
}
