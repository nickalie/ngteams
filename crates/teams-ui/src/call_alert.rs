//! The incoming call on the desktop (M3 Task 17): while an invitation the engine offered rings, its ringtone on the
//! default output ([`Ringer`]) and a notification with Answer and Decline on the bus the app is given, both gone once
//! the ring ends (answered, declined, cancelled, answered elsewhere, timed out) or the user answers it here. The
//! notification shows the caller's picture when the engine has it stored ([`crate::notify::picture`]). Answer and Decline on the notification
//! are the user's asks, as the banner's buttons are ([`calls::ask`]); its body shows the window, where the banner is.
//!
//! What it does is traced: `ringtone started`/`stopped` and `call-notification shown`/`closed`/`answered`/`declined`,
//! each with the call's number, never a name.

use std::sync::Arc;

use gpui::{AppContext as _, AsyncApp, Context, Entity, EventEmitter, Subscription};
use teams_core::{CallId, IncomingCall, Person};

use crate::bridge::Core;
use crate::calls::{self, Ask, Calls};
use crate::notify::{
    ANSWER_ACTION, BannerPicture, DECLINE_ACTION, DEFAULT_ACTION, Notice, NoticeKind, NotificationServer, SOMEONE,
};
use crate::ringtone::{Ringer, Ringtone};
use crate::trace::{TraceLine, ViewTrace};

/// What the alert asks of the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAlertEvent {
    /// Show the window: the notification's body was activated.
    ShowWindow,
}

/// The invitation alerted: its call, its notification once shown, its ringtone while it plays.
struct Alerting {
    call: CallId,
    /// The caller's MRI: their stored picture is the notification's.
    caller: String,
    notice: Notice,
    notification: Option<u32>,
    ringtone: Option<Ringtone>,
}

/// The incoming call's ringtone and notification (see the module docs). A GPUI entity the desktop holds.
pub struct CallAlert {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    trace: Arc<ViewTrace>,
    ringer: Option<Arc<dyn Ringer>>,
    server: Option<Arc<dyn NotificationServer>>,
    alerting: Option<Alerting>,
    _observing: Subscription,
}

impl EventEmitter<CallAlertEvent> for CallAlert {}

/// What an invitation's notification says: who calls, and whether it is a group call.
#[must_use]
pub fn notice_of(incoming: &IncomingCall) -> Notice {
    let caller = incoming.caller_name.as_deref().unwrap_or(SOMEONE);
    Notice {
        conversation: incoming.group_chat.clone().unwrap_or_default(),
        kind: NoticeKind::Call,
        summary: format!("{caller} is calling"),
        body: if incoming.group_chat.is_some() {
            "Incoming group call".to_owned()
        } else {
            "Incoming call".to_owned()
        },
        picture: None,
    }
}

impl CallAlert {
    /// The alert over `calls`, its ringtone played by `ringer` (none: no ringtone); it shows no notification until it
    /// is given a server ([`CallAlert::set_server`]).
    pub fn new(
        core: Arc<dyn Core>,
        calls: Entity<Calls>,
        trace: Arc<ViewTrace>,
        ringer: Option<Arc<dyn Ringer>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = cx.observe(&calls, |this: &mut Self, _, cx| this.follow(cx));
        let mut alert = Self {
            core,
            calls,
            trace,
            ringer,
            server: None,
            alerting: None,
            _observing: observing,
        };
        alert.follow(cx);
        alert
    }

    /// From now on, notifications go to `server`; an invitation already ringing is shown there now.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>, cx: &mut Context<Self>) {
        self.server = Some(server);
        if self
            .alerting
            .as_ref()
            .is_some_and(|alerting| alerting.notification.is_none())
        {
            self.notify(cx);
        }
    }

    /// The call the alert rings for, if one rings.
    #[must_use]
    pub fn alerting(&self) -> Option<CallId> {
        self.alerting.as_ref().map(|alerting| alerting.call)
    }

    /// Whether the ringtone plays.
    #[must_use]
    pub fn ringing_aloud(&self) -> bool {
        self.alerting
            .as_ref()
            .is_some_and(|alerting| alerting.ringtone.is_some())
    }

    /// The invitation that rings changed: the old one's alert stops, a new one's starts.
    fn follow(&mut self, cx: &mut Context<Self>) {
        // an answered invitation stops ringing at once, though its answer may wait for the devices
        let ringing = self.calls.read(cx).alerting().cloned();
        if self.alerting.as_ref().map(|alerting| alerting.call) == ringing.as_ref().map(|ringing| ringing.call) {
            return;
        }
        self.stop(cx);
        let Some(incoming) = ringing else { return };
        let account = self.core.me().to_owned();
        let in_call = self.calls.read(cx).call().is_some();
        let audible = self.ringer.as_ref().filter(|ringer| ringer.audible(&account, in_call));
        let ringtone = audible.map(|ringer| {
            self.trace
                .record(&TraceLine::new("ringtone", "started").call(incoming.call.get()));
            ringer.start()
        });
        self.alerting = Some(Alerting {
            call: incoming.call,
            caller: incoming.caller.as_str().to_owned(),
            notice: notice_of(&incoming),
            notification: None,
            ringtone,
        });
        self.notify(cx);
    }

    /// The ringtone stopped and the notification closed, of the invitation alerted.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(alerting) = self.alerting.take() else {
            return;
        };
        let call = alerting.call.get();
        if alerting.ringtone.is_some() {
            drop(alerting.ringtone);
            self.trace.record(&TraceLine::new("ringtone", "stopped").call(call));
        }
        if let (Some(id), Some(server)) = (alerting.notification, self.server.clone()) {
            self.trace
                .record(&TraceLine::new("call-notification", "closed").call(call));
            // a notification already gone is no matter
            cx.background_spawn(async move { server.close(id).await }).detach();
        }
    }

    /// Shows the notification of the invitation alerted, once there is a server.
    fn notify(&mut self, cx: &mut Context<Self>) {
        let (Some(server), Some(alerting)) = (self.server.clone(), self.alerting.as_ref()) else {
            return;
        };
        let (call, mut notice) = (alerting.call, alerting.notice.clone());
        // the caller as stored: their picture is never waited for from the network
        let caller = self.core.person(alerting.caller.clone());
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            if let Ok(Person {
                picture: Some(picture), ..
            }) = caller.await
            {
                notice.picture = cx
                    .background_executor()
                    .spawn(async move { BannerPicture::of_person(&picture) })
                    .await;
            }
            let Ok(id) = server.show(&notice, 0).await else {
                return;
            };
            let kept = this.update(cx, |this, _| match this.alerting.as_mut() {
                Some(alerting) if alerting.call == call && alerting.notification.is_none() => {
                    alerting.notification = Some(id);
                    this.trace
                        .record(&TraceLine::new("call-notification", "shown").call(call.get()));
                    true
                }
                _ => false,
            });
            if !kept.unwrap_or(false) {
                // its ring ended while it was being shown
                server.close(id).await;
            }
        })
        .detach();
    }

    /// Notification `id` had `action` invoked: Answer or Decline asked of the invitation it shows, or the window
    /// shown for its body. Whether it was this alert's.
    pub fn activated(&mut self, id: u32, action: &str, cx: &mut Context<Self>) -> bool {
        let Some(call) = self
            .alerting
            .as_ref()
            .filter(|alerting| alerting.notification == Some(id))
            .map(|alerting| alerting.call.get())
        else {
            return false;
        };
        let ask = match action {
            ANSWER_ACTION => Ask::Answer,
            DECLINE_ACTION => Ask::Decline,
            DEFAULT_ACTION => {
                cx.emit(CallAlertEvent::ShowWindow);
                return true;
            }
            _ => return true,
        };
        let change = if ask == Ask::Answer { "answered" } else { "declined" };
        self.trace
            .record(&TraceLine::new("call-notification", change).call(call));
        calls::ask(&self.calls, &self.core, &self.trace, ask, cx);
        true
    }
}
