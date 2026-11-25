//! QA scenes of task 188 (round 1): an own server's account hides the services its server does not serve, and its
//! title-bar search is the server's, on the real `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`); a and b
//! (Microsoft 365, on the mock) beside it in the same window keep every service.
//!
//! The parent starts the server with Workflows on or off (`[workflows] enabled`) and seeds synthetic people as an
//! operator does (`user add`, `profile set`): Bert (Engineer, Platform), Cora, Zoë (an accented name) and a person with
//! a very long name. Bert writes his 1:1 chat with the account and the group chat "Launch QA ✈️" (the account, Bert,
//! Cora, Zoë), and Zoë posts there too, over the server's HTTP API. Before the window opens, the child's own account
//! makes the team "Rocket Team" with Bert on it, its channel "Launch Plans" with a post, and a Meet now meeting, through
//! its engine as the app does.
//!
//! Then the parent asks the child for one step at a time (`hidden-step`): the child does what the user's click does
//! through the shell, says the facts it reads back (`step <name> key=value …`: the account's capabilities, the place
//! shown, the Workflows app, the Manage team tab, the meeting chat's tab, the card open, the search's finds), and the
//! parent screenshots it under `target/ui-shots/ngteams-qa-hidden-*`. Where only a pointer opens a thing (the call's
//! More menu, a person's card from the search results) the parent moves the sandbox's pointer.
//!
//! Own account: the rail (More apps only while Workflows is on) and More apps' flyout; the Workflows app; a channel's
//! menu; a message's menu; Manage team asked for its Apps tab; the meeting chat's tabs with Recap asked; a call's More
//! menu; the search's suggestions, its All tab, a person (and the card from their face), a message (and the chat
//! opened at it), a group chat and a channel. Then a's rail, message menu and call More menu.

use std::fs;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle};
use ngteams::accounts::{self, Runner, Started};
use ngteams::accounts_file::OwnServer;
use teams_auth::secrets::Secret;
use teams_mock::{AC_CHAT, LAB_A, LAB_B};
use teams_proto::{Mri, NewTeam, TeamRole, TeamWrite};
use teams_ui::chat_view::Item;
use teams_ui::global_search::{Mode as SearchMode, Tab};
use teams_ui::manage_team::ManageTab;
use teams_ui::shell::{ChatTab, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

use super::card_qa::{Writer, mri_of};
use super::{
    OWN_CA_ENV, OWN_NAME, OWN_ORIGIN_ENV, OWN_UPN, ROOT_ENV, TraceReader, WAIT, about, accounts_shown, ctl, ensure,
    own, say, shot, start_with,
};

/// The file in the sandbox dir the parent writes a step's name to.
const STEP_ASK: &str = "hidden-step";
/// The file the parent writes the seeded people's ids and the chats to (`<key> <value>` a line).
const SEEDED_FILE: &str = "hidden-seeded";
/// The file the child writes what its account made to (`<key> <value>` a line).
const MADE_FILE: &str = "hidden-made";
/// How long a step may take to find what it acts on (the lists load, the search answers).
const STEP_WAIT: Duration = Duration::from_secs(45);

const BERT: &str = "bert.hidden@lab.example.test";
const CORA: &str = "cora.hidden@lab.example.test";
const ZOE: &str = "zoe.hidden@lab.example.test";
const LONG: &str = "maximiliana.hidden@lab.example.test";
const BERT_NAME: &str = "Bert Sample";
const ZOE_NAME: &str = "Zoë Ångström-Øvergaard";
const LONG_NAME: &str = "Maximiliana Theodora Wilhelmina von Hohenzollern-Sigmaringen-Ostrowski-Papadopoulou the Third";

const BERT_SAYS_ONE: &str = "Hi, the launch checklist is ready for a look";
const BERT_SAYS: &str = "The rocket review moved to Friday";
const ZOE_SAYS: &str = "Crème brûlée after the review? 🍮";
const GROUP_TOPIC: &str = "Launch QA ✈️";
const TEAM: &str = "Rocket Team";
const CHANNEL: &str = "Launch Plans";
const CHANNEL_POST: &str = "Countdown starts at noon 🚀";
const MEETING: &str = "Weekly sync";
const MEETING_SAYS: &str = "Agenda for the weekly sync";

// --- the parent ---

/// What a scene runs on: the window's width, Teams Dark, and whether the server serves Workflows.
#[derive(Clone, Copy)]
pub struct Look {
    pub width: u32,
    pub dark: bool,
    pub workflows: bool,
}

impl Look {
    fn suffix(self) -> String {
        format!(
            "-{}{}{}",
            self.width,
            if self.dark { "-dark" } else { "" },
            if self.workflows { "-workflows-on" } else { "" }
        )
    }
}

fn sandbox_at(width: u32) -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        width,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    sandbox_or_skip("ngteams own-server hidden services QA", &options)
}

/// A point the parent clicks or rests on: `NGTEAMS_HIDDEN_QA_<NAME>=x,y` when set, else `default`.
fn at(name: &str, default: (u32, u32)) -> (u32, u32) {
    std::env::var(format!("NGTEAMS_HIDDEN_QA_{name}"))
        .ok()
        .and_then(|value| {
            let (x, y) = value.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .unwrap_or(default)
}

/// Adds the account and the people, sets their profiles, and has Bert and Zoë write; writes the ids and chats to
/// [`SEEDED_FILE`].
fn seed(sandbox: &Sandbox, server: &own::Own) -> SandboxResult {
    use std::fmt::Write as _;
    let mut people = vec![(OWN_UPN.to_owned(), server.add_user(OWN_UPN, OWN_NAME)?)];
    for (upn, name) in [
        (BERT, BERT_NAME),
        (CORA, "Cora Chief"),
        (ZOE, ZOE_NAME),
        (LONG, LONG_NAME),
    ] {
        people.push((upn.to_owned(), server.add_user(upn, name)?));
    }
    server.cli(
        &[
            "profile",
            "set",
            BERT,
            "--job-title",
            "Engineer",
            "--department",
            "Platform",
        ],
        None,
    )?;
    server.cli(
        &[
            "profile",
            "set",
            ZOE,
            "--job-title",
            "Ingénieure",
            "--department",
            "Recherche & Développement",
        ],
        None,
    )?;
    let bert = Writer::signed_in(server, BERT)?;
    let me = mri_of(&people, BERT)?;
    let one = bert.chat(&me, &[mri_of(&people, OWN_UPN)?], None)?;
    let others = [OWN_UPN, CORA, ZOE]
        .iter()
        .map(|upn| mri_of(&people, upn))
        .collect::<SandboxResult<Vec<_>>>()?;
    let group = bert.chat(&me, &others, Some(GROUP_TOPIC))?;
    bert.post(&one, BERT_SAYS_ONE)?;
    bert.post(&group, BERT_SAYS)?;
    Writer::signed_in(server, ZOE)?.post(&group, ZOE_SAYS)?;
    let mut seeded = String::new();
    for (upn, id) in &people {
        let _ = writeln!(seeded, "{upn} {id}");
    }
    let _ = writeln!(seeded, "one {one}");
    let _ = writeln!(seeded, "group {group}");
    fs::write(sandbox.path(SEEDED_FILE), seeded)?;
    Ok(())
}

/// The `<key> <value>` lines of `file` in the sandbox dir.
fn pairs(path: &std::path::Path) -> SandboxResult<Vec<(String, String)>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect())
}

fn value(pairs: &[(String, String)], key: &str) -> SandboxResult<String> {
    pairs
        .iter()
        .find(|(held, _)| held == key)
        .map(|(_, value)| value.clone())
        .ok_or_else(|| format!("no {key} in {pairs:?}").into())
}

/// A step's facts: `step <name> key=value …`.
struct Facts(Vec<(String, String)>);

impl Facts {
    fn parse(line: &str) -> Self {
        Self(
            line.split(' ')
                .skip(2)
                .filter_map(|field| {
                    let (key, value) = field.split_once('=')?;
                    Some((key.to_owned(), value.to_owned()))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value.as_str())
    }
}

/// One run of a scene.
struct Run<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    trace: TraceReader,
    input: VirtualInput,
    look: Look,
    /// What the run saw that it should not have: every one is reported at the end.
    wrong: Vec<String>,
}

impl Run<'_> {
    fn shot(&self, name: &str) -> SandboxResult {
        shot(
            self.sandbox,
            &format!("ngteams-qa-hidden-{name}{}.png", self.look.suffix()),
        )
    }

    /// Has the child do `step`, then screenshots it as `name`: the facts it said.
    fn step_shot(&mut self, step: &str, name: &str) -> SandboxResult<Facts> {
        let facts = self.step(step)?;
        self.shot(name)?;
        Ok(facts)
    }

    /// Has the child do `step`: the facts it said.
    fn step(&mut self, step: &str) -> SandboxResult<Facts> {
        let staged = self.sandbox.path("hidden-step.staged");
        fs::write(&staged, step)?;
        fs::rename(&staged, self.sandbox.path(STEP_ASK))?;
        let line = self
            .running
            .line_where(STEP_WAIT + WAIT, |line| line.starts_with(&format!("step {step} ")))
            .map_err(|error| self.running.fail(&format!("no step {step}: {error:?}")))?;
        say(&line);
        Ok(Facts::parse(&line))
    }

    /// Notes `what` as wrong unless `holds`.
    fn expect(&mut self, holds: bool, what: impl FnOnce() -> String) {
        if !holds {
            let what = what();
            say(&format!("WRONG {what}"));
            self.wrong.push(what);
        }
    }

    fn expect_fact(&mut self, facts: &Facts, key: &str, wanted: &str, step: &str) {
        let got = facts.get(key).to_owned();
        self.expect(got == wanted, || format!("{step}: {key} is {got:?}, not {wanted:?}"));
    }
}

/// The scene of `look` (see the module docs).
pub fn hidden_at(look: Look) -> SandboxResult {
    let Some(sandbox) = sandbox_at(look.width)? else {
        return Ok(());
    };
    let tables = if look.workflows {
        ""
    } else {
        "[workflows]\nenabled = false\n"
    };
    let server = own::Own::start_configured(tables)?;
    seed(&sandbox, &server)?;
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let mode = if look.dark { "hidden-dark" } else { "hidden" };
    let (mut running, mut trace, _) = start_with(&sandbox, mode, &kept)?;
    let seeded = pairs(&sandbox.path(SEEDED_FILE))?;
    let reader = mri_of(&seeded, OWN_UPN)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, super::LAB_B, &reader])?;
    let input = sandbox.input()?;
    let mut run = Run {
        sandbox: &sandbox,
        running,
        trace,
        input,
        look,
        wrong: Vec::new(),
    };
    let checked = own_account(&mut run, &server, &reader).and_then(|()| microsoft_beside(&mut run));
    if let Some(log) = server.log() {
        fs::write(
            super::shots_dir()?.join(format!("ngteams-qa-hidden-server{}.log", look.suffix())),
            log,
        )?;
    }
    if let Err(error) = checked {
        let _ = run.shot("failed");
        return Err(run.running.fail(&error.to_string()));
    }
    let Run {
        mut trace,
        running,
        wrong,
        ..
    } = run;
    super::quit_and_finish(&sandbox, &mut trace, running)?;
    ensure(wrong.is_empty(), || {
        format!("{} wrong: {}", wrong.len(), wrong.join("; "))
    })
}

/// The capabilities an own server's account has: Workflows as the server serves it, nothing else.
fn own_capabilities(workflows: bool) -> String {
    format!("workflows:{workflows},recap:false,captions:false,recording:false,apps:false,copilot:false")
}

const ALL_CAPABILITIES: &str = "workflows:true,recap:true,captions:true,recording:true,apps:true,copilot:true";

/// The own account shown: each hidden service, then the search.
fn own_account(run: &mut Run, server: &own::Own, reader: &str) -> SandboxResult {
    ctl(
        run.sandbox,
        &["account", "switch", &format!("{}/{OWN_UPN}", server.origin())],
    )?;
    run.trace
        .wait_where("switched to the own account", |line| {
            about(line, "account", "switched", reader)
        })
        .map_err(|error| run.running.fail(&error.to_string()))?;
    let workflows = run.look.workflows;
    let facts = run.step_shot("rail", "own-rail")?;
    run.expect_fact(&facts, "caps", &own_capabilities(workflows), "own rail");
    run.step_shot("more-apps", "own-more-apps")?;
    let facts = run.step_shot("workflows-app", "own-workflows-app")?;
    run.expect_fact(
        &facts,
        "workflows_app",
        if workflows { "open" } else { "none" },
        "own Workflows app",
    );
    if !workflows {
        // the Workflows app, once open, closed the flyout itself
        run.step("more-apps-close")?;
    }
    run.step_shot("channel-menu", "own-channel-menu")?;
    run.input.key(keys::ESC)?;
    run.step("menu-close")?;
    let facts = run.step_shot("message-menu", "own-message-menu")?;
    run.expect(!facts.get("options").is_empty(), || {
        "own message menu: no menu open".to_owned()
    });
    run.step("message-menu-close")?;
    let facts = run.step_shot("manage-team-apps", "own-manage-team")?;
    run.expect_fact(&facts, "manage_tab", "Channels", "own Manage team asked for Apps");
    let facts = run.step_shot("meeting-recap", "own-meeting-tabs")?;
    run.expect_fact(&facts, "chat_tab", "Chat", "own meeting chat asked for Recap");
    call_more(run, "own")?;
    search(run)
}

/// A call's More menu, opened with the pointer in the call's window; then the call ends.
fn call_more(run: &mut Run, who: &str) -> SandboxResult {
    run.step_shot("call", &format!("{who}-call"))?;
    let more = at(
        &format!("CALL_MORE_{}", run.look.width),
        call_more_default(run.look.width),
    );
    run.input.click(more.0, more.1, keys::BTN_LEFT)?;
    std::thread::sleep(Duration::from_millis(800));
    run.shot(&format!("{who}-call-more"))?;
    run.input.key(keys::ESC)?;
    run.step("call-end")?;
    Ok(())
}

fn call_more_default(width: u32) -> (u32, u32) {
    if width <= 480 { (384, 27) } else { (width - 195, 27) }
}

/// The title bar's search: suggestions, All, a person and their card, a message and its chat, a group chat, a channel.
fn search(run: &mut Run) -> SandboxResult {
    let facts = run.step_shot("search-suggestions", "own-search-suggestions")?;
    run.expect(facts.get("suggestions") != "0", || {
        "own suggestions for 'Ber': none".to_owned()
    });
    let facts = run.step_shot("search-all", "own-search-all")?;
    run.expect(facts.get("hits") != "0", || {
        "own search 'launch': no message".to_owned()
    });
    let facts = run.step_shot("search-person", "own-search-person")?;
    run.expect_fact(&facts, "people", &BERT_NAME.replace(' ', "_"), "own search 'Bert'");
    let face = at(
        &format!("RESULT_FACE_{}", run.look.width),
        if run.look.width <= 480 { (129, 216) } else { (347, 181) },
    );
    run.input.move_to(face.0, face.1)?;
    let start = Instant::now();
    let card = loop {
        let facts = run.step("facts")?;
        // the card opens before its person is read: wait for the read
        if !matches!(facts.get("card"), "" | "loading") || start.elapsed() > WAIT {
            break facts;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    run.shot("own-search-person-card")?;
    run.expect(card.get("card") == "Bert_Sample", || {
        format!("own search: the card from Bert's face is {:?}", card.get("card"))
    });
    run.input.move_to(run.look.width - 20, 780)?;
    run.step("card-close")?;
    let facts = run.step_shot("search-message", "own-search-message")?;
    run.expect(facts.get("hits") == "1", || {
        format!("own search 'checklist': {} messages", facts.get("hits"))
    });
    let facts = run.step_shot("search-message-open", "own-search-message-opened")?;
    run.expect(facts.get("main").starts_with("Chat("), || {
        format!("own search: the message opened {}", facts.get("main"))
    });
    let facts = run.step_shot("search-chat", "own-search-chat")?;
    run.expect(facts.get("chats") == "1", || {
        format!("own search 'Launch QA': {} chats", facts.get("chats"))
    });
    let facts = run.step_shot("search-channel", "own-search-channel")?;
    run.expect(facts.get("channels") != "0", || {
        "own search 'Launch Plans': no channel".to_owned()
    });
    let facts = run.step_shot("search-nothing", "own-search-nothing")?;
    run.expect(facts.get("hits") == "0" && facts.get("people").is_empty(), || {
        "own search 'zzqx': found something".to_owned()
    });
    run.step("search-leave")?;
    Ok(())
}

/// a (Microsoft 365, on the mock) in the same window keeps every service.
fn microsoft_beside(run: &mut Run) -> SandboxResult {
    ctl(run.sandbox, &["account", "switch", LAB_A])?;
    run.trace
        .wait_where("switched to a", |line| about(line, "account", "switched", LAB_A))
        .map_err(|error| run.running.fail(&error.to_string()))?;
    let facts = run.step_shot("rail", "microsoft-rail")?;
    run.expect_fact(&facts, "caps", ALL_CAPABILITIES, "a's rail");
    run.step_shot("more-apps", "microsoft-more-apps")?;
    run.step("more-apps-close")?;
    // c says something in a's chat with c, so a has a message of someone else's to open a menu on
    let staged = run.sandbox.path("post-as-c.staged");
    fs::write(&staged, AC_CHAT)?;
    fs::rename(&staged, run.sandbox.path(super::POST_TRIGGER))?;
    let facts = run.step_shot("message-menu", "microsoft-message-menu")?;
    run.expect(!facts.get("options").is_empty(), || {
        "a's message menu: no menu open".to_owned()
    });
    run.step("message-menu-close")?;
    call_more(run, "microsoft")
}

// --- the child ---

/// What the own account made before the window opened.
struct Made {
    team: String,
    channel: String,
    meeting: String,
    group: String,
    bert: String,
}

/// The own account of the `hidden` modes, signed in on the parent's server and started; it makes its team, channel,
/// post and meeting through its engine and writes them to [`MADE_FILE`].
pub fn started(runner: &Runner, server: &OwnServer) -> SandboxResult<Vec<Started>> {
    let account = accounts::add_own(&runner.setup, OWN_UPN, server, &Secret::new(own::PASSWORD))?;
    let started = runner.start(&account).map_err(String::from)?;
    let dir = own::sandbox_dir()?;
    let seeded = pairs(&dir.join(SEEDED_FILE))?;
    let bert = mri_of(&seeded, BERT)?;
    let engine = &started.engine.handle;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let made = runtime.block_on(async {
        let written = engine
            .team_write(
                TeamWrite::Create(NewTeam {
                    name: TEAM.into(),
                    description: "Synthetic QA team".into(),
                    first_channel: "General".into(),
                    public: false,
                }),
                None,
            )
            .await
            .map_err(|error| format!("team: {error:?}"))?;
        let (team, group) = written.created.ok_or("the team wasn't made")?;
        let group = group.ok_or("the team has no group")?;
        engine
            .team_write(
                TeamWrite::AddMembers {
                    team: team.clone(),
                    group: group.clone(),
                    members: vec![(Mri::new(&bert), TeamRole::Member)],
                },
                None,
            )
            .await
            .map_err(|error| format!("add Bert: {error:?}"))?;
        let draft = teams_core::ChannelDraft {
            channel: teams_proto::NewChannel {
                team: team.clone(),
                group: group.clone(),
                mailbox: None,
                name: CHANNEL.into(),
                description: "Synthetic QA channel".into(),
                kind: teams_proto::NewChannelKind::Standard,
                layout: teams_proto::ChannelLayout::Threads,
            },
            show_by_default: true,
        };
        let channel = engine
            .create_channel(draft)
            .await
            .map_err(|error| format!("channel: {error:?}"))?
            .thread
            .ok_or("the channel has no thread")?;
        engine
            .write(teams_core::engine::Action::Send {
                conversation: channel.clone(),
                text: teams_proto::chat::MessageText::plain(CHANNEL_POST).map_err(|error| format!("{error:?}"))?,
            })
            .await
            .map_err(|error| format!("post: {error:?}"))?;
        let meeting = engine
            .meet_now(MEETING)
            .await
            .map_err(|error| format!("meet now: {error:?}"))?
            .thread;
        // a meeting's chat is listed once something is said in it
        engine
            .write(teams_core::engine::Action::Send {
                conversation: meeting.clone(),
                text: teams_proto::chat::MessageText::plain(MEETING_SAYS).map_err(|error| format!("{error:?}"))?,
            })
            .await
            .map_err(|error| format!("meeting post: {error:?}"))?;
        Ok::<_, String>((team, channel, meeting))
    })?;
    let (team, channel, meeting) = made;
    fs::write(
        dir.join(MADE_FILE),
        format!("team {team}\nchannel {channel}\nmeeting {meeting}\n"),
    )?;
    say(&format!("hidden-made team={team} channel={channel} meeting={meeting}"));
    Ok(vec![started])
}

fn shell_of(window: WindowHandle<KitRoot>, cx: &mut App) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// Answers the parent's steps while the app runs (see the module docs).
pub fn answer(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Ok(dir) = own::sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut made: Option<Made> = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(step) = fs::read_to_string(dir.join(STEP_ASK)) else {
                continue;
            };
            let _ = fs::remove_file(dir.join(STEP_ASK));
            if made.is_none() {
                made = read_made(&dir);
            }
            let start = Instant::now();
            let mut done = false;
            while !done && start.elapsed() < STEP_WAIT {
                done = cx
                    .update(|cx| {
                        let shell = shell_of(window, cx)?;
                        window
                            .update(cx, |_, window, cx| act(&step, &shell, made.as_ref(), window, cx))
                            .ok()
                    })
                    .unwrap_or(false);
                if !done {
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                }
            }
            // the views read what the step asked for, then draw it
            cx.background_executor().timer(Duration::from_millis(700)).await;
            while start.elapsed() < STEP_WAIT && !cx.update(|cx| settled(&step, window, cx)) {
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let facts = cx.update(|cx| facts(window, cx));
            say(&format!("step {step} done={done} {facts}"));
        }
    })
    .detach();
}

fn read_made(dir: &std::path::Path) -> Option<Made> {
    let made = pairs(&dir.join(MADE_FILE)).ok()?;
    let seeded = pairs(&dir.join(SEEDED_FILE)).ok()?;
    Some(Made {
        team: value(&made, "team").ok()?,
        channel: value(&made, "channel").ok()?,
        meeting: value(&made, "meeting").ok()?,
        group: value(&seeded, "group").ok()?,
        bert: mri_of(&seeded, BERT).ok()?,
    })
}

/// Does `step` on the shell: whether it could (what it acts on is there yet).
fn act(step: &str, shell: &Entity<Shell>, made: Option<&Made>, window: &mut Window, cx: &mut App) -> bool {
    if step.starts_with("search-") {
        return act_search(step, shell, window, cx);
    }
    if step.starts_with("call") {
        return act_call(step, shell, made, cx);
    }
    match step {
        "rail" => {
            shell.update(cx, |shell, cx| {
                shell.close_card(window, cx);
                shell.show_chats(window, cx);
            });
            true
        }
        "more-apps" | "more-apps-close" => {
            shell.update(cx, Shell::toggle_more_apps);
            true
        }
        "workflows-app" => {
            shell.update(cx, |shell, cx| shell.resume_workflows(window, cx));
            true
        }
        "channel-menu" => made.is_some_and(|made| channel_menu(shell, &made.channel, window, cx)),
        "menu-close" => {
            shell.update(cx, |shell, cx| shell.show_chats(window, cx));
            true
        }
        "message-menu" => {
            let chat = made.map_or(AC_CHAT, |made| {
                if shell.read(cx).account() == LAB_A {
                    AC_CHAT
                } else {
                    made.group.as_str()
                }
            });
            message_menu(shell, chat, window, cx)
        }
        "message-menu-close" => {
            if let Some(chat) = shell.read(cx).chat().cloned()
                && let Some(open) = chat.read(cx).options().map(str::to_owned)
            {
                chat.update(cx, |chat, cx| chat.toggle_options(&open, cx));
            }
            true
        }
        "manage-team-apps" => made.is_some_and(|made| {
            let listed = shell
                .read(cx)
                .teams()
                .read(cx)
                .teams()
                .is_some_and(|teams| teams.iter().any(|team| team.id == made.team));
            if listed {
                // as the team's menu opens it, then the Apps tab asked for
                shell.update(cx, |shell, cx| {
                    shell.open_manage_team(made.team.clone(), ManageTab::Channels, window, cx);
                });
                if let Some(page) = shell.read(cx).manage_team().cloned() {
                    page.update(cx, |page, cx| page.show_tab(ManageTab::Apps, cx));
                }
            }
            listed && shell.read(cx).manage_team().is_some()
        }),
        "meeting-recap" => made.is_some_and(|made| {
            if shell.read(cx).list().read(cx).row(&made.meeting).is_none() {
                return false;
            }
            shell.update(cx, |shell, cx| {
                shell.open_chat(made.meeting.clone(), window, cx);
                shell.show_chat_tab(ChatTab::Recap, window, cx);
            });
            true
        }),
        "card-close" => {
            shell.update(cx, |shell, cx| shell.close_card(window, cx));
            true
        }
        _ => true,
    }
}

/// The call steps: a call made and answered, and its end.
fn act_call(step: &str, shell: &Entity<Shell>, made: Option<&Made>, cx: &mut App) -> bool {
    match step {
        "call" => {
            let caller = made.map_or(LAB_B, |made| {
                if shell.read(cx).account() == LAB_A {
                    LAB_B
                } else {
                    made.bert.as_str()
                }
            });
            let name = if caller == LAB_B { "Lab Bee" } else { BERT_NAME };
            fake_call(shell, caller, name, cx);
            true
        }
        "call-end" => {
            let models = shell.read(cx).models().clone();
            models.apply(
                teams_core::Event::Call {
                    call: teams_core::CallId::for_tests(FAKE_CALL),
                    status: teams_core::CallStatus::Ended(teams_core::End::HungUp),
                },
                cx,
            );
            true
        }
        _ => true,
    }
}

/// The title bar's search steps.
fn act_search(step: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> bool {
    let search = shell.read(cx).search().clone();
    match step {
        "search-suggestions" => {
            shell.update(cx, |shell, cx| shell.focus_search(SearchMode::Search, window, cx));
            search.update(cx, |search, cx| search.set_query("Ber", window, cx));
            true
        }
        "search-all" => results(shell, &search, "launch", Tab::All, window, cx),
        "search-person" => results(shell, &search, "Bert", Tab::People, window, cx),
        "search-message" => results(shell, &search, "checklist", Tab::Messages, window, cx),
        "search-chat" => results(shell, &search, "Launch QA", Tab::GroupChats, window, cx),
        "search-channel" => results(shell, &search, "Launch Plans", Tab::Channels, window, cx),
        "search-nothing" => results(shell, &search, "zzqx", Tab::All, window, cx),
        "search-message-open" => {
            let Some(hit) = search.read(cx).message_hits(cx).into_iter().next() else {
                return false;
            };
            shell.update(cx, |shell, cx| shell.open_message(hit.target, window, cx));
            true
        }
        "search-leave" => {
            search.update(cx, |search, cx| search.escape(window, cx));
            shell.update(cx, |shell, cx| shell.show_chats(window, cx));
            true
        }
        _ => true,
    }
}

/// The search for `query` entered, its results on `tab`.
fn results(
    shell: &Entity<Shell>,
    search: &Entity<teams_ui::GlobalSearch>,
    query: &str,
    tab: Tab,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    shell.update(cx, |shell, cx| shell.search_for(query, window, cx));
    search.update(cx, |search, cx| search.set_tab(tab, cx));
    true
}

/// Opens `channel` and its "…" menu, once the teams list has it.
fn channel_menu(shell: &Entity<Shell>, channel: &str, window: &mut Window, cx: &mut App) -> bool {
    let teams = shell.read(cx).teams().clone();
    let listed = teams
        .read(cx)
        .teams()
        .is_some_and(|teams| teams.iter().any(|team| team.channels.iter().any(|c| c.id == channel)));
    if !listed {
        return false;
    }
    shell.update(cx, |shell, cx| shell.open_channel_id(channel.to_owned(), window, cx));
    teams.update(cx, |teams, cx| teams.open_channel_menu(channel, window, cx));
    true
}

/// Opens `chat` and the more-options menu of its last message from someone else, once it shows one.
fn message_menu(shell: &Entity<Shell>, chat: &str, window: &mut Window, cx: &mut App) -> bool {
    let shown = shell
        .read(cx)
        .chat()
        .is_some_and(|view| view.read(cx).conversation() == chat);
    if !shown {
        shell.update(cx, |shell, cx| shell.open_chat(chat.to_owned(), window, cx));
    }
    let Some(view) = shell.read(cx).chat().cloned() else {
        return false;
    };
    let message = view.read(cx).items().iter().rev().find_map(|item| match item {
        Item::Message(message) if !message.own && !message.actions().is_empty() => Some(message.id.clone()),
        _ => None,
    });
    let Some(message) = message else {
        return false;
    };
    view.update(cx, |view, cx| {
        view.hover(Some(message.clone()), cx);
        view.toggle_options(&message, cx);
    });
    true
}

/// The fake call's id: a call the engine never made.
const FAKE_CALL: u64 = 188_001;

/// A call `caller` made and the account answered, as the engine would tell it: its window opens over the main one.
fn fake_call(shell: &Entity<Shell>, caller: &str, name: &str, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models
        .calls
        .update(cx, |calls, _| *calls = teams_ui::Calls::shown(None));
    let call = teams_core::CallId::for_tests(FAKE_CALL);
    let events = [
        teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(caller),
            caller_name: Some(name.to_owned()),
            group_chat: None,
            meeting: None,
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
}

/// Whether what `step` asked for is in: the search's answer, the suggestions.
fn settled(step: &str, window: WindowHandle<KitRoot>, cx: &mut App) -> bool {
    let Some(shell) = shell_of(window, cx) else {
        return true;
    };
    let search = shell.read(cx).search().read(cx);
    match step {
        "search-suggestions" => !search.suggestions().is_empty(),
        step if step.starts_with("search-") && step != "search-leave" && step != "search-message-open" => {
            search.results().is_some_and(|results| !results.loading())
        }
        _ => true,
    }
}

/// `key=value …` of what the window shows (no spaces in a value).
fn facts(window: WindowHandle<KitRoot>, cx: &mut App) -> String {
    let Some(shell) = shell_of(window, cx) else {
        return "shell=none".to_owned();
    };
    let shell = shell.read(cx);
    let caps = shell.list().read(cx).capabilities();
    let caps = format!(
        "workflows:{},recap:{},captions:{},recording:{},apps:{},copilot:{}",
        caps.workflows, caps.recap, caps.captions, caps.recording, caps.apps, caps.copilot
    );
    let search = shell.search().read(cx);
    let words = |text: &str| text.replace(' ', "_");
    let people = search
        .people()
        .iter()
        .map(|person| words(&person.name))
        .collect::<Vec<_>>()
        .join(",");
    let card = shell
        .profile_card()
        .and_then(|card| match card.read(cx).loaded() {
            teams_ui::profile_card::Loaded::Read(data) => data.name.clone(),
            _ => Some("loading".to_owned()),
        })
        .map(|name| words(&name))
        .unwrap_or_default();
    let options = shell
        .chat()
        .and_then(|chat| chat.read(cx).options().map(str::to_owned))
        .unwrap_or_default();
    format!(
        "account={} caps={caps} main={} workflows_app={} manage_tab={} chat_tab={:?} options={options} card={card} \
         suggestions={} hits={} people={people} chats={} channels={} files={}",
        shell.account(),
        words(&format!("{:?}", shell.main())),
        if matches!(shell.main(), teams_ui::shell::Main::Workflows) {
            "open"
        } else {
            "none"
        },
        shell
            .manage_team()
            .map_or_else(|| "none".to_owned(), |page| format!("{:?}", page.read(cx).tab())),
        shell.chat_tab(),
        search.suggestions().len(),
        search.message_hits(cx).len(),
        search.group_chats(cx).len(),
        search.channels(cx).len(),
        search.files().len(),
    )
}
