//! The child's side of the bots' acceptance test: what its app's window holds now, and the steps a test can't reach
//! with the pointer or the keyboard at a fixed place, done as the control of that name does (a package chosen in the
//! file dialog, the store's buttons drawn where the content puts them, an action command's tab). The parent writes
//! `<seq> <verb> <arg>` into [`ASK`] in the sandbox dir; the child says `bots-answer <seq> <json>`.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, sync::Arc};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle};
use serde_json::{Value, json};
use teams_proto::{Mri, NewTeam, StoreApp, TeamRole, TeamWrite};
use teams_ui::Desktop;
use teams_ui::app_admin::{AppDialog as AdminDialog, AppDialogEvent, DialogKind, ManageApps, shown_secret};
use teams_ui::app_dialog::AppDialog;
use teams_ui::apps_page::AppsPage;
use teams_ui::apps_page::details::AppDetailsDialog;
use teams_ui::apps_page::place::AddToPlaceDialog;
use teams_ui::bridge::Core;
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;

use super::instance::say;

/// The file in the sandbox dir the parent writes a question into.
pub const ASK: &str = "bots-ask";

/// Answers the parent's questions about the app on `desktop`, as long as it runs.
pub fn hook(desktop: Entity<Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let mut words = question.trim_end().splitn(3, ' ');
            let (seq, verb, arg) = (
                words.next().unwrap_or_default().to_owned(),
                words.next().unwrap_or_default().to_owned(),
                words.next().unwrap_or_default().to_owned(),
            );
            let reply = if verb == "make-team" {
                make_team(&desktop, &arg, cx).await
            } else {
                on_window(&desktop, &verb, &arg, cx)
            };
            say(&format!("bots-answer {seq} {reply}"));
        }
    })
    .detach();
}

fn on_window(desktop: &Entity<Desktop>, verb: &str, arg: &str, cx: &mut AsyncApp) -> Value {
    let Some(window) = cx.update(|cx| desktop.read(cx).window()) else {
        return json!({"error": "no window"});
    };
    let Some(shell) = shell_of(window, cx) else {
        return json!({"error": "no shell"});
    };
    window
        .update(cx, |_, window, cx| match verb {
            "state" => state_of(&shell, cx),
            "secret" => write_secret(&shell, Path::new(arg), cx),
            "do" => {
                let (what, rest) = arg.split_once(' ').unwrap_or((arg, ""));
                act(what, rest, &shell, window, cx)
                    .map_or_else(|error| json!({"error": error}), |()| json!({"done": what}))
            }
            other => json!({"error": format!("unknown question {other}")}),
        })
        .unwrap_or_else(|_| json!({"error": "no window"}))
}

fn shell_of(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// A team named `<name>` made by the account, with the people `<mri>,<mri>` in it: `{"team": <id>}`.
async fn make_team(desktop: &Entity<Desktop>, arg: &str, cx: &mut AsyncApp) -> Value {
    let core: Option<Arc<dyn Core>> = cx.update(|cx| {
        let accounts = desktop.read(cx).accounts().read(cx);
        accounts.tiles().first().map(|tile| Arc::clone(&tile.core))
    });
    let Some(core) = core else {
        return json!({"error": "no account"});
    };
    let (name, people) = arg.split_once('|').unwrap_or((arg, ""));
    let create = TeamWrite::Create(NewTeam {
        name: name.into(),
        description: "A made-up team for the bots' acceptance".into(),
        first_channel: "General".into(),
        public: false,
    });
    let made = match core.team_write(create, None).await {
        Ok(made) => made,
        Err(error) => return json!({"error": error.to_string()}),
    };
    let Some((team, Some(group))) = made.created else {
        return json!({"error": "no team made"});
    };
    let members = people
        .split(',')
        .filter(|mri| !mri.is_empty())
        .map(|mri| (Mri::new(mri), TeamRole::Member))
        .collect();
    let add = TeamWrite::AddMembers {
        team: team.clone(),
        group,
        members,
    };
    match core.team_write(add, None).await {
        Ok(_) => json!({"team": team}),
        Err(error) => json!({"error": error.to_string()}),
    }
}

// --- what the window holds ---

fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let read = shell.read(cx);
    let main = format!("{:?}", read.main());
    let chat = read.chat().map_or(Value::Null, |view| view_state(view.read(cx), cx));
    let channel = read
        .channel()
        .map_or(Value::Null, |channel| view_state(channel.read(cx).posts().read(cx), cx));
    json!({
        "main": main.split('(').next().unwrap_or_default(),
        "toast": read.toast().map(ToString::to_string),
        "dialog": dialog_state(read, cx),
        "chat": chat,
        "channel": channel,
        "store": read.apps().map_or(Value::Null, |page| store_state(page, cx)),
        "admin": admin_state(shell, cx),
    })
}

/// A chat's (or a channel's posts') rows, its box (its words, its apps' cards, the flyout, the mention picker).
fn view_state(view: &ChatView, cx: &App) -> Value {
    let compose = view.compose().read(cx);
    let flyout = compose.apps_flyout().map(|flyout| {
        let flyout = flyout.read(cx);
        let listed: Vec<String> = flyout.listed(cx).iter().map(|app| app.name().to_owned()).collect();
        json!({
            "loaded": flyout.loaded(),
            "listed": listed,
            "pane": flyout.pane().map(|(name, tabs, found)| json!([name, tabs, found])),
        })
    });
    let cards: Vec<Value> = compose
        .app_cards()
        .iter()
        .map(|card| json!({"app": card.app_name, "content": card.content}))
        .collect();
    let picker = compose.picker().map(|picker| {
        let names: Vec<&str> = picker.matches.iter().map(|candidate| candidate.name.as_ref()).collect();
        json!({"names": names, "loading": picker.loading})
    });
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({
        "conversation": view.conversation(),
        "items": items,
        "compose": {"value": compose.value(cx).to_string(), "cards": cards, "flyout": flyout, "picker": picker},
    })
}

fn item_of(item: &MessageItem) -> Value {
    let (kind, text) = match &item.body {
        Body::Text(rendered) => (
            "text",
            rendered
                .0
                .blocks
                .iter()
                .flat_map(Block::plain_lines)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Body::Cards(_) => ("cards", String::new()),
        _ => ("other", String::new()),
    };
    let app_cards = item.app_cards.as_ref().map_or(0, |cards| cards.0.cards.len());
    json!({
        "id": item.id, "own": item.own, "kind": kind, "text": text, "sender": item.sender,
        "app_cards": app_cards,
    })
}

fn dialog_state(shell: &Shell, cx: &App) -> Value {
    let Some(dialog) = shell.team_dialog() else {
        return Value::Null;
    };
    if let Ok(app) = dialog.clone().downcast::<AppDialog>() {
        let app = app.read(cx);
        return json!({"kind": "app", "stage": app.stage_name(), "error": app.error()});
    }
    if let Ok(details) = dialog.clone().downcast::<AppDetailsDialog>() {
        return json!({"kind": "details", "added": details.read(cx).added()});
    }
    if let Ok(place) = dialog.clone().downcast::<AddToPlaceDialog>() {
        let places: Vec<String> = place.read(cx).shown(cx).into_iter().map(|place| place.id).collect();
        return json!({"kind": "place", "places": places});
    }
    json!({"kind": "other"})
}

fn store_state(page: &Entity<AppsPage>, cx: &App) -> Value {
    let page = page.read(cx);
    let listed: Vec<String> = page
        .listing()
        .map(|listing| listing.apps.iter().map(|store| store.app.id.clone()).collect())
        .unwrap_or_default();
    let added: serde_json::Map<String, Value> = listed
        .iter()
        .map(|id| (id.clone(), Value::Bool(page.added(id))))
        .collect();
    json!({"loaded": page.listing().is_some(), "apps": listed, "added": added})
}

fn manage_apps(shell: &Entity<Shell>, cx: &App) -> Option<Entity<ManageApps>> {
    shell.read(cx).settings_view().read(cx).apps().cloned()
}

fn admin_dialog(shell: &Entity<Shell>, cx: &App) -> Option<Entity<AdminDialog>> {
    manage_apps(shell, cx)?.read(cx).dialog().cloned()
}

/// Manage apps: its page, its apps (name, bot, endpoint) and its dialog (never the secret it shows).
fn admin_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let view = shell.read(cx).settings_view().read(cx);
    let Some(page) = manage_apps(shell, cx) else {
        return json!({"page": view.page().key()});
    };
    let page = page.read(cx);
    let apps: Vec<Value> = page
        .apps()
        .iter()
        .map(|app| {
            let bot = app.bot.as_ref();
            json!({"name": app.name, "id": app.id, "bot": bot.map(|bot| bot.id.clone()),
                   "endpoint": bot.and_then(|bot| bot.endpoint.clone())})
        })
        .collect();
    let dialog = page.dialog().map_or(Value::Null, |dialog| {
        let read = dialog.read(cx);
        json!({"kind": read.kind().key(), "problem": read.problem().map(ToString::to_string),
               "secret_shown": read.shows_secret()})
    });
    json!({"page": view.page().key(), "loaded": !page.loading(), "apps": apps, "dialog": dialog})
}

/// The secret Manage apps shows now, written to `path` (never said).
fn write_secret(shell: &Entity<Shell>, path: &Path, cx: &App) -> Value {
    let Some(dialog) = admin_dialog(shell, cx) else {
        return json!({"error": "no dialog"});
    };
    let Some(secret) = shown_secret(dialog.read(cx)).map(str::to_owned) else {
        return json!({"error": "no secret shown"});
    };
    match fs::write(path, secret) {
        Ok(()) => json!({"written": true}),
        Err(error) => json!({"error": error.to_string()}),
    }
}

// --- the steps done as their controls do them ---

fn store_app(shell: &Entity<Shell>, id: &str, cx: &App) -> Result<StoreApp, String> {
    let page = shell.read(cx).apps().ok_or("the store was never opened")?.read(cx);
    page.listing()
        .and_then(|listing| listing.app(id).cloned())
        .ok_or_else(|| format!("{id} is not in the store"))
}

fn dialog_of<T: 'static>(shell: &Entity<Shell>, cx: &App) -> Result<Entity<T>, String> {
    shell
        .read(cx)
        .team_dialog()
        .cloned()
        .and_then(|dialog| dialog.downcast::<T>().ok())
        .ok_or_else(|| format!("no {} open", std::any::type_name::<T>()))
}

fn act(what: &str, arg: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Result<(), String> {
    match what {
        "manage-apps" => shell.update(cx, |shell, cx| shell.open_settings_page(Page::Apps, window, cx)),
        "store" => shell.update(cx, |shell, cx| shell.open_apps(window, cx)),
        "details" => {
            let store = store_app(shell, arg, cx)?;
            shell.update(cx, |shell, cx| shell.open_app_details(store, window, cx));
        }
        "add" => dialog_of::<AppDetailsDialog>(shell, cx)?.update(cx, AppDetailsDialog::add),
        "open" => dialog_of::<AppDetailsDialog>(shell, cx)?.update(cx, AppDetailsDialog::open),
        "add-to" => {
            let team = arg == "team";
            dialog_of::<AppDetailsDialog>(shell, cx)?.update(cx, |details, cx| details.add_to(team, cx));
        }
        "choose" => {
            let place = arg.to_owned();
            dialog_of::<AddToPlaceDialog>(shell, cx)?.update(cx, |dialog, cx| dialog.choose(place, cx));
        }
        "go" => dialog_of::<AddToPlaceDialog>(shell, cx)?.update(cx, AddToPlaceDialog::go),
        "close" => {
            if let Ok(details) = dialog_of::<AppDetailsDialog>(shell, cx) {
                details.update(cx, AppDetailsDialog::close);
            } else {
                shell.update(cx, Shell::close_team_dialog);
            }
        }
        "command" => {
            let at: usize = arg.parse().map_err(|_| format!("no command {arg}"))?;
            let view = shell.read(cx).chat().cloned().ok_or("no chat open")?;
            let flyout = view
                .read(cx)
                .compose()
                .read(cx)
                .apps_flyout()
                .cloned()
                .ok_or("no flyout open")?;
            flyout.update(cx, |flyout, cx| flyout.choose_command(at, window, cx));
        }
        "dialog-press" => {
            let at: usize = arg.parse().map_err(|_| format!("no button {arg}"))?;
            dialog_of::<AppDialog>(shell, cx)?.update(cx, |dialog, cx| dialog.press(at, cx));
        }
        _ => admin_act(what, arg, shell, window, cx)?,
    }
    Ok(())
}

/// Manage apps' steps: an upload with the package at `arg`, its confirm, an app's details, its endpoint, a close.
fn admin_act(what: &str, arg: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Result<(), String> {
    let page = manage_apps(shell, cx).ok_or("no Manage apps page")?;
    match what {
        "upload" => page.update(cx, |page, cx| page.open(DialogKind::Upload, None, window, cx)),
        "admin-open" => {
            let app = page
                .read(cx)
                .apps()
                .iter()
                .find(|app| app.name == arg)
                .cloned()
                .ok_or_else(|| format!("no app {arg}"))?;
            page.update(cx, |page, cx| page.open(DialogKind::Details, Some(app), window, cx));
        }
        "admin-close" => {
            if let Some(dialog) = page.read(cx).dialog().cloned() {
                dialog.update(cx, |_, cx| cx.emit(AppDialogEvent::Closed));
            }
        }
        _ => {
            let dialog = page.read(cx).dialog().cloned().ok_or("no dialog")?;
            match what {
                "package" => {
                    let path = Path::new(arg);
                    let bytes = fs::read(path).map_err(|error| error.to_string())?;
                    let name = path
                        .file_name()
                        .map_or_else(|| "app.zip".to_owned(), |name| name.to_string_lossy().into_owned());
                    dialog.update(cx, |dialog, cx| dialog.choose_package(name, bytes, cx));
                }
                "confirm" => dialog.update(cx, AdminDialog::confirm),
                "endpoint" => dialog.update(cx, |dialog, cx| dialog.fill("", "", arg, window, cx)),
                _ => return Err(format!("unknown action {what}")),
            }
        }
    }
    Ok(())
}
