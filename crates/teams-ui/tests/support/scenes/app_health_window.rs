//! Manage apps with each bot's delivery health, rendered in the private UI sandbox over a made-up
//! server in Teams Light and Dark: the list with a failing and a paused bot marked, and the details of a failing, a
//! paused, a healthy and a new bot, each after a test activity (taken, refused by the address guard, not answered).
//! Screenshots go to `target/ui-shots/app-health-*.png`. Never reaches a tenant or a server.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, WindowOptions,
    div, px, rgb,
};
use teams_auth::admin::{
    AdminApp, AdminBot, AppChanges, AppSource, BotAuthMode, BotHealth, HealthState, MadeApp, TestActivity, UserPage,
};
use teams_ui::app_admin::{AppAdmin, AppProblem, BotConnection, BotForm, DialogKind, ManageApps};
use teams_ui::{
    ViewTrace,
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
};
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "this scene does not request fixture diagnostics")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_APP_HEALTH_WINDOW";
const PAGES: [&str; 5] = ["list", "failing", "paused", "healthy", "new"];

pub(super) fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |mode| child(&mode));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("app health window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn shots() -> io::Result<PathBuf> {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .unwrap_or(Path::new("target"))
        .join("ui-shots");
    fs::create_dir_all(&p)?;
    Ok(p)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("app health")? else {
        return Ok(());
    };
    for (theme, label) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in PAGES {
            shot(&sandbox, theme, label, page)?;
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, label: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}|{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, mode.as_str())])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no page ready: {error:?}")))?;
    let name = format!("app-health-{label}-{page}.png");
    let screenshot = sandbox.screenshot(&name, true)?;
    fs::copy(screenshot, shots()?.join(&name))?;
    say(&format!("shot {name}"));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

fn health(state: HealthState) -> BotHealth {
    let now = now_ms();
    match state {
        HealthState::Failing => BotHealth {
            state,
            failures: 3,
            paused_until: None,
            last_error: Some("the bot answered 500".to_owned()),
            last_success: Some(now - 3 * 3_600_000),
            pending: 1,
            sent: 12,
            failed: 2,
        },
        HealthState::Paused => BotHealth {
            state,
            failures: 0,
            paused_until: Some(now + 50_000),
            last_error: Some("the request timed out".to_owned()),
            last_success: None,
            pending: 6,
            sent: 0,
            failed: 20,
        },
        HealthState::Healthy => BotHealth {
            state,
            failures: 0,
            paused_until: None,
            last_error: None,
            last_success: Some(now - 4 * 60_000),
            pending: 0,
            sent: 31,
            failed: 0,
        },
        _ => BotHealth {
            state,
            failures: 0,
            paused_until: None,
            last_error: None,
            last_success: None,
            pending: 0,
            sent: 0,
            failed: 0,
        },
    }
}

fn app(at: u8, name: &str, accent: &str, state: HealthState) -> AdminApp {
    let id = format!("00000000-0000-4000-8000-0000000000{at:02x}");
    AdminApp {
        version: "1.0.0".to_owned(),
        name: name.to_owned(),
        short_description: "A made-up bot for the screenshots".to_owned(),
        developer: "Made-up Maker".to_owned(),
        accent_color: Some(accent.to_owned()),
        source: AppSource::Form,
        enabled: true,
        everyone: true,
        users: Vec::new(),
        bot: Some(AdminBot {
            id: id.clone(),
            mri: format!("28:{id}"),
            endpoint: Some(format!(
                "https://bots.example.test/{}/api/messages",
                name.to_lowercase().replace(' ', "-")
            )),
            auth_mode: BotAuthMode::Signed,
            scopes: vec!["personal".to_owned()],
        }),
        health: Some(health(state)),
        created_at: 1,
        updated_at: 2,
        id,
    }
}

fn apps() -> Vec<AdminApp> {
    vec![
        app(0xb1, "Lunch Orders", "#4f52b2", HealthState::Failing),
        app(0xb2, "Night Reports", "#c4314b", HealthState::Paused),
        app(0xb3, "Standup Helper", "#237b4b", HealthState::Healthy),
        app(0xb4, "Fresh Bot", "#8f6200", HealthState::New),
    ]
}

/// A made-up server: the apps above, and how each bot answers a test activity.
struct Server {
    apps: Vec<AdminApp>,
    test: Mutex<Option<Result<TestActivity, AppProblem>>>,
}

impl AppAdmin for Server {
    fn apps(&self) -> Result<Vec<AdminApp>, AppProblem> {
        Ok(self.apps.clone())
    }

    fn register(&self, _: &BotForm) -> Result<MadeApp, AppProblem> {
        Err(AppProblem::NotFound)
    }

    fn upload(&self, _: Vec<u8>) -> Result<MadeApp, AppProblem> {
        Err(AppProblem::NotFound)
    }

    fn change(&self, _: &str, _: &AppChanges) -> Result<AdminApp, AppProblem> {
        Err(AppProblem::NotFound)
    }

    fn new_secret(&self, _: &str) -> Result<String, AppProblem> {
        Err(AppProblem::NotFound)
    }

    fn delete(&self, _: &str) -> Result<(), AppProblem> {
        Err(AppProblem::NotFound)
    }

    fn people(&self, _: &str, _: Option<&str>) -> Result<UserPage, AppProblem> {
        Ok(UserPage {
            users: Vec::new(),
            next: None,
        })
    }

    fn connection(&self) -> Result<BotConnection, AppProblem> {
        Ok(BotConnection {
            origin: "https://teams.example.test".to_owned(),
            issuer: "https://teams.example.test/bot".to_owned(),
        })
    }

    fn test_activity(&self, _: &str) -> Result<TestActivity, AppProblem> {
        self.test
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or(Err(AppProblem::NotFound))
    }
}

/// How the page's bot answers its test activity: none for the list and the new bot.
fn test_of(page: &str) -> Option<Result<TestActivity, AppProblem>> {
    match page {
        "failing" => Some(Ok(TestActivity {
            delivered: false,
            status: Some(500),
            error: Some("the bot answered 500".to_owned()),
            elapsed_ms: 84,
        })),
        "paused" => Some(Ok(TestActivity {
            delivered: false,
            status: None,
            error: Some("the address is on a network the server doesn't reach".to_owned()),
            elapsed_ms: 2,
        })),
        "healthy" => Some(Ok(TestActivity {
            delivered: true,
            status: Some(200),
            error: None,
            elapsed_ms: 37,
        })),
        _ => None,
    }
}

/// The page over the theme's background, and its dialog over it as the settings draw it.
struct Stage(Entity<ManageApps>);

impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = self.0.read(cx).dialog().cloned().map(|dialog| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0080))
                .child(dialog)
        });
        div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(theme::current(cx).background))
            .child(div().w(px(900.0)).h(px(860.0)).child(self.0.clone()))
            .children(dialog)
    }
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "list"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        teams_ui::window::init(cx);
        theme::set_appearance(
            &Appearance {
                theme,
                follow_system: false,
                ..Appearance::default()
            },
            cx,
        );
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let server: Arc<dyn AppAdmin> = Arc::new(Server {
            apps: apps(),
            test: Mutex::new(test_of(&page)),
        });
        let options = WindowOptions {
            window_bounds: Some(teams_ui::window::initial_bounds(gpui::size(px(1000.0), px(900.0)), cx)),
            #[cfg(windows)]
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC APP HEALTH".into()),
                ..Default::default()
            }),
            app_id: Some("ngteams-app-health-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let manage: Entity<ManageApps> =
                cx.new(|cx| ManageApps::new(server, Arc::new(ViewTrace::off()), window, cx));
            manage.update(cx, ManageApps::show);
            step(&page, manage.clone(), window);
            let stage = cx.new(|cx| {
                cx.observe(&manage, |_, _, cx| cx.notify()).detach();
                Stage(manage)
            });
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}

/// The page's step, taken once the list settled, then "ready".
fn step(page: &str, manage: Entity<ManageApps>, window: &mut Window) {
    let page = page.to_owned();
    after(window, 15, move |window, cx| {
        let at = match page.as_str() {
            "failing" => Some(0),
            "paused" => Some(1),
            "healthy" => Some(2),
            "new" => Some(3),
            _ => None,
        };
        if let Some(at) = at {
            let app = apps().into_iter().nth(at);
            manage.update(cx, |manage, cx| manage.open(DialogKind::Details, app, window, cx));
            if test_of(&page).is_some()
                && let Some(dialog) = manage.read(cx).dialog().cloned()
            {
                dialog.update(cx, teams_ui::app_admin::AppDialog::send_test);
            }
        }
        after(window, 20, |_, _| say("ready"));
    });
}

fn after(window: &mut Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after(window, frames, done);
            window.refresh();
            return;
        }
        if frames == 0 {
            done(window, cx);
        } else {
            after(window, frames - 1, done);
            window.refresh();
        }
    });
    window.refresh();
}
