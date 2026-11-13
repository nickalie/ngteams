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
//! - A notice's picture is an attachment: a PNG the app writes in its banner folder ([`BannerFiles`]), named by the
//!   notification's tag, which the notification center takes in; whatever is left is removed when the notification
//!   closes and at the quit. GPUI's binding takes no attachment, so the app posts the notifications itself through the
//!   same notification center ([`Poster`]); GPUI's delegate still hands back each response and keeps banners shown
//!   while the app is in front.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use gpui::{App, AsyncApp, SystemNotification, SystemNotificationAction, SystemNotificationResponse, WeakEntity};

use super::Desktop;
use crate::notify::{
    ANSWER_ACTION, BannerFiles, DECLINE_ACTION, DEFAULT_ACTION, Notice, NoticeKind, NotificationServer,
};
use crate::tray::TrayIcon;

/// What the notification server asks of the main thread, where the notification center is used.
enum Command {
    Show(Banner),
    Close(String),
}

/// A notification as it is posted: what it says and offers, and its picture's file.
struct Banner {
    notification: SystemNotification,
    picture: Option<PathBuf>,
}

/// The app's notifications on macOS: [`NotificationServer`] over GPUI's system notifications.
pub(crate) struct Notifications {
    commands: async_channel::Sender<Command>,
    /// This run's nonce, the start of every tag.
    run: String,
    next: AtomicU32,
    /// The tags shown and not yet closed, withdrawn on the quit.
    shown: Mutex<BTreeSet<String>>,
    /// Where the notifications' pictures are written; none shows them without.
    files: Option<BannerFiles>,
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
        if let Some(files) = &self.files {
            files.clear();
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
        let picture = notice
            .picture
            .as_ref()
            .and_then(|picture| self.files.as_ref()?.write(&tag, picture));
        let notification = system_notification(notice, tag);
        let sent = self.commands.try_send(Command::Show(Banner { notification, picture }));
        Box::pin(async move {
            sent.map_err(|_| "the notification center is gone".to_owned())?;
            Ok(id)
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let tag = self.tag(id);
        if self.shown().remove(&tag) {
            if let Some(files) = &self.files {
                files.remove(&tag);
            }
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

/// Puts the notifications (their pictures written to `files`) and the Dock badge up; with `with_dock`, closing the
/// main window keeps the app in the Dock. Answers what failed.
pub(super) fn connect(
    desktop: &WeakEntity<Desktop>,
    with_dock: bool,
    files: Option<BannerFiles>,
    cx: &mut AsyncApp,
) -> Vec<String> {
    let mut problems = Vec::new();
    let _ = desktop.update(cx, |this, cx| {
        this.tray.update(cx, |tray, cx| tray.set_icon(Arc::new(DockBadge), cx));
        if with_dock {
            this.set_in_tray(true, cx);
        }
    });
    if in_bundle() {
        notifications(desktop, files, cx);
    } else {
        problems.push("no notifications: macOS shows them only for the app started from ngteams.app".to_owned());
    }
    problems
}

/// The notification server handed to the desktop, its commands carried out on the main thread and its responses
/// handed back.
fn notifications(desktop: &WeakEntity<Desktop>, files: Option<BannerFiles>, cx: &mut AsyncApp) {
    let (commands, queue) = async_channel::unbounded();
    let server = Arc::new(Notifications {
        commands,
        run: run_nonce(),
        next: AtomicU32::new(0),
        shown: Mutex::new(BTreeSet::new()),
        files,
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
        // the center is reached only once GPUI's delegate is set (the response callback above) and on the main thread
        let mut poster = Poster::default();
        while let Ok(command) = queue.recv().await {
            cx.update(|cx| match command {
                Command::Show(banner) => poster.post(&banner),
                Command::Close(tag) => cx.dismiss_system_notification(&tag),
            });
        }
    })
    .detach();
}

/// The category of a notification with Answer and Decline: an incoming call's.
const CALL_CATEGORY: &str = "ngteams-call";

/// Posts the app's notifications to the system's notification center, as GPUI's binding does, with a picture as an
/// attachment. Used on the main thread only.
#[derive(Default)]
struct Poster {
    /// The user was asked to allow notifications (once a run, at the first).
    asked: bool,
    /// The call category was registered.
    call_category: bool,
}

impl Poster {
    fn post(&mut self, banner: &Banner) {
        use block2::RcBlock;
        use objc2_foundation::{NSArray, NSError, NSString};
        use objc2_user_notifications::{UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter};

        let center = UNUserNotificationCenter::currentNotificationCenter();
        self.ask(&center);
        let notification = &banner.notification;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        if !notification.actions.is_empty() {
            self.register_calls(&center, &notification.actions);
            content.setCategoryIdentifier(&NSString::from_str(CALL_CATEGORY));
        }
        if let Some(attachment) = banner.picture.as_deref().and_then(attachment) {
            content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
        }
        // a nil trigger delivers at once; the tag as the identifier lets a newer notification replace it
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&notification.tag),
            &content,
            None,
        );
        let delivered = RcBlock::new(|error: *mut NSError| {
            // SAFETY: when non-null, `error` is a valid `NSError` for the callback's duration
            if let Some(error) = unsafe { error.as_ref() } {
                log::warn!("a notification wasn't delivered: {}", error.localizedDescription());
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&delivered));
    }

    /// Asks the user to allow banners and sounds, once.
    fn ask(&mut self, center: &objc2_user_notifications::UNUserNotificationCenter) {
        use block2::RcBlock;
        use objc2::runtime::Bool;
        use objc2_foundation::NSError;
        use objc2_user_notifications::UNAuthorizationOptions;

        if std::mem::replace(&mut self.asked, true) {
            return;
        }
        let answered = RcBlock::new(|granted: Bool, error: *mut NSError| {
            // SAFETY: when non-null, `error` is a valid `NSError` for the callback's duration
            if let Some(error) = unsafe { error.as_ref() } {
                log::warn!("notifications weren't allowed: {}", error.localizedDescription());
            } else if !granted.as_bool() {
                log::info!("notifications aren't allowed");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &answered,
        );
    }

    /// Registers the call category with `actions` (Answer and Decline), once: the app's only category.
    fn register_calls(
        &mut self,
        center: &objc2_user_notifications::UNUserNotificationCenter,
        actions: &[SystemNotificationAction],
    ) {
        use objc2_foundation::{NSArray, NSSet, NSString};
        use objc2_user_notifications::{
            UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
        };

        if std::mem::replace(&mut self.call_category, true) {
            return;
        }
        let actions: Vec<_> = actions
            .iter()
            .map(|action| {
                UNNotificationAction::actionWithIdentifier_title_options(
                    &NSString::from_str(&action.id),
                    &NSString::from_str(&action.label),
                    UNNotificationActionOptions::empty(),
                )
            })
            .collect();
        let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CALL_CATEGORY),
            &NSArray::from_retained_slice(&actions),
            &NSArray::new(),
            UNNotificationCategoryOptions::empty(),
        );
        center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));
    }
}

/// The picture at `path` as a notification's attachment; `None` (logged) when the center refuses it.
fn attachment(
    path: &std::path::Path,
) -> Option<objc2::rc::Retained<objc2_user_notifications::UNNotificationAttachment>> {
    use objc2_foundation::{NSString, NSURL};
    use objc2_user_notifications::UNNotificationAttachment;

    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
    // SAFETY: no options dictionary is passed, so none of the wrong type
    match unsafe {
        UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(&NSString::from_str("picture"), &url, None)
    } {
        Ok(attachment) => Some(attachment),
        Err(error) => {
            log::info!(
                "a notification's picture wasn't attached: {}",
                error.localizedDescription()
            );
            None
        }
    }
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
            files: None,
        };
        (server, queue)
    }

    fn notice(kind: NoticeKind) -> Notice {
        Notice {
            conversation: "19:chat".to_owned(),
            kind,
            summary: "Ada".to_owned(),
            body: "<b>hi</b>".to_owned(),
            picture: None,
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
        let Ok(Command::Show(Banner {
            notification: shown, ..
        })) = queue.try_recv()
        else {
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
        let Ok(Command::Show(Banner {
            notification: shown, ..
        })) = queue.try_recv()
        else {
            panic!("nothing shown")
        };
        let ids: Vec<&str> = shown.actions.iter().map(|action| action.id.as_ref()).collect();
        assert_eq!(ids, [ANSWER_ACTION, DECLINE_ACTION]);
    }

    #[test]
    fn an_app_activity_item_shows_as_a_plain_banner() {
        let (server, queue) = server();
        let id = server
            .show(&notice(NoticeKind::Activity), 0)
            .now_or_never()
            .expect("ready");
        let Ok(Command::Show(Banner {
            notification: shown, ..
        })) = queue.try_recv()
        else {
            panic!("nothing shown")
        };
        assert_eq!((shown.title.as_ref(), shown.body.as_ref()), ("Ada", "<b>hi</b>"));
        assert!(shown.actions.is_empty(), "its body opens it, as a chat's banner");
        assert_eq!(id.ok().and_then(|id| server.id(&server.tag(id))), Some(1));
    }

    #[test]
    fn a_picture_is_written_by_its_tag_and_removed_with_its_notification() {
        let data = tempfile::tempdir().expect("temp dir");
        let (mut server, queue) = server();
        server.files = BannerFiles::open(data.path());
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoded");
        let tile = Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png,
        });
        let pictured = Notice {
            picture: crate::notify::BannerPicture::of_app(&tile),
            ..notice(NoticeKind::Activity)
        };
        let _ = server.show(&pictured, 0).now_or_never().expect("ready");
        let Ok(Command::Show(Banner { picture, .. })) = queue.try_recv() else {
            panic!("nothing shown")
        };
        let picture = picture.expect("a picture");
        assert_eq!(
            picture.file_name().and_then(|name| name.to_str()),
            Some("ngteams-1-a-1.png")
        );
        assert!(picture.exists());
        server.close(1).now_or_never().expect("ready");
        assert!(!picture.exists());
        assert!(matches!(queue.try_recv(), Ok(Command::Close(_))));

        // one without a picture names no file
        let _ = server
            .show(&notice(NoticeKind::Message), 0)
            .now_or_never()
            .expect("ready");
        let Ok(Command::Show(Banner { picture, .. })) = queue.try_recv() else {
            panic!("nothing shown")
        };
        assert!(picture.is_none());
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
