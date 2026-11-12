//! The UI commands: what the window shows, changed through the desktop and its shells as the window's own controls
//! change it, and any registered `teams_ui::` action dispatched to the window by its name.
//!
//! - `window show|focus`: the window brought to the front, opened again if it was closed (as the tray's "Open").
//!   `hide` closes it into the tray, refused ([`ErrorCode::NoTray`]) without a tray item, since closing would quit the
//!   app; `toggle` hides a window shown and shows one that isn't. `activate` (a second launch's hand-off) shows it.
//! - `open chat|channel|calendar|activity|settings|search`: the account addressed shown (`--account`, else the one
//!   shown), the window first, and the view opened as its list, its rail or its search box opens it. A chat is named
//!   as the reads name it ([`address::chat`]); a channel by its id, its name or `<team>/<channel>`, in any case, from
//!   the account's teams; a settings page by its key or title.
//! - `account switch <account>`: that account shown in the window, signed out or not.
//! - `ui state`: what the window shows ([`watch::ui_state`], the `ui` event's).
//! - `action list`: the registered `teams_ui::` actions; `action <name>`: one of them, by its full name or its short
//!   one (any case, when only one action has it), dispatched to the window's focus, the window drawn first so a focus
//!   just moved (an account switched) is where it lands. GPUI runs it later, so the answer says only that it was
//!   dispatched; [`ErrorCode::NoWindow`] while the window is closed.
//!
//! Each command but `ui state` and the actions answers with what the window shows once it is done.

use std::collections::HashMap;

use gpui::{AnyWindowHandle, App, AsyncApp, Entity, WeakEntity};
use teams_ctl::protocol::{ChatRef, CtlError, ErrorCode, OpenTarget, Request, Response, UiState, WindowOp};
use teams_ui::settings_view::Page;
use teams_ui::{Core, Desktop};

use super::address::{self, engine_error};
use super::dispatch::alive;
use super::watch;
use crate::app::Running;

/// The namespace of the app's actions `action` dispatches.
pub const NAMESPACE: &str = "teams_ui::";

/// Whether `request` is a UI command (see the module docs).
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(
        request,
        Request::Window { .. }
            | Request::Open { .. }
            | Request::SwitchAccount { .. }
            | Request::UiState
            | Request::Action { .. }
            | Request::ActionList
            | Request::Activate
    )
}

/// `request`, a UI command, answered for `account` (see the module docs).
///
/// # Errors
/// The account, chat, channel, page or action named isn't one, the window can't be hidden or is closed, or the app is
/// quitting.
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let answered = match request {
        Request::Open {
            target: OpenTarget::Chat { chat },
        } => {
            open_chat(running, account, chat, cx).await?;
            None
        }
        Request::Open {
            target: OpenTarget::Channel { channel },
        } => {
            open_channel(running, account, channel, cx).await?;
            None
        }
        _ => cx.update(|cx| at_once(running, account, request, cx))?,
    };
    match answered {
        Some(response) => Ok(response),
        // read once the change's effects ran (a window closed is told then)
        None => cx.update(|cx| Ok(Response::UiState(ui_state(alive(running, cx)?.read(cx), cx)))),
    }
}

/// What the window shows, as `ui state` tells it.
#[must_use]
pub fn ui_state(running: &Running, cx: &App) -> UiState {
    let upns: HashMap<String, String> = running
        .bridges()
        .iter()
        .map(|(mri, upn, _)| (mri.clone(), upn.clone()))
        .collect();
    watch::ui_state(&running.desktop().read(cx).ui_state(cx), &upns)
}

/// A command done on the foreground at once: its answer, or `None` to answer with what the window shows.
fn at_once(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut App,
) -> Result<Option<Response>, CtlError> {
    let running = alive(running, cx)?;
    let desktop = running.read(cx).desktop().clone();
    match request {
        Request::Window { op } => window(&desktop, *op, cx)?,
        Request::Activate => window(&desktop, WindowOp::Show, cx)?,
        Request::Open { target } => {
            let mri = address::known(running.read(cx), account, cx)?.0.clone();
            open(&desktop, &mri, target, cx)?;
        }
        Request::SwitchAccount { to } => {
            let mri = address::known(running.read(cx), Some(to), cx)?.0.clone();
            desktop.update(cx, |desktop, cx| desktop.switch(&mri, cx));
        }
        Request::UiState => return Ok(Some(Response::UiState(ui_state(running.read(cx), cx)))),
        Request::ActionList => return Ok(Some(Response::Actions { actions: actions(cx) })),
        Request::Action { name } => return action(&desktop, name, cx).map(Some),
        _ => return Err(CtlError::new(ErrorCode::BadRequest, "not a UI command")),
    }
    Ok(None)
}

/// `op` done to the main window (see the module docs).
fn window(desktop: &Entity<Desktop>, op: WindowOp, cx: &mut App) -> Result<(), CtlError> {
    match (op, desktop.read(cx).window()) {
        (WindowOp::Hide | WindowOp::Toggle, Some(window)) => {
            if !desktop.read(cx).in_tray() {
                return Err(CtlError::new(
                    ErrorCode::NoTray,
                    "no tray item to keep the app: closing the window would quit it",
                ));
            }
            // closed outside the desktop's update: its close watch updates it
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        (WindowOp::Hide, None) => {}
        (WindowOp::Show | WindowOp::Focus | WindowOp::Toggle, _) => desktop.update(cx, Desktop::show),
    }
    Ok(())
}

/// The calendar, the Activity pane, the settings or the search's results of account `mri` opened in the window.
fn open(desktop: &Entity<Desktop>, mri: &str, target: &OpenTarget, cx: &mut App) -> Result<(), CtlError> {
    match target {
        OpenTarget::Calendar => desktop.update(cx, |desktop, cx| desktop.show_calendar_of(mri, cx)),
        OpenTarget::Activity => desktop.update(cx, |desktop, cx| desktop.show_activity_of(mri, cx)),
        OpenTarget::Settings { page } => {
            let page = page.as_deref().map(settings_page).transpose()?;
            desktop.update(cx, |desktop, cx| desktop.open_settings_of(mri, page, cx));
        }
        OpenTarget::Search { query } => {
            if query.trim().is_empty() {
                return Err(CtlError::new(ErrorCode::BadRequest, "nothing to search for"));
            }
            desktop.update(cx, |desktop, cx| desktop.search_of(mri, query, cx));
        }
        OpenTarget::Chat { .. } | OpenTarget::Channel { .. } => {
            return Err(CtlError::new(
                ErrorCode::BadRequest,
                "a chat or channel is looked up first",
            ));
        }
    }
    Ok(())
}

/// The settings page `name` names: its key or its title, in any case.
fn settings_page(name: &str) -> Result<Page, CtlError> {
    let name = name.trim();
    Page::ALL
        .into_iter()
        .find(|page| page.key().eq_ignore_ascii_case(name) || page.title().eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let pages: Vec<&str> = Page::ALL.iter().map(|page| page.key()).collect();
            CtlError::new(
                ErrorCode::BadRequest,
                format!("no settings page is {name}: one of {}", pages.join(", ")),
            )
        })
}

async fn open_chat(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    chat: &str,
    cx: &mut AsyncApp,
) -> Result<(), CtlError> {
    let addressed = cx.update(|cx| address::account(alive(running, cx)?.read(cx), account, cx))?;
    let found = address::chat(&*addressed.core, chat).await?;
    cx.update(|cx| {
        let desktop = alive(running, cx)?.read(cx).desktop().clone();
        desktop.update(cx, |desktop, cx| desktop.open_chat_of(&addressed.mri, found.id, cx));
        Ok(())
    })
}

async fn open_channel(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    channel: &str,
    cx: &mut AsyncApp,
) -> Result<(), CtlError> {
    let addressed = cx.update(|cx| address::account(alive(running, cx)?.read(cx), account, cx))?;
    let id = channel_id(&*addressed.core, channel).await?;
    cx.update(|cx| {
        let desktop = alive(running, cx)?.read(cx).desktop().clone();
        desktop.update(cx, |desktop, cx| desktop.open_channel_of(&addressed.mri, id, cx));
        Ok(())
    })
}

/// The id of the channel `wanted` names among the account's teams: its id, its name or `<team>/<channel>`, in any
/// case. Before the teams were ever read, a thread's id (`19:…`) is taken as it is.
///
/// # Errors
/// [`ErrorCode::UnknownChat`], [`ErrorCode::AmbiguousChat`] with the candidates, or the engine's.
pub async fn channel_id(core: &dyn Core, wanted: &str) -> Result<String, CtlError> {
    let wanted = wanted.trim();
    let Some(teams) = core.teams().await.map_err(|error| engine_error(&error))? else {
        if wanted.starts_with("19:") {
            return Ok(wanted.to_owned());
        }
        return Err(CtlError::new(
            ErrorCode::UnknownChat,
            format!("the teams aren't read yet: name the channel {wanted} by its id"),
        ));
    };
    let channels = teams
        .iter()
        .flat_map(|team| team.channels.iter().map(move |channel| (team, channel)));
    let mut found: Vec<ChatRef> = Vec::new();
    for (team, channel) in channels {
        if channel.id == wanted {
            return Ok(channel.id.clone());
        }
        let title = format!("{}/{}", team.name, channel.name);
        if channel.name.eq_ignore_ascii_case(wanted) || title.eq_ignore_ascii_case(wanted) {
            found.push(ChatRef {
                id: channel.id.clone(),
                title,
            });
        }
    }
    match found.len() {
        0 => Err(CtlError::new(
            ErrorCode::UnknownChat,
            format!("no channel is {wanted}: name it by its id, its name or <team>/<channel>"),
        )),
        1 => Ok(found.swap_remove(0).id),
        many => Err(CtlError {
            code: ErrorCode::AmbiguousChat,
            message: format!("{many} channels are called {wanted}: name one by <team>/<channel> or its id"),
            candidates: found,
        }),
    }
}

/// The registered `teams_ui::` actions, sorted.
#[must_use]
pub fn actions(cx: &App) -> Vec<String> {
    let mut names: Vec<String> = cx
        .all_action_names()
        .iter()
        .filter(|name| name.starts_with(NAMESPACE))
        .map(|name| (*name).to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The action of `names` that `wanted` names: its full name, else its short one when only one has it, in any case.
///
/// # Errors
/// [`ErrorCode::UnknownAction`]: none has it, or several share the short name.
pub fn action_named(names: &[String], wanted: &str) -> Result<String, CtlError> {
    let wanted = wanted.trim();
    if let Some(full) = names.iter().find(|name| name.eq_ignore_ascii_case(wanted)) {
        return Ok(full.clone());
    }
    let short = wanted.rsplit("::").next().unwrap_or(wanted);
    let found: Vec<&String> = names
        .iter()
        .filter(|name| {
            name.rsplit("::")
                .next()
                .is_some_and(|its| its.eq_ignore_ascii_case(short))
        })
        .collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CtlError::new(
            ErrorCode::UnknownAction,
            format!("no action is {wanted}: `ngteamsctl action list` lists them"),
        )),
        many => {
            let many: Vec<&str> = many.iter().map(|name| name.as_str()).collect();
            Err(CtlError::new(
                ErrorCode::UnknownAction,
                format!(
                    "{wanted} names several actions ({}): give the full name",
                    many.join(", ")
                ),
            ))
        }
    }
}

/// Action `name` dispatched to the main window's focus.
fn action(desktop: &Entity<Desktop>, name: &str, cx: &mut App) -> Result<Response, CtlError> {
    let name = action_named(&actions(cx), name)?;
    let no_window = || {
        CtlError::new(
            ErrorCode::NoWindow,
            "the window is closed: `ngteamsctl window show` opens it",
        )
    };
    let window = desktop.read(cx).window().ok_or_else(no_window)?;
    let action = cx.build_action(&name, None).map_err(|error| {
        CtlError::new(
            ErrorCode::BadRequest,
            format!("{name} takes data a name alone can't give: {error}"),
        )
    })?;
    // updated as any window, its root view not taken: the draw renders it
    AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            // GPUI finds the focus's handlers in the frame last drawn, and a frame is drawn only when the platform asks
            // for one: a focus moved since (an account switched a request before) would fall back to the root, which
            // handles no app action. Drawn now, as GPUI's own `open_window` does, the frame holds the focus.
            window.draw(cx).clear(cx);
            window.dispatch_action(action, cx);
        })
        .map_err(|_| no_window())?;
    Ok(Response::Dispatched { action: name })
}
