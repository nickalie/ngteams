//! The engine thread: its own `GMainContext`, pushed thread-default before any `WebKit` object exists (`WebKit` binds
//! its main run loop to the thread and context that first use it), the display, the stores and the pages, and the
//! questions held open for the consumer.
//!
//! Everything here lives on that thread. The state sits in a thread-local [`Engine`]; every borrow of it is short and
//! never spans a `WebKit` call, since `WebKit` may emit signals from inside one.

mod ime;
mod interaction;
mod nix;
mod page;
mod platform;
mod spares;
mod webkit;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;

use async_channel::{Receiver, Sender};
use gio::prelude::*;

use self::page::Page;
use self::platform::Display;
use self::webkit::{
    Download, JscContext, NetworkSession, PolicyDecision, ScriptReply, UriSchemeRequest, WebContext, WebView,
};
use crate::config::{FrameMode, HostConfig};
use crate::event::{
    Event, MenuToken, PolicyToken, ReplyToken, SchemeError, SchemeRequest, SchemeResponse, SchemeToken,
};
use crate::host::{Command, ViewCommand};
use crate::spec::{SessionId, SessionSpec, ViewId, ViewSpec};

/// A script message's reply and the script context its value is made in.
struct Parked {
    view: ViewId,
    reply: ScriptReply,
    context: JscContext,
}

struct Engine {
    display: Display,
    events: Sender<Event>,
    sessions: HashMap<SessionId, NetworkSession>,
    pages: HashMap<ViewId, Rc<Page>>,
    policies: HashMap<PolicyToken, (ViewId, PolicyDecision)>,
    replies: HashMap<ReplyToken, Parked>,
    schemes: HashMap<SchemeToken, (Option<ViewId>, UriSchemeRequest)>,
    /// The `<select>` popups shown, until answered or closed.
    menus: HashMap<MenuToken, interaction::OptionMenuObject>,
    next_token: u64,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

/// Runs `change` on the engine; `None` before it starts or after it ends.
fn with<R>(change: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    ENGINE.with(|engine| engine.borrow_mut().as_mut().map(change))
}

fn token(engine: &mut Engine) -> u64 {
    engine.next_token += 1;
    engine.next_token
}

fn send(events: &Sender<Event>, event: Event) {
    if events.try_send(event).is_err() {
        log::debug!("nobody listens to the WPE engine's events any more");
    }
}

/// The engine thread's body: starts the engine, tells `ready` how that went, and serves commands until
/// [`Command::Quit`], after which it tells `stopped`.
///
/// The thread then lives on, parked, for the life of the process: it is `WebKit`'s main thread, whose thread-local
/// teardown waits forever on the run loop it tears down, so it must never end. Its main loop does not run again: a
/// web process `WebKit` was still launching when the stop ended it ([`spares`]) has not yet sent its credentials
/// through the bubblewrap launcher's socket, and the launcher's watch on that socket, dispatched, aborts the whole
/// process (`g_error` in `IPC::Connection::remoteProcessID`, `Source/WebKit/Platform/IPC/glib/ConnectionGLib.cpp` at
/// `wpewebkit-2.54`).
pub(crate) fn run(
    config: &HostConfig,
    commands: &Receiver<Command>,
    events: Sender<Event>,
    ready: &SyncSender<Result<(), String>>,
    stopped: SyncSender<()>,
) {
    let context = glib::MainContext::new();
    let ran = context.with_thread_default(|| {
        let display = match start(config) {
            Ok(display) => display,
            Err(message) => {
                let _ = ready.send(Err(message));
                return false;
            }
        };
        interaction::watch_clipboard(&display, events.clone());
        ENGINE.with(|engine| {
            engine.replace(Some(Engine {
                display,
                events,
                sessions: HashMap::new(),
                pages: HashMap::new(),
                policies: HashMap::new(),
                replies: HashMap::new(),
                schemes: HashMap::new(),
                menus: HashMap::new(),
                next_token: 0,
            }));
        });
        let main_loop = glib::MainLoop::new(Some(&context), false);
        context.spawn_local(serve(commands.clone(), stopped, main_loop.clone()));
        let _ = ready.send(Ok(()));
        main_loop.run();
        true
    });
    match ran {
        Ok(true) => loop {
            std::thread::park();
        },
        Ok(false) => {}
        Err(err) => {
            let _ = ready.send(Err(format!(
                "the engine's main context could not be made the thread's default: {err}"
            )));
        }
    }
}

/// The display, connected, and the consumer's schemes registered.
fn start(config: &HostConfig) -> Result<Display, String> {
    let target = match &config.frames {
        FrameMode::DmaBuf(target) => Some(target.clone()),
        FrameMode::Shm => None,
    };
    let display = Display::connect(target)?;
    let context = WebContext::default().ok_or("WebKit has no default web context")?;
    #[cfg(coverage)]
    context.set_profiling_directory();
    for scheme in &config.schemes {
        context.register_uri_scheme(scheme);
    }
    context.add_read_only_paths(&nix::sandbox_paths());
    if let Some(extensions) = &config.extensions {
        context.set_web_process_extensions(&extensions.directory, &extensions.data, &extensions.sandbox_paths);
    }
    Ok(display)
}

/// Runs commands as they come, until [`Command::Quit`] or the host is gone; then stops the engine and its main loop,
/// in this one dispatch, before the loop could see a web process the stop ended ([`run`]).
async fn serve(commands: Receiver<Command>, stopped: SyncSender<()>, main_loop: glib::MainLoop) {
    while let Ok(command) = commands.recv().await {
        if matches!(command, Command::Quit) {
            break;
        }
        apply(command);
    }
    stop();
    main_loop.quit();
    let _ = stopped.send(());
}

fn apply(command: Command) {
    match command {
        Command::CreateSession(id, spec) => create_session(id, &spec),
        Command::DropSession(id) => {
            with(|engine| engine.sessions.remove(&id));
        }
        Command::ClearSession(id) => clear_session(id),
        Command::CreateView(id, spec) => create_view(id, &spec),
        Command::CloseView(id) => close_view(id),
        Command::View(id, command) => view_command(id, command),
        Command::Policy(token, allow) => decide_policy(token, allow),
        Command::Reply(token, reply) => reply_script_message(token, reply),
        Command::Scheme(token, response) => finish_scheme_request(token, &response),
        Command::Clipboard(text) => {
            if let Some(display) = with(|engine| engine.display.clone()) {
                interaction::set_clipboard(&display, &text);
            }
        }
        Command::OptionMenu(token, index) => interaction::choose_option(token, index),
        Command::Quit => {}
    }
}

/// Closes every page, ends its web process ([`Page::end_web_process`]) and the ones `WebKit` keeps with no page
/// (module `spares`), and drops the engine; the main loop stops right after ([`serve`]), so `WebKit` calls back nothing.
fn stop() {
    let pages = with(|engine| engine.pages.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
    for page in pages {
        close_view(page.id);
        page.end_web_process();
    }
    spares::end();
    let engine = ENGINE.with(RefCell::take);
    drop(engine);
}

// MARK: - stores

fn create_session(id: SessionId, spec: &SessionSpec) {
    let session = match spec {
        SessionSpec::Ephemeral => NetworkSession::ephemeral(),
        SessionSpec::Persistent { data_dir, cache_dir } => NetworkSession::persistent(data_dir, cache_dir),
        SessionSpec::Guarded { proxy, ignore_hosts } => {
            let Some(session) = NetworkSession::guarded(proxy, ignore_hosts) else {
                log::warn!("a guarded WPE store could not get its proxy: not made");
                return;
            };
            session
        }
    };
    session.connect_local("download-started", false, move |values| {
        if let Some(download) = webkit::object_arg::<Download>(values, 1) {
            download_started(id, &download);
        }
        None
    });
    with(|engine| engine.sessions.insert(id, session));
}

/// Nothing a page shows is saved to disk: every download is cancelled as it starts, and reported.
fn download_started(session: SessionId, download: &Download) {
    let uri = download.uri().unwrap_or_default();
    let web_view = download.web_view();
    download.cancel();
    with(|engine| {
        let view = page_of(engine, web_view.as_ref());
        send(&engine.events, Event::DownloadRefused { session, view, uri });
    });
}

fn clear_session(id: SessionId) {
    let Some((session, events)) = with(|engine| (engine.sessions.get(&id).cloned(), engine.events.clone())) else {
        return;
    };
    let Some(manager) = session.and_then(|session| session.website_data_manager()) else {
        send(
            &events,
            Event::SessionCleared {
                session: id,
                result: Err("no such browser store".to_owned()),
            },
        );
        return;
    };
    manager.clear_all(move |cleared| {
        let result = cleared.map_err(|err| err.message().to_owned());
        send(&events, Event::SessionCleared { session: id, result });
    });
}

// MARK: - pages

/// The page `web_view` belongs to, if it is one of the engine's.
fn page_of(engine: &Engine, web_view: Option<&WebView>) -> Option<ViewId> {
    let web_view = web_view?;
    engine
        .pages
        .values()
        .find(|page| page.web_view == *web_view)
        .map(|page| page.id)
}

fn create_view(id: ViewId, spec: &ViewSpec) {
    let Some((display, session, events)) = with(|engine| {
        (
            engine.display.clone(),
            engine.sessions.get(&spec.session).cloned(),
            engine.events.clone(),
        )
    }) else {
        return;
    };
    let built = session
        .ok_or_else(|| "no such browser store".to_owned())
        .and_then(|session| Page::new(id, spec, &display, &session, &events));
    match built {
        Ok(page) => {
            let page_id = page.web_view.page_id();
            with(|engine| engine.pages.insert(id, page));
            send(&events, Event::ViewCreated { view: id, page_id });
        }
        Err(message) => {
            log::warn!("a WPE page ({id:?}) could not be built: {message}");
            send(&events, Event::ViewFailed { view: id, message });
        }
    }
}

/// Closes page `id`, answering what it left open: navigations are ignored, replies refused, scheme reads failed.
fn close_view(id: ViewId) {
    let Some((page, policies, replies, schemes)) = with(|engine| {
        let page = engine.pages.remove(&id);
        let policies: Vec<PolicyDecision> = drain(&mut engine.policies, |(view, _)| *view == id)
            .into_iter()
            .map(|(_, decision)| decision)
            .collect();
        let replies: Vec<Parked> = drain(&mut engine.replies, |parked| parked.view == id);
        let schemes: Vec<UriSchemeRequest> = drain(&mut engine.schemes, |(view, _)| *view == Some(id))
            .into_iter()
            .map(|(_, request)| request)
            .collect();
        (page, policies, replies, schemes)
    }) else {
        return;
    };
    for decision in policies {
        decision.ignore();
    }
    for parked in replies {
        parked.reply.return_error_message("page closed");
    }
    for request in schemes {
        request.finish_error(&scheme_error(SchemeError::Failed, "page closed"));
    }
    if let Some(page) = page {
        page.close();
    }
}

/// Removes and returns the values of `map` that `matches` picks.
fn drain<K: Copy + Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>, matches: impl Fn(&V) -> bool) -> Vec<V> {
    let keys: Vec<K> = map
        .iter()
        .filter(|(_, value)| matches(value))
        .map(|(key, _)| *key)
        .collect();
    keys.into_iter().filter_map(|key| map.remove(&key)).collect()
}

fn view_command(id: ViewId, command: ViewCommand) {
    let page = with(|engine| engine.pages.get(&id).cloned()).flatten();
    match page {
        Some(page) => page.apply(command),
        None => {
            if let ViewCommand::Evaluate { reply, .. } = command {
                let _ = reply.try_send(Err("no such page".to_owned()));
            }
        }
    }
}

// MARK: - questions held open

/// Holds a policy decision for the consumer's answer; `None` once the engine has stopped.
fn hold_policy(view: ViewId, decision: &PolicyDecision) -> Option<PolicyToken> {
    with(|engine| {
        let token = PolicyToken(token(engine));
        engine.policies.insert(token, (view, decision.clone()));
        token
    })
}

fn decide_policy(token: PolicyToken, allow: bool) {
    let Some(Some((_, decision))) = with(|engine| engine.policies.remove(&token)) else {
        return;
    };
    if allow {
        decision.use_();
    } else {
        decision.ignore();
    }
}

/// Holds a script message's reply for the consumer's answer; gives it back once the engine has stopped.
fn hold_reply(view: ViewId, reply: ScriptReply, context: JscContext) -> Result<ReplyToken, ScriptReply> {
    ENGINE.with(|engine| match engine.borrow_mut().as_mut() {
        Some(engine) => {
            let token = ReplyToken(token(engine));
            engine.replies.insert(token, Parked { view, reply, context });
            Ok(token)
        }
        None => Err(reply),
    })
}

fn reply_script_message(token: ReplyToken, reply: Result<String, String>) {
    let Some(Some(parked)) = with(|engine| engine.replies.remove(&token)) else {
        return;
    };
    match reply {
        Ok(json) => match parked.context.value_from_json(&json) {
            Some(value) => parked.reply.return_value(&value),
            None => parked.reply.return_error_message("the reply is not JSON"),
        },
        Err(message) => parked.reply.return_error_message(&message),
    }
}

/// A read of one of the consumer's schemes: held open and reported.
fn scheme_requested(request: &UriSchemeRequest) {
    let uri = request.uri();
    let web_view = request.web_view();
    let held = with(|engine| {
        let view = page_of(engine, web_view.as_ref());
        let token = SchemeToken(token(engine));
        engine.schemes.insert(token, (view, request.clone()));
        send(&engine.events, Event::SchemeRequest(SchemeRequest { token, view, uri }));
    });
    if held.is_none() {
        request.finish_error(&scheme_error(SchemeError::Failed, "the engine has stopped"));
    }
}

fn finish_scheme_request(token: SchemeToken, response: &SchemeResponse) {
    let Some(Some((_, request))) = with(|engine| engine.schemes.remove(&token)) else {
        return;
    };
    match response {
        SchemeResponse::File(path) => serve_file(&request, path),
        SchemeResponse::Bytes { data, mime } => {
            let length = i64::try_from(data.len()).unwrap_or(-1);
            let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data.as_slice()));
            request.finish(stream.upcast_ref(), length, mime);
        }
        SchemeResponse::Error { kind, message } => request.finish_error(&scheme_error(*kind, message)),
    }
}

/// Streams the file at `path`, typed by its name.
fn serve_file(request: &UriSchemeRequest, path: &std::path::Path) {
    match gio::File::for_path(path).read(None::<&gio::Cancellable>) {
        Ok(stream) => {
            let length = std::fs::metadata(path)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok())
                .unwrap_or(-1);
            let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
            let mime = gio::content_type_get_mime_type(&content_type);
            request.finish(
                stream.upcast_ref(),
                length,
                mime.as_deref().unwrap_or("application/octet-stream"),
            );
        }
        Err(error) => request.finish_error(&error),
    }
}

fn scheme_error(kind: SchemeError, message: &str) -> glib::Error {
    let code = match kind {
        SchemeError::NotFound => gio::IOErrorEnum::NotFound,
        SchemeError::PermissionDenied => gio::IOErrorEnum::PermissionDenied,
        SchemeError::IsDirectory => gio::IOErrorEnum::IsDirectory,
        SchemeError::Failed => gio::IOErrorEnum::Failed,
    };
    glib::Error::new(code, message)
}
