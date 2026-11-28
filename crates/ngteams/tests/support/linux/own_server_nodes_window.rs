//! ngteams on its own server as deployed, through the app's own windows, each app in a UI sandbox of its own
//! (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent deploys the real `ngteams-server` binary as `docs/server.md` ("Scaling") runs it: two API nodes and a
//! media node on one Postgres, one Redis and one bucket, coturn as the calls' relay, TLS from a made-up certificate
//! authority, a front on the deployment's one origin before the nodes (`own_server_nodes.rs`). Its people are made
//! with `user add`: `srv-a` (an administrator), `srv-b`, `srv-c` and more synthetic people of odd names. Each of
//! `srv-a`, `srv-b` and `srv-c` is an app of its own ([`Instance`]: `ngteams::app::launch` in its own sandbox), signed
//! in from the first sign-in screen by choosing "Own server" and typing the server's address, the user name, the
//! password and the CA file; the front serves `srv-a` and `srv-c` from one API node and `srv-b` from the other.
//!
//! 1. `srv-a` and `srv-b` sign in; each window shown (shots).
//! 2. They chat both ways over the control socket, each message shown in the other's window (its chat view's trace)
//!    in under 2 s.
//! 3. A reaction and an edit show in the other window; `srv-a` sets Busy and `srv-b`'s chat header shows it.
//! 4. `srv-a` pastes a picture and a file into its compose box (the sandbox's clipboard, Ctrl+V) and sends them:
//!    `srv-b`'s window shows them, and reads the picture and downloads the file as its window does.
//! 5. A 1:1 audio and video call `srv-a` → `srv-b`, answered over the control socket: each call window presents the
//!    other's video. Then `srv-c` signs in and `srv-a` calls the three's group chat (with the odd-named people in
//!    it): each window presents the two others' video, through the media node.
//! 6. `srv-b`'s API node is killed (`SIGKILL`): its app reconnects through the front to the other node, and what
//!    `srv-a` sent meanwhile shows in its window once it caught up.
//!
//! 7. One window with two Microsoft 365 accounts (the mock's) adds `srv-a`'s account from the rail's "+" ("Own
//!    server", typed): each account's requests go to its own services alone (the app's request log, and what the
//!    mock received), and Settings offer Manage users under the administrator `srv-a` alone.
//!
//! With `NGTS_QA_DEPLOYED_ORIGIN` (and `NGTS_QA_DEPLOYED_CA`, its certificate authority) the test deploys nothing:
//! `srv-a` and `srv-b` (made beforehand on that server with `user add` and the tests' password) run steps 1 to 4 on
//! the server at that origin, such as a Compose deployment behind its reverse proxy.
//!
//! Screenshots under `target/ui-shots/own-server-nodes-*.png`. Without a coturn (`NGTS_TEST_COTURN`,
//! `NGTS_TEST_COTURN_SECRET`) it says so and passes, as the other call tests do. Without weston 15, niri or
//! `dbus-daemon`, the server binary (`NGTEAMS_QA_SERVER_BIN`, else beside the test's target directory), Postgres or
//! Redis (`NGTEAMS_SERVER_PG_URL`, `NGTEAMS_SERVER_REDIS_URL`) it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use teams_ctl::protocol::{OpenTarget, Request, Response, StatusChoice, Toggle};

#[path = "own_server_nodes_app.rs"]
mod instance;
#[path = "own_server_nodes_mixed.rs"]
mod mixed;
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one deploys its nodes from it"
)]
mod own_server_binary;
#[path = "../own_server_nodes.rs"]
mod own_server_nodes;
#[path = "../window.rs"]
mod support;

use instance::{Instance, WAIT, Who, ensure, say, strings};
use own_server_nodes::{COTURN_ENV, COTURN_SECRET_ENV, Needs, Nodes};
use ui_sandbox::SandboxResult;

/// The V key's code (`KEY_V`), for Ctrl+V.
const KEY_V: u32 = 47;
/// The longest a message may take from its send in one window to its showing in the other.
const PUSH: Duration = Duration::from_secs(2);
const A: Who = Who {
    name: "srv-a",
    upn: "srv-a@own.test",
    node: 0,
    tone: 440.0,
};
const B: Who = Who {
    name: "srv-b",
    upn: "srv-b@own.test",
    node: 1,
    tone: 660.0,
};
const C: Who = Who {
    name: "srv-c",
    upn: "srv-c@own.test",
    node: 0,
    tone: 880.0,
};
const NAMES: [&str; 3] = ["Ada Ł. Lovelace", "Bert 🎺 Bach", "Cora 'Cee' Nuñez"];
/// Synthetic people with no app beside the odd-named ones: a crowd, as a real server has.
const CROWD: usize = 300;
/// The crowd's people in the big group chat of step 2.
const CROWD_GROUP: usize = 50;
/// Synthetic people with no app: odd names.
const ODD: [&str; 4] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "Dr. 🦀 Ferris",
    "<b>Bold</b> & \"Quoted\"",
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(instance::CHILD_ENV).as_deref() {
        Ok(mode @ ("app" | "mixed")) => instance::child(mode),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("own server nodes window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Says why the test can't run here; an error under `GPUI_SMOKE_REQUIRE=1`.
fn skip(why: &str) -> SandboxResult {
    if std::env::var("GPUI_SMOKE_REQUIRE").as_deref() == Ok("1") {
        return Err(format!("required, but {why}").into());
    }
    say(&format!("skipped: {why}"));
    Ok(())
}

/// The origin of a server deployed beforehand, for [`deployed`].
const DEPLOYED_ORIGIN_ENV: &str = "NGTS_QA_DEPLOYED_ORIGIN";
/// Its certificate authority (PEM), an account's `ca_file`.
const DEPLOYED_CA_ENV: &str = "NGTS_QA_DEPLOYED_CA";

fn parent() -> SandboxResult {
    if let Ok(origin) = std::env::var(DEPLOYED_ORIGIN_ENV) {
        let ca_file = std::env::var(DEPLOYED_CA_ENV).map_err(|_| format!("{DEPLOYED_CA_ENV} isn't set"))?;
        return deployed(&origin, Path::new(&ca_file));
    }
    // CI gives Postgres and Redis but no coturn: a missing coturn is a skip, also under `GPUI_SMOKE_REQUIRE=1`.
    if std::env::var(COTURN_ENV).is_err() || std::env::var(COTURN_SECRET_ENV).is_err() {
        say(&format!("skipped: no coturn named ({COTURN_ENV}, {COTURN_SECRET_ENV})"));
        return Ok(());
    }
    let needs = match Needs::from_env() {
        Ok(needs) => needs,
        Err(why) => return skip(&why),
    };
    let Some(binary) = own_server_binary::binary()? else {
        return skip("no ngteams-server binary (cargo build -p ngteams-server, or NGTEAMS_QA_SERVER_BIN)");
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let logs = instance::shots_dir()?;
    let mut nodes = Nodes::start(&binary, &needs, &runtime, &logs)?;
    say(&format!("deployed at {}", nodes.origin));
    let outcome = seed(&nodes).and_then(|(odd, crowd)| run(&mut nodes, &odd, &crowd));
    let outcome = outcome.map_err(|error| format!("{error}\n--- the nodes:{}", nodes.logs()));
    nodes.stop(&runtime)?;
    outcome?;
    say("ngteams own server nodes window: ok");
    Ok(())
}

/// Steps 1 to 4 on the server deployed at `origin`.
fn deployed(origin: &str, ca_file: &Path) -> SandboxResult {
    let start = |who: &Who| Instance::start_at(who, origin, None, "app");
    let Some(mut a) = start(&A)? else {
        return Ok(());
    };
    let mut b = start(&B)?.ok_or("no second sandbox")?;
    let outcome = deployed_steps(origin, ca_file, &mut a, &mut b).map_err(|error| {
        format!(
            "{error}\n--- srv-a: {}\n--- srv-b: {}\n--- the sandboxes:{}{}",
            a.running.fail(&"killed"),
            b.running.fail(&"killed"),
            a.sandbox.logs(),
            b.sandbox.logs()
        )
    });
    outcome?;
    a.quit()?;
    b.quit()?;
    say("ngteams on the deployed server: ok");
    Ok(())
}

fn deployed_steps(origin: &str, ca_file: &Path, a: &mut Instance, b: &mut Instance) -> SandboxResult {
    for instance in [&mut *a, &mut *b] {
        instance.sign_in(origin, ca_file)?;
        instance.shot("signed-in")?;
    }
    say(&format!(
        "step 1 passed: srv-a {} and srv-b {} signed in at {origin}",
        a.mri, b.mri
    ));
    let chat = chat_both_ways(a, b)?;
    say("step 2 passed");
    reaction_edit_presence(a, b, &chat)?;
    say("step 3 passed");
    picture_and_file(a, b, &chat)?;
    say("step 4 passed");
    Ok(())
}

/// The people: `srv-a` an administrator, `srv-b`, `srv-c`, the odd-named ones and the [`CROWD`] (their MRIs).
fn seed(nodes: &Nodes) -> SandboxResult<(Vec<String>, Vec<String>)> {
    for (index, who) in [A, B, C].iter().enumerate() {
        nodes.add_user(who.upn, NAMES[index], index == 0)?;
    }
    let odd = ODD
        .iter()
        .enumerate()
        .map(|(index, name)| nodes.add_user(&format!("odd.{index}@own.test"), name, false))
        .collect::<SandboxResult<Vec<_>>>()?;
    let started = Instant::now();
    let crowd = crowd(nodes)?;
    say(&format!(
        "seeded {} more people in {}ms",
        crowd.len(),
        started.elapsed().as_millis()
    ));
    Ok((odd, crowd))
}

/// The [`CROWD`], made with `user add` as an operator makes people, four at a time: their MRIs.
fn crowd(nodes: &Nodes) -> SandboxResult<Vec<String>> {
    const NAMED: [&str; 6] = [
        "ابن سينا",
        "Ana   Maria   de la Cruz",
        "Robert'); DROP TABLE users;--",
        "Zoe\u{0308} Combined",
        "null",
        "Ωmega ∑um",
    ];
    let made: Vec<Result<String, String>> = thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..CROWD)
                        .step_by(4)
                        .map(|n| {
                            let name = NAMED
                                .get(n)
                                .map_or_else(|| format!("Seeded Person {n:03}"), |name| (*name).to_owned());
                            nodes
                                .add_user(&format!("seed-{n:03}@own.test"), &name, false)
                                .map_err(|error| error.to_string())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|_| vec![Err("a seeder panicked".to_owned())])
            })
            .collect()
    });
    made.into_iter().map(|made| made.map_err(Into::into)).collect()
}

/// Every step, in order; each instance quits at the end.
fn run(nodes: &mut Nodes, odd: &[String], crowd: &[String]) -> SandboxResult {
    let Some(mut a) = Instance::start(&A, nodes)? else {
        return Ok(());
    };
    let mut b = Instance::start(&B, nodes)?.ok_or("no second sandbox")?;
    let outcome = steps(nodes, &mut a, &mut b, odd, crowd);
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- srv-a: {}\n--- srv-b: {}\n--- the sandboxes:{}{}",
            a.running.fail(&"killed"),
            b.running.fail(&"killed"),
            a.sandbox.logs(),
            b.sandbox.logs()
        )
    });
    outcome?;
    a.quit()?;
    b.quit()
}

fn steps(nodes: &mut Nodes, a: &mut Instance, b: &mut Instance, odd: &[String], crowd: &[String]) -> SandboxResult {
    for instance in [&mut *a, &mut *b] {
        instance.sign_in(&nodes.origin, &nodes.ca_file)?;
        instance.shot("signed-in")?;
    }
    say(&format!("step 1 passed: srv-a {} and srv-b {} signed in", a.mri, b.mri));
    let chat = chat_both_ways(a, b)?;
    crowd_group(a, b, crowd, &chat)?;
    say("step 2 passed");
    reaction_edit_presence(a, b, &chat)?;
    say("step 3 passed");
    picture_and_file(a, b, &chat)?;
    say("step 4 passed");
    one_to_one_call(a, b, &chat)?;
    let c = group_call(nodes, a, b, odd)?;
    say("step 5 passed");
    killed_node(nodes, a, b, &chat)?;
    say("step 6 passed");
    c.quit()?;
    mixed_window(nodes, &a.mri)?;
    say("step 7 passed");
    Ok(())
}

// --- step 2 ---

/// `srv-a` starts a chat with `srv-b`; both open it; three messages each way, each shown in the other's window in
/// under [`PUSH`]. The chat's id.
fn chat_both_ways(a: &mut Instance, b: &mut Instance) -> SandboxResult<String> {
    let chat = match a.ctl(Request::NewChat {
        people: vec![b.upn.to_owned()],
        wait: true,
    })? {
        Response::Outbox(row) => row
            .chat
            .clone()
            .or(row.message.clone())
            .ok_or_else(|| format!("no chat: {row:?}"))?,
        other => return Err(format!("new chat: {other:?}").into()),
    };
    say(&format!("the chat {chat}"));
    open_chat(a, &chat)?;
    // as in Teams, the other person has the new chat once its first message is sent
    a.ctl(Request::Send {
        chat: chat.clone(),
        text: "hi srv-b 👋".to_owned(),
        wait: true,
    })?;
    wait_for_chat(b, &chat)?;
    open_chat(b, &chat)?;
    let mut times = Vec::new();
    for n in 0..3 {
        times.push((
            "a→b",
            send_shown(a, b, &chat, &format!("srv-a → srv-b #{n} ✉️ «quoted» & co"))?.1,
        ));
        times.push(("b→a", send_shown(b, a, &chat, &format!("srv-b → srv-a #{n} 🎺"))?.1));
    }
    say(&format!("step 2 push times: {}", listed(&times)));
    ensure(times.iter().all(|(_, taken)| *taken < PUSH), || {
        format!("a message took {PUSH:?} or longer: {}", listed(&times))
    })?;
    a.shot("chat")?;
    b.shot("chat")?;
    b.theme(teams_ui::theme::DARK_KEY)?;
    b.shot("chat-dark")?;
    b.theme(teams_ui::theme::LIGHT_KEY)?;
    Ok(chat)
}

/// `srv-a` makes a group chat with `srv-b` and [`CROWD_GROUP`] of the crowd and posts in it: `srv-b`'s window shows
/// the post, and its member list has everyone. Both then open their 1:1 `chat` again.
fn crowd_group(a: &mut Instance, b: &mut Instance, crowd: &[String], chat: &str) -> SandboxResult {
    let mut people = vec![b.upn.to_owned()];
    people.extend(crowd.iter().take(CROWD_GROUP).cloned());
    let group = match a.ctl(Request::NewChat { people, wait: true })? {
        Response::Outbox(row) => row
            .chat
            .clone()
            .or(row.message.clone())
            .ok_or_else(|| format!("no group: {row:?}"))?,
        other => return Err(format!("new crowd group: {other:?}").into()),
    };
    open_chat(a, &group)?;
    a.ctl(Request::Send {
        chat: group.clone(),
        text: "hello, crowd 👋".to_owned(),
        wait: true,
    })?;
    wait_for_chat(b, &group)?;
    open_chat(b, &group)?;
    let (_, taken) = send_shown(a, b, &group, "a second post to the crowd")?;
    let members = match b.ctl(Request::Members { chat: group.clone() })? {
        Response::Members { members } => members,
        other => return Err(format!("members: {other:?}").into()),
    };
    say(&format!(
        "step 2: the crowd group {group}: shown in srv-b's window in {}ms; srv-b lists {} members",
        taken.as_millis(),
        members.len()
    ));
    ensure(members.len() == CROWD_GROUP + 2, || {
        format!("srv-b lists {} members of the crowd group", members.len())
    })?;
    b.shot("crowd-group")?;
    for instance in [&mut *a, &mut *b] {
        let mark = instance.trace.mark();
        instance.ctl(Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        })?;
        instance.trace.after(mark, WAIT, "the 1:1 chat open again", |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
    }
    Ok(())
}

fn listed(times: &[(&str, Duration)]) -> String {
    times
        .iter()
        .map(|(way, taken)| format!("{way} {}ms", taken.as_millis()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn open_chat(instance: &mut Instance, chat: &str) -> SandboxResult {
    instance.ctl(Request::Open {
        target: OpenTarget::Chat { chat: chat.to_owned() },
    })?;
    let mark = 0;
    instance.trace.after(mark, WAIT, "the chat opened", |line| {
        line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
    })?;
    Ok(())
}

/// Waits until the instance's chat list has `chat`.
fn wait_for_chat(instance: &mut Instance, chat: &str) -> SandboxResult {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Response::Chats { chats } = instance.ctl(Request::Chats {
            unread_only: false,
            limit: None,
        })? && chats.iter().any(|row| row.id == chat)
        {
            return Ok(());
        }
        ensure(Instant::now() < deadline, || {
            format!("{}: no chat {chat}", instance.name)
        })?;
        thread::sleep(Duration::from_millis(200));
    }
}

/// `from` sends `text` to `chat`; `to`'s chat view shows it. The message's id and the time from the send to the
/// showing.
fn send_shown(from: &mut Instance, to: &mut Instance, chat: &str, text: &str) -> SandboxResult<(String, Duration)> {
    let mark = to.trace.mark();
    let sent = Instant::now();
    let id = match from.ctl(Request::Send {
        chat: chat.to_owned(),
        text: text.to_owned(),
        wait: false,
    })? {
        Response::Outbox(row) => row.message,
        other => return Err(format!("{}: send: {other:?}", from.name).into()),
    };
    let id = match id {
        Some(id) => id,
        None => newest_id(from, chat)?,
    };
    let shown = to
        .trace
        .after(mark, WAIT, &format!("{id} shown"), |line| shown_in(line, chat, &id))?;
    Ok((id, shown.saturating_duration_since(sent)))
}

/// Whether `line` is `chat`'s view showing message `id`.
fn shown_in(line: &serde_json::Value, chat: &str, id: &str) -> bool {
    line["view"] == "chat-view"
        && line["change"] == "shown"
        && line["conversation"] == chat
        && strings(line, "messages").iter().any(|shown| shown == id)
}

/// The newest message of `chat` as the instance's control socket lists it.
fn newest_id(instance: &mut Instance, chat: &str) -> SandboxResult<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Response::Messages { messages, .. } = instance.ctl(Request::Messages {
            chat: chat.to_owned(),
            limit: Some(1),
            before: None,
        })? && let Some(newest) = messages.first()
            && !newest.id.starts_with("pending")
        {
            return Ok(newest.id.clone());
        }
        ensure(Instant::now() < deadline, || {
            format!("{}: no message in {chat}", instance.name)
        })?;
        thread::sleep(Duration::from_millis(100));
    }
}

// --- step 3 ---

fn reaction_edit_presence(a: &mut Instance, b: &mut Instance, chat: &str) -> SandboxResult {
    let (id, _) = send_shown(a, b, chat, "react to this, then I edit it")?;
    // srv-b reacts: srv-a's view shows the reaction
    let mark = a.trace.mark();
    let asked = Instant::now();
    b.ctl(Request::React {
        chat: chat.to_owned(),
        message: id.clone(),
        reaction: "like".to_owned(),
        wait: false,
    })?;
    let reacted = a.trace.after(mark, WAIT, "the reaction shown", |line| {
        shown_in(line, chat, &id) && strings(line, "reacted").contains(&id)
    })?;
    // srv-a edits: srv-b's view shows it edited
    let mark = b.trace.mark();
    let edit_asked = Instant::now();
    a.ctl(Request::Edit {
        chat: chat.to_owned(),
        message: id.clone(),
        text: "react to this — edited ✏️".to_owned(),
        wait: false,
    })?;
    let edited = b
        .trace
        .after(mark, WAIT, "the edit shown", |line| edited_in(line, chat, &id))?;
    a.shot("reacted")?;
    b.shot("edited")?;
    let busy = presence(a, b, chat)?;
    say(&format!(
        "step 3 times: reaction {}ms, edit {}ms, presence {}ms",
        reacted.saturating_duration_since(asked).as_millis(),
        edited.saturating_duration_since(edit_asked).as_millis(),
        busy.as_millis()
    ));
    Ok(())
}

/// Whether `line` shows message `id` of `chat` as edited.
fn edited_in(line: &serde_json::Value, chat: &str, id: &str) -> bool {
    shown_in(line, chat, id) && kind_in(line, id).as_deref() == Some("edited")
}

/// The kind `line` gives message `id` (`text`, `edited`, `unshown`: part of it can't be shown, …).
fn kind_in(line: &serde_json::Value, id: &str) -> Option<String> {
    let at = strings(line, "messages").iter().position(|shown| shown == id)?;
    strings(line, "kinds").get(at).cloned()
}

/// `srv-a` sets Busy; `srv-b`'s header of their chat shows it busy. How long it took.
///
/// First `srv-a` sets Available and `srv-b`'s header shows it: on a server deployed beforehand `srv-a` can still be
/// Busy from an earlier run (a status the user sets stays for days), and then setting Busy again changes nothing.
fn presence(a: &mut Instance, b: &mut Instance, chat: &str) -> SandboxResult<Duration> {
    a.ctl(Request::SetStatus {
        status: StatusChoice::Available,
    })?;
    b.trace.until(WAIT, "srv-a available in srv-b's header", |seen| {
        seen.iter()
            .rev()
            .find(|(_, line)| header_of(line, chat))
            .is_some_and(|(_, line)| strings(line, "kinds").iter().any(|kind| kind == "available"))
    })?;
    let mark = b.trace.mark();
    let asked = Instant::now();
    a.ctl(Request::SetStatus {
        status: StatusChoice::Busy,
    })?;
    let shown = b.trace.after(mark, WAIT, "srv-a busy in srv-b's header", |line| {
        header_of(line, chat) && strings(line, "kinds").iter().any(|kind| kind == "busy")
    })?;
    b.shot("presence-busy")?;
    Ok(shown.saturating_duration_since(asked))
}

/// Whether `line` is the header of `chat` as shown.
fn header_of(line: &serde_json::Value, chat: &str) -> bool {
    line["view"] == "header" && line["change"] == "shown" && line["conversation"] == chat
}

// --- step 4 ---

fn picture_and_file(a: &mut Instance, b: &mut Instance, chat: &str) -> SandboxResult {
    let dir = tempfile::tempdir()?;
    let png = picture_png()?;
    let id = paste_and_send(a, b, chat, vec![("image/png".to_owned(), png.clone())])?;
    let said = b.probe(&format!("picture {chat} {id}"))?;
    ensure(
        said.contains(&format!("bytes={}", png.len())) || said.contains("png=true"),
        || format!("srv-b's picture: {said}"),
    )?;
    b.shot("picture")?;
    let name = "Q4 plan «draft» ✨.txt";
    let path = dir.path().join(name);
    let body = "a file sent from one node, downloaded at the other 📎\n".repeat(400);
    std::fs::write(&path, &body)?;
    let uri = format!("file://{}\r\n", encoded_path(&path));
    let id = paste_and_send(a, b, chat, vec![("text/uri-list".to_owned(), uri.into_bytes())])?;
    let said = b.probe(&format!("file {chat} {id}"))?;
    let sum: u64 = body.bytes().map(u64::from).sum();
    ensure(said.contains(&format!("bytes={} sum={sum}", body.len())), || {
        format!("srv-b's download of the file: {said}")
    })?;
    b.shot("file")?;
    b.theme(teams_ui::theme::DARK_KEY)?;
    b.shot("file-dark")?;
    b.theme(teams_ui::theme::LIGHT_KEY)?;
    say(&format!(
        "step 4: srv-b read the picture ({} bytes) and downloaded the file ({} bytes)",
        png.len(),
        body.len()
    ));
    Ok(())
}

/// A small PNG: a gradient with a block in it.
fn picture_png() -> SandboxResult<Vec<u8>> {
    let picture = image::RgbImage::from_fn(240, 160, |x, y| {
        let block = (60..180).contains(&x) && (40..120).contains(&y);
        if block {
            image::Rgb([200, 60, 40])
        } else {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0),
                u8::try_from(y % 256).unwrap_or(0),
                160,
            ])
        }
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

/// `path` as a `file://` URI's path: every byte but the unreserved ones and `/` percent-encoded.
fn encoded_path(path: &Path) -> String {
    path.to_string_lossy()
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// `offers` on `from`'s clipboard, pasted into its compose box (Ctrl+V) and sent (Enter); shown in `to`'s window.
/// The message's id.
fn paste_and_send(
    from: &mut Instance,
    to: &mut Instance,
    chat: &str,
    offers: Vec<(String, Vec<u8>)>,
) -> SandboxResult<String> {
    from.focus_main()?;
    let mut input = from.sandbox.input()?;
    let picture = offers.iter().any(|(mime, _)| mime.starts_with("image/"));
    from.sandbox.set_clipboard(offers)?;
    let mark = from.trace.mark();
    input.key_with(&[ui_sandbox::input::Modifier::Ctrl], KEY_V)?;
    if picture {
        // a picture goes into the text; a file is attached beside it, and its upload starts
        from.trace.after(mark, WAIT, "the paste in the compose box", |line| {
            line["view"] == "compose" && line["change"] == "changed" && line["conversation"] == chat
        })?;
    }
    thread::sleep(Duration::from_secs(3));
    from.shot(if picture { "pasted-picture" } else { "pasted-file" })?;
    let before = own_messages(from, chat)?;
    let to_mark = to.trace.mark();
    let mark = from.trace.mark();
    let sent = |line: &serde_json::Value| {
        line["view"] == "compose" && line["change"] == "sent" && line["conversation"] == chat
    };
    input.key(ui_sandbox::input::keys::ENTER)?;
    if from.trace.after(mark, Duration::from_secs(20), "sent", sent).is_err() {
        // still uploading when Enter came: Enter again, as a user would
        input.key(ui_sandbox::input::keys::ENTER)?;
        from.trace.after(mark, WAIT, "sent", sent)?;
    }
    from.trace.after(mark, WAIT, "the outbox row settled", |line| {
        line["view"] == "outbox"
            && line["conversation"] == chat
            && strings(line, "kinds")
                .iter()
                .any(|kind| kind == "sent" || kind == "failed")
    })?;
    let id = new_own_message(from, chat, &before)?;
    to.trace
        .after(to_mark, WAIT, &format!("{id} shown"), |line| shown_in(line, chat, &id))?;
    // the newest line of it: drawn whole, no part of it a placeholder
    let kind = to
        .trace
        .seen
        .iter()
        .rev()
        .find(|(_, line)| shown_in(line, chat, &id))
        .and_then(|(_, line)| kind_in(line, &id));
    ensure(kind.as_deref() == Some("text"), || {
        format!("{}: {id} is shown as {kind:?}", to.name)
    })?;
    Ok(id)
}

/// The ids of `chat`'s newest messages that are `from`'s own.
fn own_messages(from: &mut Instance, chat: &str) -> SandboxResult<Vec<String>> {
    match from.ctl(Request::Messages {
        chat: chat.to_owned(),
        limit: Some(10),
        before: None,
    })? {
        Response::Messages { messages, .. } => Ok(messages
            .into_iter()
            .filter(|message| message.from_me)
            .map(|message| message.id)
            .collect()),
        other => Err(format!("{}: messages: {other:?}", from.name).into()),
    }
}

/// The id of `from`'s own message in `chat` that isn't among `before`, once the service gave it one.
fn new_own_message(from: &mut Instance, chat: &str, before: &[String]) -> SandboxResult<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = own_messages(from, chat)?;
        if let Some(id) = now
            .iter()
            .find(|id| !before.contains(id) && id.chars().all(|char| char.is_ascii_digit()))
        {
            return Ok(id.clone());
        }
        ensure(Instant::now() < deadline, || {
            format!(
                "{}: no new message of its own in {chat}: {now:?} (before {before:?})",
                from.name
            )
        })?;
        thread::sleep(Duration::from_millis(200));
    }
}

// --- step 5 ---

/// Waits until the instance's window presented video of `lines` others (each on a line of its own).
fn presents(instance: &mut Instance, lines: usize) -> SandboxResult {
    let what = format!("{}: {lines} others' video presented", instance.name);
    instance.trace.until(WAIT, &what, |seen| {
        let kinds: std::collections::BTreeSet<String> = seen
            .iter()
            .filter(|(_, line)| line["view"] == "video-tile" && line["change"] == "presented")
            .flat_map(|(_, line)| strings(line, "kinds"))
            .collect();
        kinds.iter().filter(|kind| is_others(kind)).count() >= lines
    })
}

/// Whether a video tile's kind is someone else's: a 1:1 call's other party (`remote`) or a group call's line.
fn is_others(kind: &str) -> bool {
    kind == "remote" || kind.starts_with("line-")
}

fn one_to_one_call(a: &mut Instance, b: &mut Instance, chat: &str) -> SandboxResult {
    let (a_mark, b_mark) = (a.trace.mark(), b.trace.mark());
    a.ctl(Request::Call { chat: chat.to_owned() })?;
    b.wait_call("srv-b rings", |row| row.ringing)?;
    b.ctl(Request::Answer)?;
    for instance in [&mut *a, &mut *b] {
        instance.wait_call("connected", |row| row.state == "connected")?;
        instance.ctl(Request::Camera { op: Toggle::On })?;
    }
    for (instance, mark) in [(&mut *a, a_mark), (&mut *b, b_mark)] {
        instance
            .trace
            .after(mark, WAIT, "the other's video presented", |line| {
                line["view"] == "video-tile"
                    && line["change"] == "presented"
                    && strings(line, "kinds").iter().any(|kind| is_others(kind))
            })?;
    }
    thread::sleep(Duration::from_secs(3));
    for instance in [&*a, &*b] {
        let (_, title) = instance.focused_window()?;
        say(&format!("{}: the call window {title:?}", instance.name));
        instance.shot("call-1-1")?;
    }
    b.theme(teams_ui::theme::DARK_KEY)?;
    b.shot("call-1-1-dark")?;
    b.theme(teams_ui::theme::LIGHT_KEY)?;
    hang_up(&mut [a, b])
}

/// Everyone leaves; no call is up anywhere.
fn hang_up(instances: &mut [&mut Instance]) -> SandboxResult {
    for instance in instances.iter_mut() {
        // a 1:1 call ends for both once one leaves: the other has none to hang up then
        if let Err(error) = instance.ctl(Request::HangUp)
            && !error.to_string().contains("no call is up")
        {
            return Err(error);
        }
    }
    for instance in instances.iter_mut() {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = instance.calls()?;
            if calls.iter().all(|row| !row.ringing && row.state != "connected") {
                break;
            }
            ensure(Instant::now() < deadline, || {
                format!("{}: still {calls:?}", instance.name)
            })?;
            thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

/// `srv-c` signs in; `srv-a` calls the three's group chat (the odd-named people in it too); `srv-b` and `srv-c`
/// answer; every camera on; each window presents the two others' video.
fn group_call(nodes: &Nodes, a: &mut Instance, b: &mut Instance, odd: &[String]) -> SandboxResult<Instance> {
    let mut c = Instance::start(&C, nodes)?.ok_or("no third sandbox")?;
    c.sign_in(&nodes.origin, &nodes.ca_file)?;
    c.shot("signed-in")?;
    let mut people = vec![b.upn.to_owned(), c.upn.to_owned()];
    people.extend(odd.iter().cloned());
    let group = match a.ctl(Request::NewChat { people, wait: true })? {
        Response::Outbox(row) => row
            .chat
            .clone()
            .or(row.message.clone())
            .ok_or_else(|| format!("no group: {row:?}"))?,
        other => return Err(format!("new group: {other:?}").into()),
    };
    say(&format!("the group {group}"));
    a.ctl(Request::Call { chat: group.clone() })?;
    for instance in [&mut *b, &mut c] {
        instance.wait_call("rings", |row| row.ringing)?;
        instance.ctl(Request::Answer)?;
    }
    for instance in [&mut *a, &mut *b, &mut c] {
        instance.wait_call("connected", |row| row.state == "connected")?;
        instance.ctl(Request::Camera { op: Toggle::On })?;
    }
    thread::sleep(Duration::from_secs(8));
    for instance in [&mut *a, &mut *b, &mut c] {
        presents(instance, 2)?;
        instance.shot("call-group")?;
    }
    b.theme(teams_ui::theme::DARK_KEY)?;
    b.shot("call-group-dark")?;
    b.theme(teams_ui::theme::LIGHT_KEY)?;
    hang_up(&mut [a, b, &mut c])?;
    Ok(c)
}

// --- step 6 ---

/// `srv-b`'s node is killed; `srv-a` sends meanwhile; `srv-b`'s window shows it once its app reconnected through the
/// other node and caught up.
fn killed_node(nodes: &mut Nodes, a: &mut Instance, b: &mut Instance, chat: &str) -> SandboxResult {
    let before = nodes.connections(b.name);
    say(&format!("srv-b's connections per node before the kill: {before:?}"));
    nodes.kill_api(b.node)?;
    let killed = Instant::now();
    let mut ids = Vec::new();
    for n in 0..3 {
        let mark = b.trace.mark();
        let id = match a.ctl(Request::Send {
            chat: chat.to_owned(),
            text: format!("while srv-b's node is down #{n}"),
            wait: true,
        })? {
            Response::Outbox(row) => row.message.clone().ok_or_else(|| format!("no id: {row:?}"))?,
            other => return Err(format!("send: {other:?}").into()),
        };
        ids.push((id, mark));
    }
    for (id, mark) in &ids {
        b.trace.after(
            *mark,
            Duration::from_secs(90),
            &format!("{id} shown after the kill"),
            |line| shown_in(line, chat, id),
        )?;
    }
    let caught_up = killed.elapsed();
    let after = nodes.connections(b.name);
    say(&format!("srv-b's connections per node after the kill: {after:?}"));
    let other = 1 - b.node;
    ensure(after.get(other) > before.get(other), || {
        format!("srv-b never reached the other node: {before:?} → {after:?}")
    })?;
    let (_, back) = send_shown(b, a, chat, "srv-b again, through the other node")?;
    let (_, there) = send_shown(a, b, chat, "srv-a to srv-b after the kill")?;
    say(&format!(
        "step 6 times: caught up {}ms after the kill; then b→a {}ms, a→b {}ms",
        caught_up.as_millis(),
        back.as_millis(),
        there.as_millis()
    ));
    b.shot("after-node-killed")?;
    nodes.start_api(b.node)
}

// --- step 7 ---

/// The rail's "+" with two accounts on the rail, in the sandbox's 1280×800 output.
const RAIL_ADD_AT: (u32, u32) = (38, 713);

/// One window with the mock's a and b; `srv-a` added from the rail's "+"; where the requests went; Manage users.
fn mixed_window(nodes: &Nodes, srv_a: &str) -> SandboxResult {
    let who = Who {
        name: "mixed",
        upn: A.upn,
        node: 0,
        tone: 440.0,
    };
    let mut mixed = Instance::start_mode(&who, nodes, "mixed")?.ok_or("no sandbox for the mixed window")?;
    let outcome = mixed_steps(nodes, &mut mixed, srv_a);
    let outcome = outcome.map_err(|error| format!("{error}\n--- the mixed window: {}", mixed.running.fail(&"killed")));
    outcome?;
    mixed.quit()
}

fn mixed_steps(nodes: &Nodes, mixed: &mut Instance, srv_a: &str) -> SandboxResult {
    for account in [teams_mock::LAB_A, teams_mock::LAB_B] {
        mixed.trace.any(WAIT, "a mock account on the rail", |line| {
            about(line, "account", "added", account)
        })?;
    }
    mixed.trace.any(WAIT, "connected", |line| {
        line["view"] == "status" && line["change"] == "connected"
    })?;
    mixed.connect_ctl()?;
    mixed.shot("two-microsoft")?;
    let mut input = mixed.sandbox.input()?;
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, ui_sandbox::input::keys::BTN_LEFT)?;
    mixed.type_own_sign_in(&nodes.origin, &nodes.ca_file)?;
    mixed
        .trace
        .any(Duration::from_secs(180), "srv-a's account added", |line| {
            about(line, "account", "added", srv_a)
        })?;
    mixed.shot("three-accounts")?;
    let admin = format!("{}/{}", nodes.origin, A.upn);
    let users = || Request::Open {
        target: OpenTarget::Settings {
            page: Some("users".to_owned()),
        },
    };
    let offered = |mixed: &mut Instance, account: &str| mixed.ctl_as(account, users()).is_ok();
    let deadline = Instant::now() + WAIT;
    while !offered(mixed, &admin) {
        ensure(Instant::now() < deadline, || {
            "srv-a, an administrator, isn't offered Manage users".to_owned()
        })?;
        thread::sleep(Duration::from_millis(300));
    }
    thread::sleep(Duration::from_secs(2));
    mixed.shot("manage-users")?;
    for account in [teams_mock::LAB_A, teams_mock::LAB_B] {
        ensure(!offered(mixed, account), || {
            format!("the Microsoft 365 account {account} is offered Manage users")
        })?;
    }
    requests_stay(mixed)
}

/// A trace line of `view` with `change` naming the account `account` among its people.
fn about(line: &serde_json::Value, view: &str, change: &str, account: &str) -> bool {
    line["view"] == view && line["change"] == change && line["people"] == serde_json::json!([account])
}

/// The own account's requests went to its server, sign-in and authz among them, and no sign-in or authz request went
/// anywhere else; the mock got no sign-in or authz request and nothing carrying the own account's token.
fn requests_stay(mixed: &mut Instance) -> SandboxResult {
    let requests = mixed.probe("requests")?;
    say(&format!("step 7 {requests}"));
    let field = |line: &str, name: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
            .unwrap_or_default()
    };
    let paths = field(&requests, "own_paths");
    let signed_there = ["/oauth2/v2.0/token", "/api/authsvc/v1.0/authz"]
        .iter()
        .all(|path| paths.split(',').any(|own| own == *path));
    ensure(
        signed_there && field(&requests, "elsewhere_auth") == "0" && field(&requests, "elsewhere") != "0",
        || format!("the requests: {requests}"),
    )?;
    let mock = mixed.probe("mock")?;
    say(&format!("step 7 {mock}"));
    ensure(
        field(&mock, "auth_paths") == "0" && field(&mock, "own_token") == "0" && field(&mock, "total") != "0",
        || format!("the mock's requests: {mock}"),
    )
}
