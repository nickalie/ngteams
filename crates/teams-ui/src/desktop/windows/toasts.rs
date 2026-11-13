//! Live-process `WinRT` notifications, with explicit replacement, withdrawal and global account-safe IDs. A notice's
//! picture is a PNG in the app's banner folder ([`BannerFiles`]), named by the toast's key, in the app logo's place;
//! it is removed with its toast (closed, replaced, dismissed, or the run's stop).
use super::{Apartment, activation::Activator, registration};
use crate::notify::picture::{file_key, toast_image};
use crate::notify::{
    ANSWER_ACTION, BannerFiles, DECLINE_ACTION, DEFAULT_ACTION, Notice, NoticeKind, NotificationServer, escape_markup,
};
use futures_util::future::BoxFuture;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tokio::sync::oneshot;
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification,
        ToastNotificationManager, ToastNotifier,
    },
    core::HSTRING,
};

type Activation = (u32, String);
type Events = async_channel::Receiver<Activation>;
enum Command {
    Show(Notice, u32, oneshot::Sender<Result<u32, String>>),
    Close(u32),
    Forget(u32, Arc<AtomicBool>),
    Activate(String),
    Stop,
}
/// Commands are serialized on an MTA thread. The final owner explicitly stops it, regardless of callback senders.
pub(crate) struct Notifications {
    commands: mpsc::Sender<Command>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Notifications {
    pub(super) fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}
impl Drop for Notifications {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Notifications {
    /// The toasts of app `app_id` named `name`, their pictures written to `files` (none: toasts without pictures).
    pub(crate) async fn start(
        app_id: String,
        name: String,
        files: Option<BannerFiles>,
    ) -> Result<(Self, Events), String> {
        let (commands, queue) = mpsc::channel();
        let (events, activations) = async_channel::unbounded();
        let (ready, started) = oneshot::channel();
        // Cancellation before the frontend is returned must leave no detached native worker.
        let (activate, opened) = mpsc::channel();
        let callbacks = commands.clone();
        let thread = std::thread::Builder::new()
            .name("ngteams-toasts".into())
            .spawn(move || {
                let result = Apartment::new().map_err(|e| e.to_string()).and_then(|apartment| {
                    let activation_commands = callbacks.clone();
                    let activator = Activator::register(&app_id, &registration::activator_id(&app_id), move |args| {
                        let _ = activation_commands.send(Command::Activate(args));
                    })
                    .map_err(|e| e.to_string())?;
                    let registered = registration::register(&app_id, &name).map_err(|e| e.to_string())?;
                    let center = Center::new(app_id, callbacks, events, files).map_err(|e| e.to_string())?;
                    Ok((apartment, activator, registered, center))
                });
                match result {
                    Ok((_apartment, _activator, _registered, mut center)) => {
                        if ready.send(Ok(())).is_ok() && opened.recv().is_ok() {
                            center.run(&queue);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        started.await.map_err(|_| "toast thread stopped".to_owned())??;
        let frontend = Self {
            commands,
            thread: std::sync::Mutex::new(Some(thread)),
        };
        let _ = activate.send(());
        Ok((frontend, activations))
    }
}
impl NotificationServer for Notifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let (reply, result) = oneshot::channel();
        let sent = self.commands.send(Command::Show(notice.clone(), replaces, reply));
        Box::pin(async move {
            sent.map_err(|_| "toast thread stopped".to_owned())?;
            result.await.map_err(|_| "toast thread stopped".to_owned())?
        })
    }
    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let _ = self.commands.send(Command::Close(id));
        Box::pin(async {})
    }
}
struct Shown {
    toast: ToastNotification,
    live: Arc<AtomicBool>,
    key: String,
    /// Its picture's file, by its key in the banner folder.
    picture: Option<String>,
}
struct Center {
    app_id: HSTRING,
    // A private group per run; neither tenant IDs nor message text enter toast launch arguments/tags.
    group: HSTRING,
    notifier: ToastNotifier,
    commands: mpsc::Sender<Command>,
    events: async_channel::Sender<Activation>,
    shown: HashMap<u32, Shown>,
    next: u32,
    generation: u64,
    files: Option<BannerFiles>,
}
impl Center {
    fn new(
        app_id: String,
        commands: mpsc::Sender<Command>,
        events: async_channel::Sender<Activation>,
        files: Option<BannerFiles>,
    ) -> windows::core::Result<Self> {
        let app_id = HSTRING::from(app_id);
        Ok(Self {
            notifier: ToastNotificationManager::CreateToastNotifierWithId(&app_id)?,
            app_id,
            // A fresh run nonce prevents PID reuse or a crash from reviving an old call action.
            // SAFETY: CoCreateGuid returns a value and needs no caller-owned pointers.
            group: HSTRING::from(format!("run-{:?}", unsafe {
                windows::Win32::System::Com::CoCreateGuid()
            }?)),
            commands,
            events,
            shown: HashMap::new(),
            next: 0,
            generation: 0,
            files,
        })
    }
    fn run(&mut self, queue: &mpsc::Receiver<Command>) {
        while let Ok(command) = queue.recv() {
            match command {
                Command::Show(notice, replaces, reply) => {
                    let result = self
                        .show(&notice, replaces)
                        .map_err(|e| format!("Windows toast failed ({:#x})", e.code().0));
                    let _ = reply.send(result);
                }
                Command::Close(id) => self.close(id),
                Command::Forget(id, live) => {
                    if self.shown.get(&id).is_some_and(|shown| Arc::ptr_eq(&shown.live, &live))
                        && let Some(shown) = self.shown.remove(&id)
                    {
                        self.drop_picture(&shown);
                    }
                }
                Command::Activate(args) => self.activated(&args),
                Command::Stop => break,
            }
        }
        let ids: Vec<u32> = self.shown.keys().copied().collect();
        for id in ids {
            self.close(id);
        }
        if let Some(files) = &self.files {
            files.clear();
        }
    }

    /// Removes `shown`'s picture file, if it has one.
    fn drop_picture(&self, shown: &Shown) {
        if let (Some(files), Some(picture)) = (&self.files, &shown.picture) {
            files.remove(picture);
        }
    }
    fn show(&mut self, notice: &Notice, replaces: u32) -> windows::core::Result<u32> {
        let id = if replaces != 0 && self.shown.contains_key(&replaces) {
            replaces
        } else {
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_UNEXPECTED))?;
            self.next
        };
        let document = XmlDocument::new()?;
        self.generation += 1;
        let key = format!("{}-{}-{}", self.group, id, self.generation);
        let picture_key = file_key(&key);
        let picture = notice.picture.as_ref().and_then(|picture| {
            let path = self.files.as_ref()?.write(&picture_key, picture)?;
            Some(toast_image(&path, picture.shape()))
        });
        let loaded = document.LoadXml(&HSTRING::from(xml(notice, &key, picture.as_deref())));
        if picture.is_some()
            && loaded.is_err()
            && let Some(files) = &self.files
        {
            files.remove(&picture_key);
        }
        loaded?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        toast.SetGroup(&self.group)?;
        toast.SetTag(&HSTRING::from(id.to_string()))?;
        toast.SetExpiresOnReboot(true)?;
        let live = Arc::new(AtomicBool::new(true));
        self.handlers(&toast, id, &live)?;
        self.notifier.Show(&toast)?;
        let shown = Shown {
            toast,
            live,
            key,
            picture: picture.map(|_| picture_key),
        };
        if let Some(old) = self.shown.insert(id, shown) {
            old.live.store(false, Ordering::Release);
            self.drop_picture(&old);
        }
        Ok(id)
    }
    fn handlers(&self, toast: &ToastNotification, id: u32, live: &Arc<AtomicBool>) -> windows::core::Result<()> {
        let (active, commands) = (Arc::clone(live), self.commands.clone());
        toast.Failed(&TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
            move |_, args| {
                let code = args.as_ref().and_then(|args| args.ErrorCode().ok()).map(|code| code.0);
                log::warn!("Windows toast rejected ({code:x?})");
                #[cfg(test)]
                eprintln!("synthetic Windows toast rejected ({code:x?})");
                active.store(false, Ordering::Release);
                let _ = commands.send(Command::Forget(id, Arc::clone(&active)));
                Ok(())
            },
        ))?;
        let (active, commands) = (Arc::clone(live), self.commands.clone());
        toast.Dismissed(&TypedEventHandler::<ToastNotification, ToastDismissedEventArgs>::new(
            move |_, args| {
                if args.as_ref().and_then(|a| a.Reason().ok()) != Some(ToastDismissalReason::TimedOut) {
                    active.store(false, Ordering::Release);
                    let _ = commands.send(Command::Forget(id, Arc::clone(&active)));
                }
                Ok(())
            },
        ))?;
        Ok(())
    }
    fn activated(&mut self, args: &str) {
        let Some((key, action)) = args.rsplit_once(':') else {
            return;
        };
        if !valid_action(action) {
            return;
        }
        if let Some((&id, shown)) = self.shown.iter().find(|(_, shown)| shown.key == key)
            && shown.live.swap(false, Ordering::AcqRel)
        {
            let _ = self.events.try_send((id, action.into()));
        }
    }
    fn close(&mut self, id: u32) {
        if let Some(shown) = self.shown.remove(&id) {
            shown.live.store(false, Ordering::Release);
            let _ = self.notifier.Hide(&shown.toast);
            self.drop_picture(&shown);
            if let Ok(history) = ToastNotificationManager::History() {
                let _ = history.RemoveGroupedTagWithId(&HSTRING::from(id.to_string()), &self.group, &self.app_id);
            }
        }
    }
}
fn valid_action(action: &str) -> bool {
    matches!(action, DEFAULT_ACTION | ANSWER_ACTION | DECLINE_ACTION)
}

/// Only an opaque per-notice key and fixed actions are launch arguments. Display text is XML-escaped and remains plain
/// text; `picture` is the notice's picture's element ([`toast_image`]), when it has one.
fn xml(notice: &Notice, key: &str, picture: Option<&str>) -> String {
    let call = notice.kind == NoticeKind::Call;
    let scenario = if call {
        " scenario=\"incomingCall\" duration=\"long\" useButtonStyle=\"true\""
    } else {
        ""
    };
    let actions = if call {
        format!(
            r#"<actions><action content="Answer" hint-buttonStyle="Success" arguments="{key}:answer" activationType="foreground"/><action content="Decline" hint-buttonStyle="Critical" arguments="{key}:decline" activationType="foreground"/></actions>"#
        )
    } else {
        String::new()
    };
    // Message sounds follow Windows notification preferences; the app's ringer owns call audio.
    let audio = if call { r#"<audio silent="true"/>"# } else { "" };
    format!(
        r#"<toast launch="{key}:default"{scenario}><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>{}</binding></visual>{actions}{audio}</toast>"#,
        escape_markup(&notice.summary),
        escape_markup(&notice.body),
        picture.unwrap_or_default()
    )
}
