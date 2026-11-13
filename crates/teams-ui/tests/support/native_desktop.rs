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
use std::time::Duration;
use teams_ui::notify::{Notice, NoticeKind, NotificationServer as _};
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
    let (server, activations) = toasts::Notifications::start(app_id.clone(), "NGTEAMS SYNTHETIC DESKTOP".into(), None)
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
    tray_contract(&tray, &events).await;
    server.show(&notice, 0).await.unwrap();
    let history = ToastNotificationManager::History().unwrap();
    drop(server);
    wait_history(&history, &app_id, 0).await;
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
