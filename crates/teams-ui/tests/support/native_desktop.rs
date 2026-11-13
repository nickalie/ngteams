//! Actual native shell contract; no Teams services or account.
#[path = "../../src/desktop/windows/activation.rs"]
mod activation;
#[path = "../../src/desktop/windows/apartment.rs"]
mod apartment;
use apartment::Apartment;
#[path = "native_toast_driver.rs"]
mod driver;
#[path = "../../src/desktop/windows/registration.rs"]
mod registration;
#[path = "../../src/desktop/windows/toasts.rs"]
mod toasts;
#[path = "../../src/desktop/windows/tray.rs"]
mod tray;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use teams_ui::notify::picture::{BANNER_DIR, file_uri};
use teams_ui::notify::{
    BannerFiles, BannerPicture, JOIN_ACTION, Notice, NoticeKind, NotificationServer as _, PictureShape,
};
use teams_ui::tray::{TrayEvent, TrayIcon as _};
use windows::{
    UI::Notifications::{ToastNotificationHistory, ToastNotificationManager},
    core::HSTRING,
};

pub fn main() -> std::process::ExitCode {
    if std::env::var_os("NGTEAMS_NATIVE_DESKTOP_REQUIRE").is_none() {
        println!("native desktop requires the authorized win11 console helper");
        return std::process::ExitCode::SUCCESS;
    }
    teams_platform::windows_sandbox::window_allowed_here().expect("authorized console");
    let _apartment = Apartment::new().expect("fixture apartment");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(75), contract())
            .await
            .expect("native desktop deadline");
    });
    std::process::ExitCode::SUCCESS
}
async fn contract() {
    let app_id = format!("com.nickalie.ngteams.synthetic.{}", std::process::id());
    teams_platform::windows_desktop::set_app_id(&app_id).unwrap();
    let (files, folder) = banner_folder();
    let (server, activations) =
        toasts::Notifications::start(app_id.clone(), "NGTEAMS SYNTHETIC DESKTOP".into(), Some(files))
            .await
            .unwrap();
    notification_setting(&app_id);
    let (tray, events) = tray::NativeTray::start("NGTEAMS SYNTHETIC DESKTOP".into())
        .await
        .unwrap();
    assert!(matches!(events.recv().await.unwrap(), tray::Event::Hosted(true)));
    let notice = Notice {
        conversation: "synthetic-chat".into(),
        kind: NoticeKind::Message,
        summary: "NGTEAMS SYNTHETIC DESKTOP".into(),
        body: "Synthetic message <one> & two".into(),
        picture: None,
    };
    // SAFETY: read-only shell availability query; no notification preference is changed.
    println!("synthetic shell notification state={:?}", unsafe {
        windows::Win32::UI::Shell::SHQueryUserNotificationState()
    });
    println!(
        "synthetic notification mode={:?}",
        ToastNotificationManager::GetDefault().and_then(|manager| manager.NotificationMode())
    );
    activation_contract(&server, &activations, &notice).await;
    history_contract(&server, &app_id, &notice).await;
    notification_setting(&app_id);
    stale_activation_contract(&server, &activations, &app_id, &notice).await;
    picture_contract(&server, &activations, &app_id, &folder, &notice).await;
    meeting_contract(&server, &activations, &app_id, &folder, &notice).await;
    tray_contract(&tray, &events).await;
    stop_contract(server, &app_id, &folder, &notice).await;
    drop(tray);
    while events.recv().await.is_ok() {}
    println!("native desktop contract passed");
}
async fn history_contract(server: &toasts::Notifications, app_id: &str, notice: &Notice) {
    let first = server.show(notice, 0).await.unwrap();
    let second = server.show(notice, 0).await.unwrap();
    assert_ne!(first, second, "all accounts share unique IDs");
    let updated = Notice {
        kind: NoticeKind::Collapsed,
        body: "4 new synthetic messages".into(),
        ..notice.clone()
    };
    assert_eq!(server.show(&updated, first).await.unwrap(), first);
    let history = ToastNotificationManager::History().unwrap();
    wait_history(&history, app_id, 2).await;
    let shown = history.GetHistoryWithId(&HSTRING::from(app_id)).unwrap();
    let contents: Vec<String> = (0..shown.Size().unwrap())
        .map(|i| {
            shown
                .GetAt(i)
                .unwrap()
                .Content()
                .unwrap()
                .GetXml()
                .unwrap()
                .to_string_lossy()
        })
        .collect();
    assert!(contents.iter().any(|xml| xml.contains("&lt;one&gt; &amp; two")));
    assert!(
        contents.iter().all(|xml| !xml.contains("silent=\"true\"")),
        "message sound follows the OS preference"
    );
    assert!(
        contents.iter().all(|xml| !xml.contains("synthetic-chat")),
        "routing IDs stay out of Windows launch arguments"
    );
    server.close(second).await;
    wait_history(&history, app_id, 1).await;
    server.close(first).await;
    wait_history(&history, app_id, 0).await;
}
async fn tray_contract(tray: &tray::NativeTray, events: &async_channel::Receiver<tray::Event>) {
    let hwnd = tray.test_window().await;
    assert_ne!(hwnd, 0);
    tray.set_window(Some(hwnd));
    tray.set_unread(7).await;
    assert!(tray.test_overlay().await, "real taskbar accepts the unread overlay");
    tray_activation_contract(tray, events).await;
    overlay_lifecycle_contract(tray).await;
}
async fn overlay_lifecycle_contract(tray: &tray::NativeTray) {
    tray.test_explorer_restart();
    tray.set_unread(9).await;
    assert!(tray.test_overlay().await);
    tray.set_unread(0).await;
    assert!(tray.test_overlay().await, "real taskbar clears the unread overlay");
    tray.set_window(None);
    let reopened = tray.test_window().await;
    tray.set_window(Some(reopened));
    tray.set_unread(12).await;
    assert!(tray.test_overlay().await, "overlay follows a recreated window");
    let before = tray.test_overlay_count().await;
    // The early Explorer broadcast may race the new taskbar button. The later main-window notification must
    // reapply the unchanged unread overlay, independently of any unread or window update.
    tray::NativeTray::test_button_ready(reopened);
    assert_eq!(tray.test_overlay_count().await, before + 1);
    println!("TaskbarButtonCreated on the recreated main HWND reapplied its unchanged unread overlay");
    wait_for_overlay_shot().await;
}

async fn wait_for_overlay_shot() {
    if let Some(directory) = std::env::var_os("NGTEAMS_NATIVE_TASKBAR_SHOT") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::write(directory.join("overlay-ready"), std::process::id().to_string()).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !directory.join("overlay-shot").exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "taskbar screenshot was not captured"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
async fn activation_contract(
    server: &toasts::Notifications,
    activations: &async_channel::Receiver<(u32, String)>,
    notice: &Notice,
) {
    for (title, action, kind) in [
        (
            "NGTEAMS SYNTHETIC OPEN",
            teams_ui::notify::DEFAULT_ACTION,
            NoticeKind::Message,
        ),
        ("NGTEAMS SYNTHETIC ANSWER", "answer", NoticeKind::Call),
        ("NGTEAMS SYNTHETIC DECLINE", "decline", NoticeKind::Call),
    ] {
        let call = Notice {
            kind,
            summary: title.into(),
            body: "Synthetic caller".into(),
            ..notice.clone()
        };
        let id = server.show(&call, 0).await.unwrap();
        println!("synthetic toast ready: {title}");
        driver::click(title, action).await;
        let actual = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .expect("toast activation callback deadline")
            .unwrap();
        assert_eq!(actual, (id, action.to_owned()), "real toast action carries its own ID");
        server.close(id).await;
        driver::closed(title).await;
    }
}
async fn wait_history(history: &ToastNotificationHistory, app_id: &str, count: u32) {
    for _ in 0..100 {
        if history
            .GetHistoryWithId(&HSTRING::from(app_id))
            .unwrap()
            .Size()
            .unwrap()
            == count
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("toast history did not reach {count}");
}

async fn tray_activation_contract(tray: &tray::NativeTray, events: &async_channel::Receiver<tray::Event>) {
    for event in [
        windows_sys::Win32::UI::Shell::NIN_SELECT,
        windows_sys::Win32::UI::Shell::NIN_SELECT | windows_sys::Win32::UI::Shell::NINF_KEY,
    ] {
        tray.test_callback(event);
        assert!(matches!(
            events.recv().await.unwrap(),
            tray::Event::Asked(TrayEvent::Open)
        ));
    }
}

// Exercise the actual COM class factory, including reusing a notification ID after replacement.
async fn stale_activation_contract(
    server: &toasts::Notifications,
    events: &async_channel::Receiver<(u32, String)>,
    app_id: &str,
    notice: &Notice,
) {
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
    use windows::Win32::UI::Notifications::INotificationActivationCallback;
    let clsid = registration::activator_id(app_id);
    // SAFETY: this initialized MTA connects only to our exact synthetic COM class.
    let callback: INotificationActivationCallback =
        unsafe { CoCreateInstance(&raw const clsid, None, CLSCTX_LOCAL_SERVER) }.unwrap();
    let history = ToastNotificationManager::History().unwrap();
    let id = server.show(notice, 0).await.unwrap();
    wait_history(&history, app_id, 1).await;
    let old = launch_argument(&history, app_id);
    assert_eq!(server.show(notice, id).await.unwrap(), id);
    let mut current = old.clone();
    for _ in 0..100 {
        current = launch_argument(&history, app_id);
        if current != old {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_ne!(old, current);
    for (identity, arguments) in [
        ("com.nickalie.ngteams.synthetic.invalid", current.clone()),
        (app_id, old),
        (app_id, current.replace(":default", ":unknown")),
    ] {
        // SAFETY: terminated synthetic strings; no user input data or tenant action.
        unsafe { callback.Activate(&HSTRING::from(identity), &HSTRING::from(arguments), &[]) }.unwrap();
    }
    server.close(id).await;
    wait_history(&history, app_id, 0).await;
    // SAFETY: checks a closed notification's opaque key against the same owned synthetic COM callback.
    unsafe { callback.Activate(&HSTRING::from(app_id), &HSTRING::from(current), &[]) }.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        events.is_empty(),
        "stale, foreign and unknown COM activations are ignored"
    );
    println!("synthetic COM stale activation contract passed");
}
fn launch_argument(history: &ToastNotificationHistory, app_id: &str) -> String {
    history
        .GetHistoryWithId(&HSTRING::from(app_id))
        .unwrap()
        .GetAt(0)
        .unwrap()
        .Content()
        .unwrap()
        .DocumentElement()
        .unwrap()
        .GetAttribute(&HSTRING::from("launch"))
        .unwrap()
        .to_string_lossy()
}

fn notification_setting(app_id: &str) {
    match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
        .and_then(|notifier| notifier.Setting())
    {
        Ok(setting) => println!("synthetic notification setting={}", setting.0),
        Err(error) => println!(
            "synthetic notification setting query unavailable ({:#x})",
            error.code().0
        ),
    }
}

/// #114: a toast with a picture shows it in the app logo's place, a person's cropped to a circle and an app's square;
/// its PNG is in the banner folder while the toast is shown and is removed on close, replace and activation.
async fn picture_contract(
    server: &toasts::Notifications,
    activations: &async_channel::Receiver<(u32, String)>,
    app_id: &str,
    folder: &Path,
    notice: &Notice,
) {
    let toasts = Pictured {
        server,
        history: ToastNotificationManager::History().unwrap(),
        app_id,
        folder,
    };
    wait_history(&toasts.history, app_id, 0).await;
    let person = Notice {
        summary: "NGTEAMS SYNTHETIC PERSON".into(),
        picture: Some(picture(PictureShape::Circle)),
        ..notice.clone()
    };
    let app = Notice {
        kind: NoticeKind::Activity,
        summary: "NGTEAMS SYNTHETIC APP".into(),
        body: "Synthetic app item".into(),
        picture: Some(picture(PictureShape::Square)),
        ..notice.clone()
    };
    let (person_id, person_file) = toasts.show(&person, PictureShape::Circle).await;
    let (app_toast, app_file) = toasts.show(&app, PictureShape::Square).await;
    toasts.close_removes(person_id, &person_file, &app_file).await;
    let replaced = toasts.replace_removes(&app, app_toast, &app_file).await;
    picture_activation_contract(activations, app_id, app_toast, folder, &replaced).await;
    server.close(app_toast).await;
    wait_history(&toasts.history, app_id, 0).await;
    assert!(banner_files(folder).is_empty());
}

/// #125: a started meeting's toast shows its starter's picture cropped to a circle and a Join button; Join, clicked on
/// the real toast, reaches the app as the toast's own `join` and removes the picture's PNG.
async fn meeting_contract(
    server: &toasts::Notifications,
    activations: &async_channel::Receiver<(u32, String)>,
    app_id: &str,
    folder: &Path,
    notice: &Notice,
) {
    const TITLE: &str = "NGTEAMS SYNTHETIC MEETING";
    let toasts = Pictured {
        server,
        history: ToastNotificationManager::History().unwrap(),
        app_id,
        folder,
    };
    wait_history(&toasts.history, app_id, 0).await;
    let meeting = Notice {
        kind: NoticeKind::Meeting,
        summary: TITLE.into(),
        body: "Synthetic starter started the meeting".into(),
        picture: Some(picture(PictureShape::Circle)),
        ..notice.clone()
    };
    let (id, file) = toasts.show(&meeting, PictureShape::Circle).await;
    let xml = toast_xml(&toasts.history, app_id, id);
    assert!(
        xml.contains(r#"<action content="Join""#) && xml.contains(&format!(":{JOIN_ACTION}\"")),
        "the meeting toast offers Join: {xml}"
    );
    assert!(!xml.contains("incomingCall"), "a meeting toast is no call: {xml}");
    println!("synthetic toast ready: {TITLE}");
    driver::click(TITLE, JOIN_ACTION).await;
    let actual = tokio::time::timeout(Duration::from_secs(3), activations.recv())
        .await
        .expect("toast activation callback deadline")
        .unwrap();
    assert_eq!(
        actual,
        (id, JOIN_ACTION.to_owned()),
        "the real toast's Join carries its own ID"
    );
    wait_gone(folder, &file).await;
    assert!(banner_files(folder).is_empty(), "Join removes the starter's picture");
    println!("synthetic meeting toast's Join reached the app; its picture {file} removed");
    server.close(id).await;
    driver::closed(TITLE).await;
    wait_history(&toasts.history, app_id, 0).await;
}

/// The toasts with pictures of [`picture_contract`].
struct Pictured<'a> {
    server: &'a toasts::Notifications,
    history: ToastNotificationHistory,
    app_id: &'a str,
    folder: &'a Path,
}

impl Pictured<'_> {
    /// Shows `notice` as a new toast: its ID and its picture's file, checked in its XML (`shape`) and as a 96-pixel
    /// PNG of its own in the banner folder.
    async fn show(&self, notice: &Notice, shape: PictureShape) -> (u32, String) {
        let before = banner_files(self.folder).len();
        let id = self.server.show(notice, 0).await.unwrap();
        wait_history(&self.history, self.app_id, u32::try_from(before).unwrap() + 1).await;
        let file = picture_file(&toast_xml(&self.history, self.app_id, id), self.folder, shape);
        let png = image::open(self.folder.join(&file)).expect("a PNG").into_rgba8();
        assert_eq!(png.dimensions(), (96, 96), "{file}");
        assert_eq!(
            banner_files(self.folder).len(),
            before + 1,
            "each toast's picture is a file of its own"
        );
        println!("synthetic toast shows its {shape:?} picture {file}");
        (id, file)
    }

    /// Closing toast `id` removes its `file` and keeps the other toast's `kept`.
    async fn close_removes(&self, id: u32, file: &str, kept: &str) {
        self.server.close(id).await;
        wait_history(&self.history, self.app_id, 1).await;
        wait_gone(self.folder, file).await;
        assert_eq!(
            banner_files(self.folder),
            vec![kept.to_owned()],
            "closing one toast keeps the other's picture"
        );
        println!("synthetic banner picture removed at the close");
    }

    /// Replacing toast `id` (of `app`, its picture `old`) writes a new file and removes `old`; the new file.
    async fn replace_removes(&self, app: &Notice, id: u32, old: &str) -> String {
        let updated = Notice {
            body: "Synthetic app item, updated".into(),
            ..app.clone()
        };
        assert_eq!(self.server.show(&updated, id).await.unwrap(), id);
        wait_gone(self.folder, old).await;
        let replaced = replaced_picture(&self.history, self.app_id, id, self.folder).await;
        assert_ne!(replaced, old, "the replacement's picture is a new file");
        assert_eq!(
            banner_files(self.folder),
            vec![replaced.clone()],
            "replacing removes the old picture"
        );
        println!("synthetic banner picture removed at the replace");
        replaced
    }
}

/// The picture file of toast `id` once the history holds its replacement (its body "updated").
async fn replaced_picture(history: &ToastNotificationHistory, app_id: &str, id: u32, folder: &Path) -> String {
    for _ in 0..100 {
        let xml = toast_xml(history, app_id, id);
        if xml.contains("updated") {
            return picture_file(&xml, folder, PictureShape::Square);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("the replacement is not in the toast history");
}

/// Activating the shown toast `id` (its own launch argument, through the real COM class) removes its `file`.
async fn picture_activation_contract(
    activations: &async_channel::Receiver<(u32, String)>,
    app_id: &str,
    id: u32,
    folder: &Path,
    file: &str,
) {
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance};
    use windows::Win32::UI::Notifications::INotificationActivationCallback;
    let clsid = registration::activator_id(app_id);
    // SAFETY: this initialized MTA connects only to our exact synthetic COM class.
    let callback: INotificationActivationCallback =
        unsafe { CoCreateInstance(&raw const clsid, None, CLSCTX_LOCAL_SERVER) }.unwrap();
    let launch = launch_argument(&ToastNotificationManager::History().unwrap(), app_id);
    // SAFETY: terminated synthetic strings; the shown synthetic toast's own launch argument.
    unsafe { callback.Activate(&HSTRING::from(app_id), &HSTRING::from(launch), &[]) }.unwrap();
    let activated = tokio::time::timeout(Duration::from_secs(3), activations.recv())
        .await
        .expect("toast activation callback deadline")
        .unwrap();
    assert_eq!(activated, (id, teams_ui::notify::DEFAULT_ACTION.to_owned()));
    wait_gone(folder, file).await;
    println!("synthetic banner picture removed at the activation");
}

/// A shown toast's picture is removed when the toasts stop.
async fn stop_contract(server: toasts::Notifications, app_id: &str, folder: &Path, notice: &Notice) {
    let pictured = Notice {
        picture: Some(picture(PictureShape::Circle)),
        ..notice.clone()
    };
    server.show(&pictured, 0).await.unwrap();
    let history = ToastNotificationManager::History().unwrap();
    wait_history(&history, app_id, 1).await;
    assert_eq!(
        banner_files(folder).len(),
        1,
        "the shown toast's picture is in the banner folder"
    );
    drop(server);
    assert!(
        banner_files(folder).is_empty(),
        "the stop removes a shown toast's picture"
    );
    println!("synthetic banner picture removed at the stop");
    wait_history(&history, app_id, 0).await;
}

/// The app's own banner folder (#114), in the console helper's private run directory: its files, and its path.
fn banner_folder() -> (BannerFiles, std::path::PathBuf) {
    let data = Path::new(&std::env::var_os("UI_SANDBOX_DIR").expect("private run directory")).join("banner-data");
    let files = BannerFiles::open(&data).expect("banner folder");
    (files, data.join(BANNER_DIR))
}

/// A one-colour synthetic picture: a person's (`Circle`) as the notifier makes it from a stored photo, an app's tile
/// (`Square`) from its tile's bytes.
fn picture(shape: PictureShape) -> BannerPicture {
    let rgba = image::RgbaImage::from_pixel(120, 120, image::Rgba([200, 30, 30, 255]));
    let mut png = Vec::new();
    rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    match shape {
        PictureShape::Circle => BannerPicture::of_person(&teams_store::StoredPicture {
            media_type: "image/png".into(),
            bytes: png,
        }),
        PictureShape::Square => BannerPicture::of_app(&Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png,
        })),
    }
    .expect("synthetic picture")
}

/// The names of the PNG files in the banner folder, sorted.
fn banner_files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        })
        .collect();
    names.sort();
    names
}

async fn wait_gone(folder: &Path, file: &str) {
    for _ in 0..100 {
        if !folder.join(file).exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("banner picture {file} was not removed");
}

/// The XML of shown toast `id` (its tag) in the toast history.
fn toast_xml(history: &ToastNotificationHistory, app_id: &str, id: u32) -> String {
    let shown = history.GetHistoryWithId(&HSTRING::from(app_id)).unwrap();
    (0..shown.Size().unwrap())
        .map(|i| shown.GetAt(i).unwrap())
        .find(|toast| toast.Tag().unwrap().to_string_lossy() == id.to_string())
        .expect("toast in the history")
        .Content()
        .unwrap()
        .GetXml()
        .unwrap()
        .to_string_lossy()
}

/// The file name of the picture `xml` shows in the app logo's place, checked to be a file of `folder` and cropped to
/// a circle for a person's only.
fn picture_file(xml: &str, folder: &Path, shape: PictureShape) -> String {
    let start = xml.find("<image ").expect("the toast has a picture");
    let element = &xml[start..start + xml[start..].find('>').unwrap()];
    assert!(element.contains(r#"placement="appLogoOverride""#), "{element}");
    assert_eq!(
        element.contains(r#"hint-crop="circle""#),
        shape == PictureShape::Circle,
        "only a person's picture is cropped to a circle: {element}"
    );
    let src = element
        .split(r#"src=""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    let name = src.rsplit('/').next().unwrap().to_owned();
    assert_eq!(
        src,
        file_uri(&folder.join(&name)),
        "the picture is a file of the banner folder"
    );
    name
}
