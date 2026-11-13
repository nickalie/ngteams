//! The production Desktop, native transports and real GPUI windows; all engine data is made up.
use crate::common::{FakeCore, LAB_A, LAB_B};
use gpui::{App, AppContext as _, AsyncApp, Entity};
use std::{path::PathBuf, sync::Arc, time::Duration};
use teams_core::Event;
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::{
    AccountParts, Core, Desktop, DesktopEvent, DesktopParts, Models, SettingsModel, ShellParts, ViewTrace,
    desktop::WindowShown, settings::Settings, settings_view::Page, shell::Main,
};

const TITLE: &str = "NGTEAMS SYNTHETIC SHELL";
const CHAT: &str = "19:synthetic-shell@thread.v2";

pub fn main() -> std::process::ExitCode {
    if std::env::var_os("NGTEAMS_NATIVE_SHELL_REQUIRE").is_none() {
        println!("native shell requires the authorized win11 console helper");
        return std::process::ExitCode::SUCCESS;
    }
    teams_platform::windows_sandbox::window_allowed_here().expect("authorized console");
    let identity = format!("com.nickalie.ngteams.synthetic.{}", std::process::id());
    teams_platform::windows_desktop::set_app_id(&identity).unwrap();
    let result = Arc::new(std::sync::Mutex::new(false));
    let outcome = Arc::clone(&result);
    gpui_platform::application()
        .with_assets(teams_ui::AppAssets)
        .run(move |cx: &mut App| {
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            let (desktop, cores, models) = desktop(Box::leak(identity.into_boxed_str()), cx);
            cx.subscribe(&desktop, |_, event, cx| {
                if *event == DesktopEvent::Quit {
                    cx.quit();
                }
            })
            .detach();
            let connected = desktop.update(cx, |desktop, cx| {
                desktop.open_window(cx).unwrap();
                desktop.connect(Ok(String::new()), cx)
            });
            cx.spawn(async move |cx: &mut AsyncApp| {
                assert!(connected.await.is_empty(), "native transports connected");
                exercise(&desktop, &cores, &models, cx).await;
                *outcome.lock().unwrap() = true;
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if *result.lock().unwrap() {
        println!("native shell contract passed");
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn desktop(id: &'static str, cx: &mut App) -> (Entity<Desktop>, [Arc<FakeCore>; 2], [Models; 2]) {
    let cores = [core(LAB_A), core(LAB_B)];
    let models = [Models::new(cx), Models::new(cx)];
    let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
    let first: Arc<dyn Core> = cores[0].clone();
    let second: Arc<dyn Core> = cores[1].clone();
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: id,
                app_name: format!("{TITLE} {}", std::process::id()).into(),
                title: TITLE.into(),
                shell: ShellParts {
                    core: first,
                    models: models[0].clone(),
                    settings,
                    trace: Arc::new(ViewTrace::new(std::io::sink())),
                },
                account_name: "Synthetic A".into(),
                account_detail: "a@example.test".into(),
                can_add: true,
                ringer: None,
                pictures_dir: None,
            },
            cx,
        )
    });
    desktop.update(cx, |desktop, cx| {
        desktop.add_account(
            AccountParts {
                core: second,
                models: models[1].clone(),
                name: "Synthetic B".into(),
                detail: "b@example.test".into(),
            },
            cx,
        );
    });
    (desktop, cores, models)
}

fn core(me: &str) -> Arc<FakeCore> {
    let core = Arc::new(FakeCore {
        me: me.to_owned().into(),
        ..FakeCore::default()
    });
    core.conversations.lock().unwrap().push(Conversation {
        id: CHAT.into(),
        kind: ConversationKind::Group,
        topic: Some("Synthetic planning".into()),
        version: 1,
        last_activity: Some(1_000),
    });
    core.members
        .lock()
        .unwrap()
        .insert(CHAT.into(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    core.unread.lock().unwrap().insert(CHAT.into(), 0);
    core
}

async fn exercise(desktop: &Entity<Desktop>, cores: &[Arc<FakeCore>; 2], models: &[Models; 2], cx: &mut AsyncApp) {
    desktop.update(cx, Desktop::show);
    pause(cx).await;
    assert_eq!(
        desktop.read_with(cx, |desktop, cx| desktop.ui_state(cx).window),
        WindowShown::Shown
    );
    checkpoint("plain", cx).await;
    reactivate(desktop, cx).await;
    change_unread(cores, models, 3, cx);
    pause(cx).await;
    assert_eq!(
        desktop.read_with(cx, |desktop, cx| desktop.tray().read(cx).combined(cx)),
        Some(2)
    );
    checkpoint("unread", cx).await;
    account_settings(desktop, cx).await;
    reopen(desktop, cx).await;
    read_chat(desktop, cores, models, cx).await;
}
async fn account_settings(desktop: &Entity<Desktop>, cx: &mut AsyncApp) {
    desktop.update(cx, |desktop, cx| {
        desktop.open_settings_of(LAB_B, Some(Page::Notifications), cx);
    });
    pause(cx).await;
    assert_state(desktop, LAB_B, Main::Settings, cx);
    checkpoint("settings", cx).await;
    desktop.update(cx, |desktop, cx| {
        desktop.settings().update(cx, |settings, cx| {
            settings.change(
                |settings| {
                    settings.appearance.follow_system = false;
                    settings.appearance.theme = teams_ui::theme::DARK_KEY;
                },
                cx,
            );
        });
    });
    pause(cx).await;
    checkpoint("dark", cx).await;
}
async fn reopen(desktop: &Entity<Desktop>, cx: &mut AsyncApp) {
    let window = desktop.read_with(cx, |desktop, _| desktop.window().unwrap());
    // The runner closes the real HWND and uses the real tray's Open command to recreate it.
    checkpoint("close", cx).await;
    assert_eq!(
        desktop.read_with(cx, |desktop, cx| desktop.ui_state(cx).window),
        WindowShown::Tray
    );
    checkpoint("open", cx).await;
    pause(cx).await;
    let reopened = desktop.read_with(cx, |desktop, _| desktop.window().unwrap());
    assert_ne!(
        window.window_id(),
        reopened.window_id(),
        "tray recreated the real GPUI window"
    );
    assert_state(desktop, LAB_B, Main::Nothing, cx);
    checkpoint("reopened", cx).await;
}
async fn read_chat(desktop: &Entity<Desktop>, cores: &[Arc<FakeCore>; 2], models: &[Models; 2], cx: &mut AsyncApp) {
    desktop.update(cx, |desktop, cx| desktop.open_chat_of(LAB_A, CHAT.into(), cx));
    pause(cx).await;
    assert_state(desktop, LAB_A, Main::Chat(CHAT.into()), cx);
    checkpoint("chat", cx).await;
    change_unread(cores, models, 0, cx);
    pause(cx).await;
    assert_eq!(
        desktop.read_with(cx, |desktop, cx| desktop.tray().read(cx).combined(cx)),
        Some(0)
    );
    checkpoint("read", cx).await;
}

async fn reactivate(desktop: &Entity<Desktop>, cx: &mut AsyncApp) {
    let first = desktop.read_with(cx, |desktop, _| desktop.window().unwrap());
    checkpoint("minimize", cx).await;
    desktop.update(cx, Desktop::show);
    pause(cx).await;
    assert_eq!(
        desktop
            .read_with(cx, |desktop, _| desktop.window().unwrap())
            .window_id(),
        first.window_id()
    );
    checkpoint("active", cx).await;
}
fn change_unread(cores: &[Arc<FakeCore>; 2], models: &[Models; 2], count: u64, cx: &mut AsyncApp) {
    for (core, models) in cores.iter().zip(models) {
        core.unread.lock().unwrap().insert(CHAT.into(), count);
        cx.update(|cx| {
            models.apply(
                Event::ReadHorizon {
                    conversation: CHAT.into(),
                },
                cx,
            );
        });
    }
}

fn assert_state(desktop: &Entity<Desktop>, account: &str, main: Main, cx: &AsyncApp) {
    let state = desktop.read_with(cx, Desktop::ui_state);
    assert_eq!(state.account.as_deref(), Some(account));
    assert_eq!(state.main, Some(main));
}
async fn pause(cx: &AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(1200)).await;
}
async fn checkpoint(step: &str, cx: &AsyncApp) {
    let dir = PathBuf::from(std::env::var_os("UI_SANDBOX_DIR").unwrap());
    std::fs::write(dir.join("step"), step).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !dir.join(format!("{step}.done")).exists() {
        assert!(std::time::Instant::now() < deadline, "native shell checkpoint {step}");
        cx.background_executor().timer(Duration::from_millis(50)).await;
    }
}
