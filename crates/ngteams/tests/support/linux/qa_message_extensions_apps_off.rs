//! An own server without apps (`[apps]` and `[outgoing_webhooks]` off, so its authz names neither service), as its
//! user meets it in the app's window: no entry point of the apps shows. The rail has no Store: Workflows' "More apps"
//! (a service of its own) moves up to the Store's place, and the place under it stays empty. The compose box's "+"
//! opens its menu of the hidden tools, not the apps' flyout. A message's More actions lists no app. The checks run again after a restart of the app on the same
//! data. The chat header's shots show that it has no "Apps in this chat".

use std::path::Path;
use std::time::Duration;
use std::{fs, thread};

use teams_ctl::protocol::Request;
use ui_sandbox::input::keys;
use ui_sandbox::{Sandbox, SandboxResult};

use super::{
    AppRun, CA_ENV, CUSTOMIZE_FOOT, FILL, ORIGIN_ENV, OWN_B, OWN_UPN, PANE_X, PLUS, Rect, SETTLE, SURFACE, at,
    changed_box, expect, load, message_menu, near, note, own_server_binary, runs, say, step,
};

/// The rail's Store on the 1280 × 800 screen of a server with apps (`apps_window.rs`).
const RAIL_STORE: (u32, u32) = (37, 245);
/// Workflows' "More apps" under the Store on a server with apps: without the Store, nothing is there.
const RAIL_AFTER_STORE: (u32, u32) = (37, 300);
/// A place on the rail under its last item on each server, the rail's own background.
const RAIL_EMPTY: (u32, u32) = (37, 400);
/// The header's "Apps in this chat" of the chat with Erin Made-Up on a server with apps (the `own` run's shots).
const HEADER_APPS: (u32, u32) = (687, 71);
/// A place in the header right of its name and presence, the header's own background.
const HEADER_EMPTY: (u32, u32) = (900, 71);
/// Where the compose box takes a click, left of its tools.
const BOX: (u32, u32) = (700, 749);
const SENT: &str = "Made-up plan for Friday";

/// The run: the server, the first start, then a restart on the same data.
pub(super) fn run(sandbox: &Sandbox, dir: &Path) -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped the apps-off run: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let server = own_server_binary::BinaryServer::start_with(
        &binary,
        "\n[apps]\nenabled = false\n\n[outgoing_webhooks]\nenabled = false\n",
    )?;
    server.add_user(OWN_UPN, "Dana Made-Up")?;
    server.add_user(OWN_B, "Erin Made-Up")?;
    let env = vec![
        (ORIGIN_ENV, server.origin.clone()),
        (CA_ENV, server.ca_file.to_string_lossy().into_owned()),
    ];
    let outcome = first_start(sandbox, &env, dir).and_then(|chat| restart(sandbox, &env, dir, &chat));
    if outcome.is_err() {
        let _ = fs::write(dir.join("own-server.log"), server.log());
    }
    outcome
}

/// The first start: a chat with a message of the user's, then the checks; the chat's id.
fn first_start(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<String> {
    let mut run = AppRun::launch(sandbox, env, dir, "own")?;
    let outcome = (|| -> SandboxResult<String> {
        thread::sleep(SETTLE * 2);
        run.request(Request::NewChat {
            people: vec![OWN_B.to_owned()],
            wait: true,
        })?;
        let rows = run.until_in("list", "the chat made", |state| {
            state["rows"].as_array().is_some_and(|rows| !rows.is_empty())
        })?;
        let chat = rows["rows"][0]["id"].as_str().ok_or("no chat row")?.to_owned();
        run.open(&chat)?;
        thread::sleep(SETTLE);
        run.click(BOX.0, BOX.1)?;
        run.input.type_text(SENT)?;
        thread::sleep(Duration::from_millis(300));
        run.input.key(keys::ENTER)?;
        run.until_chat("the message sent", |state| {
            state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["own"] == true && item["text"] == SENT))
        })?;
        nothing_offered(&mut run, sandbox, &chat, "apps-off")?;
        Ok(chat)
    })();
    match outcome {
        Ok(chat) => run.quit(sandbox).map(|()| chat),
        Err(error) => Err(run.fail(sandbox, &*error)),
    }
}

/// The app started again on the same data: the same checks.
fn restart(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path, chat: &str) -> SandboxResult {
    step("apps-off: the app starts again");
    let mut run = AppRun::launch(sandbox, env, dir, "own")?;
    let outcome = (|| -> SandboxResult {
        thread::sleep(SETTLE * 2);
        run.open(chat)?;
        run.until_chat("the message after the restart", |state| {
            state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["own"] == true && item["text"] == SENT))
        })?;
        nothing_offered(&mut run, sandbox, chat, "apps-off-restart")
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// The rail, the "+" and the message menu offer nothing of the apps; the shots of `name` in Light and Dark.
fn nothing_offered(run: &mut AppRun, sandbox: &Sandbox, chat: &str, name: &str) -> SandboxResult {
    thread::sleep(SETTLE);
    let still = load(&run.shot(sandbox, &format!("qa-me-{name}-chat-light.png"))?)?;
    run.theme(true)?;
    thread::sleep(SETTLE);
    run.shot(sandbox, &format!("qa-me-{name}-chat-dark.png"))?;
    run.theme(false)?;
    thread::sleep(SETTLE);
    rail_has_no_store(&still, name);
    header_has_no_apps(&still, name);
    run.click(RAIL_STORE.0, RAIL_STORE.1)?;
    thread::sleep(SETTLE);
    let state = run.state_in("chat")?;
    expect(
        !state["main"].as_str().is_some_and(|main| main.contains("Apps")),
        || format!("{name}: a click where the rail's Store is opens {}", state["main"]),
    );
    run.shot(sandbox, &format!("qa-me-{name}-rail-click-light.png"))?;
    run.open(chat)?;
    thread::sleep(SETTLE);
    plus_opens_no_flyout(run, sandbox, name)?;
    more_actions_lists_no_app(run, sandbox, name)?;
    step(&format!("{name}: no entry point of the apps shows"));
    Ok(())
}

/// The rail shows nothing where a server with apps shows the item under its Store: the place has the rail's own
/// background.
fn rail_has_no_store(shot: &image::RgbImage, name: &str) {
    let background = at(shot, RAIL_EMPTY.0, RAIL_EMPTY.1);
    let (x, y) = RAIL_AFTER_STORE;
    let drawn = (y - 16..y + 16)
        .flat_map(|y| (x - 16..x + 16).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(shot, x, y), background, 8))
        .count();
    expect(drawn == 0, || {
        format!("{name}: the rail draws {drawn} px under its items, where a server without the Store has none")
    });
}

/// The chat header shows nothing where a server with apps shows "Apps in this chat".
fn header_has_no_apps(shot: &image::RgbImage, name: &str) {
    let background = at(shot, HEADER_EMPTY.0, HEADER_EMPTY.1);
    let (x, y) = HEADER_APPS;
    let drawn = (y - 10..y + 10)
        .flat_map(|y| (x - 10..x + 10).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(shot, x, y), background, 8))
        .count();
    expect(drawn == 0, || {
        format!("{name}: the chat header draws {drawn} px where a server with apps shows Apps in this chat")
    });
}

/// A click on the "+" opens the menu of the hidden tools, not the apps' flyout.
fn plus_opens_no_flyout(run: &mut AppRun, sandbox: &Sandbox, name: &str) -> SandboxResult {
    let before = load(&run.shot(sandbox, &format!("qa-me-{name}-plus-closed-light.png"))?)?;
    run.click(PLUS.0, PLUS.1)?;
    thread::sleep(SETTLE);
    let state = run.state_in("chat")?;
    expect(state["compose"]["flyout"].is_null(), || {
        format!("{name}: the + opens the apps' flyout: {}", state["compose"]["flyout"])
    });
    let after = load(&run.shot(sandbox, &format!("qa-me-{name}-plus-light.png"))?)?;
    let region = Rect {
        x0: 0,
        y0: 0,
        x1: after.width(),
        y1: PLUS.1.saturating_sub(16),
    };
    expect(changed_box(&before, &after, region).is_some(), || {
        format!("{name}: a click on the + opens no menu")
    });
    run.theme(true)?;
    thread::sleep(SETTLE);
    run.shot(sandbox, &format!("qa-me-{name}-plus-dark.png"))?;
    run.theme(false)?;
    thread::sleep(SETTLE);
    run.input.key(keys::ESC)?;
    thread::sleep(SETTLE);
    Ok(())
}

/// The More actions of the user's own message lists no app.
fn more_actions_lists_no_app(run: &mut AppRun, sandbox: &Sandbox, name: &str) -> SandboxResult {
    let shot = load(&run.shot(sandbox, &format!("qa-me-{name}-message-light.png"))?)?;
    let Some(bubble) = own_bubble(&shot) else {
        note(format!(
            "{name}: no bubble of the user's message to open More options on"
        ));
        return Ok(());
    };
    let Some(menu) = message_menu(run, sandbox, bubble, name, || {})? else {
        note(format!("{name}: no More options menu on the user's message"));
        return Ok(());
    };
    run.click(menu.x0 + 60, menu.y1.saturating_sub(18 + CUSTOMIZE_FOOT))?;
    thread::sleep(SETTLE);
    let state = run.state_in("chat")?;
    expect(state["actions"].as_array().is_some_and(Vec::is_empty), || {
        format!("{name}: More actions lists {}", state["actions"])
    });
    run.shot(sandbox, &format!("qa-me-{name}-more-light.png"))?;
    run.theme(true)?;
    thread::sleep(SETTLE);
    run.shot(sandbox, &format!("qa-me-{name}-more-dark.png"))?;
    run.theme(false)?;
    thread::sleep(SETTLE);
    run.input.key(keys::ESC)?;
    run.input.key(keys::ESC)?;
    thread::sleep(SETTLE);
    Ok(())
}

/// The lowest bubble in the chat's messages that is not the page's white: the user's own, on the right.
fn own_bubble(shot: &image::RgbImage) -> Option<Rect> {
    let fill =
        |pixel: [u8; 3]| pixel != SURFACE && pixel != FILL && pixel[2] > 230 && pixel[0] > 200 && pixel[2] > pixel[0];
    (100..PLUS.1.saturating_sub(40)).rev().find_map(|y| {
        runs(shot, y, PANE_X, shot.width(), fill)
            .into_iter()
            .find(|(x0, x1)| (100..=700).contains(&(x1 - x0)))
            .map(|(x0, x1)| Rect {
                x0,
                y0: y.saturating_sub(24),
                x1,
                y1: y + 1,
            })
    })
}
