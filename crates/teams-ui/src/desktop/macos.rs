//! macOS's desktop, as Teams has it there: notifications through the system's notification center (GPUI's
//! `UNUserNotificationCenter` binding), the unread total on the Dock icon's badge, and the Dock in the tray's place:
//! closing the main window keeps the app in the Dock, and a click on its Dock icon shows the window again (the app
//! asks [`Desktop::show`] then). No D-Bus address is read or reached.
//!
//! - The notification center serves only an app started from its bundle (`ngteams.app`): a binary run on its own gets
//!   no notifications, said as a problem (GPUI's binding would abort the process on the center's first use).
//! - Each notification's tag is this run's nonce and its id, never a chat's id or its text: an old run's notification
//!   activated after a restart names no notification of this run and does nothing (its body still brings the app up).
//! - Answer, and a notification's body, bring the app to the front; Decline doesn't.
//! - The app's quit withdraws the notifications this run still shows ([`Notifications::withdraw_all`]).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use gpui::{App, AsyncApp, SystemNotification, SystemNotificationAction, SystemNotificationResponse, WeakEntity};

use super::Desktop;
use crate::notify::{ANSWER_ACTION, DECLINE_ACTION, DEFAULT_ACTION, Notice, NoticeKind, NotificationServer};
use crate::tray::TrayIcon;

/// What the notification server asks of the main thread, where GPUI's notification center lives.
enum Command {
    Show(SystemNotification),
    Close(String),
}

/// The app's notifications on macOS: [`NotificationServer`] over GPUI's system notifications.
pub(crate) struct Notifications {
    commands: async_channel::Sender<Command>,
    /// This run's nonce, the start of every tag.
    run: String,
    next: AtomicU32,
    /// The tags shown and not yet closed, withdrawn on the quit.
    shown: Mutex<BTreeSet<String>>,
}

impl Notifications {
    /// The tag of notification `id` of this run.
    fn tag(&self, id: u32) -> String {
        format!("{}-{id}", self.run)
    }

    /// The id of this run's notification `tag`; none for another run's.
    fn id(&self, tag: &str) -> Option<u32> {
        tag.strip_prefix(&self.run)?.strip_prefix('-')?.parse().ok()
    }

    fn shown(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Withdraws every notification this run still shows.
    pub(crate) fn withdraw_all(&self, cx: &App) {
        for tag in std::mem::take(&mut *self.shown()) {
            cx.dismiss_system_notification(&tag);
        }
    }
}

impl NotificationServer for Notifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let id = if replaces == 0 {
            self.next.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            replaces
        };
        let tag = self.tag(id);
        self.shown().insert(tag.clone());
        let notification = system_notification(notice, tag);
        let sent = self.commands.try_send(Command::Show(notification));
        Box::pin(async move {
            sent.map_err(|_| "the notification center is gone".to_owned())?;
            Ok(id)
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let tag = self.tag(id);
        if self.shown().remove(&tag) {
            let _ = self.commands.try_send(Command::Close(tag));
        }
        Box::pin(async {})
    }
}

/// `notice` as the system shows it, under `tag`: an incoming call with Answer and Decline. Its text is plain text.
fn system_notification(notice: &Notice, tag: String) -> SystemNotification {
    let actions = if notice.kind == NoticeKind::Call {
        vec![
            SystemNotificationAction {
                id: ANSWER_ACTION.into(),
                label: "Answer".into(),
            },
            SystemNotificationAction {
                id: DECLINE_ACTION.into(),
                label: "Decline".into(),
            },
        ]
    } else {
        Vec::new()
    };
    SystemNotification {
        tag: tag.into(),
        title: notice.summary.clone().into(),
        body: notice.body.clone().into(),
        actions,
    }
}

/// The action a response asks: its button's, or the body's.
fn action_of(response: &SystemNotificationResponse) -> Option<&str> {
    match response.action_id.as_deref() {
        None => Some(DEFAULT_ACTION),
        Some(action @ (ANSWER_ACTION | DECLINE_ACTION)) => Some(action),
        Some(_) => None,
    }
}

/// The run's nonce: the process and the time it started.
fn run_nonce() -> String {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("ngteams-{}-{started:x}", std::process::id())
}

/// Whether this process was started from an app bundle, where the notification center serves it.
fn in_bundle() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// The Dock icon's badge: the unread total, none at 0.
struct DockBadge;

impl TrayIcon for DockBadge {
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()> {
        badge(unread);
        Box::pin(async {})
    }
}

/// Shows `unread` on the Dock icon. The tray shows its total on the main thread; anywhere else nothing is shown
/// rather than `AppKit` used off it.
fn badge(unread: u64) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    let Some(main_thread) = MainThreadMarker::new() else {
        log::warn!("the Dock badge wasn't set: asked off the main thread");
        return;
    };
    let label = (unread > 0).then(|| NSString::from_str(&unread.to_string()));
    NSApplication::sharedApplication(main_thread)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

/// Puts the notifications and the Dock badge up; with `with_dock`, closing the main window keeps the app in the Dock.
/// Answers what failed.
pub(super) fn connect(desktop: &WeakEntity<Desktop>, with_dock: bool, cx: &mut AsyncApp) -> Vec<String> {
    let mut problems = Vec::new();
    let _ = desktop.update(cx, |this, cx| {
        this.tray.update(cx, |tray, cx| tray.set_icon(Arc::new(DockBadge), cx));
        if with_dock {
            this.set_in_tray(true, cx);
        }
    });
    if in_bundle() {
        notifications(desktop, cx);
    } else {
        problems.push("no notifications: macOS shows them only for the app started from ngteams.app".to_owned());
    }
    problems
}

/// The notification server handed to the desktop, its commands carried out on the main thread and its responses
/// handed back.
fn notifications(desktop: &WeakEntity<Desktop>, cx: &mut AsyncApp) {
    let (commands, queue) = async_channel::unbounded();
    let server = Arc::new(Notifications {
        commands,
        run: run_nonce(),
        next: AtomicU32::new(0),
        shown: Mutex::new(BTreeSet::new()),
    });
    let answering = Arc::clone(&server);
    let responding = desktop.clone();
    cx.update(|cx| {
        cx.on_system_notification_response(move |response, cx| {
            let (Some(id), Some(action)) = (answering.id(&response.tag), action_of(&response)) else {
                // another run's notification, or a button this run never offered: the app is brought up, nothing else
                cx.activate(true);
                return;
            };
            answering.shown().remove(response.tag.as_ref());
            if action != DECLINE_ACTION {
                cx.activate(true);
            }
            let _ = responding.update(cx, |this, cx| this.activated(id, action, cx));
        });
    });
    let _ = desktop.update(cx, |this, cx| {
        this.mac_notifications = Some(Arc::clone(&server));
        this.use_server(server, cx);
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(command) = queue.recv().await {
            cx.update(|cx| match command {
                Command::Show(notification) => cx.show_system_notification(notification),
                Command::Close(tag) => cx.dismiss_system_notification(&tag),
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt as _;

    fn server() -> (Notifications, async_channel::Receiver<Command>) {
        let (commands, queue) = async_channel::unbounded();
        let server = Notifications {
            commands,
            run: "ngteams-1-a".to_owned(),
            next: AtomicU32::new(0),
            shown: Mutex::new(BTreeSet::new()),
        };
        (server, queue)
    }

    fn notice(kind: NoticeKind) -> Notice {
        Notice {
            conversation: "19:chat".to_owned(),
            kind,
            summary: "Ada".to_owned(),
            body: "<b>hi</b>".to_owned(),
        }
    }

    #[test]
    fn tags_name_the_run_and_the_id_only() {
        let (server, queue) = server();
        let first = server
            .show(&notice(NoticeKind::Message), 0)
            .now_or_never()
            .expect("ready");
        assert_eq!(first, Ok(1));
        let Ok(Command::Show(shown)) = queue.try_recv() else {
            panic!("nothing shown")
        };
        assert_eq!(shown.tag.as_ref(), "ngteams-1-a-1");
        assert_eq!((shown.title.as_ref(), shown.body.as_ref()), ("Ada", "<b>hi</b>"));
        assert!(shown.actions.is_empty());
        assert_eq!(server.id("ngteams-1-a-1"), Some(1));
        assert_eq!(server.id("ngteams-2-b-1"), None);
        assert_eq!(server.id("ngteams-1-a-x"), None);
        // a replacement keeps its id
        let again = server
            .show(&notice(NoticeKind::Collapsed), 1)
            .now_or_never()
            .expect("ready");
        assert_eq!(again, Ok(1));
    }

    #[test]
    fn a_call_offers_answer_and_decline() {
        let (server, queue) = server();
        let _ = server.show(&notice(NoticeKind::Call), 0).now_or_never().expect("ready");
        let Ok(Command::Show(shown)) = queue.try_recv() else {
            panic!("nothing shown")
        };
        let ids: Vec<&str> = shown.actions.iter().map(|action| action.id.as_ref()).collect();
        assert_eq!(ids, [ANSWER_ACTION, DECLINE_ACTION]);
    }

    #[test]
    fn closing_withdraws_only_what_is_shown() {
        let (server, queue) = server();
        let _ = server
            .show(&notice(NoticeKind::Message), 0)
            .now_or_never()
            .expect("ready");
        let _ = queue.try_recv();
        server.close(1).now_or_never().expect("ready");
        assert!(matches!(queue.try_recv(), Ok(Command::Close(tag)) if tag == "ngteams-1-a-1"));
        server.close(1).now_or_never().expect("ready");
        assert!(queue.try_recv().is_err());
    }

    #[test]
    fn responses_ask_known_actions_only() {
        let response = |action: Option<&str>| SystemNotificationResponse {
            tag: "t".into(),
            action_id: action.map(Into::into),
        };
        assert_eq!(action_of(&response(None)), Some(DEFAULT_ACTION));
        assert_eq!(action_of(&response(Some(ANSWER_ACTION))), Some(ANSWER_ACTION));
        assert_eq!(action_of(&response(Some(DECLINE_ACTION))), Some(DECLINE_ACTION));
        assert_eq!(action_of(&response(Some("other"))), None);
    }

    #[test]
    fn a_run_nonce_names_the_process() {
        assert!(run_nonce().starts_with(&format!("ngteams-{}-", std::process::id())));
    }
}
