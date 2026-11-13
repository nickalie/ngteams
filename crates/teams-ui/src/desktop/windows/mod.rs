//! Native shell integration. No D-Bus fallback: toasts and the tray each own a native thread.
//! IDs are allocated across accounts by the single toast center; activation returns only an ID/action to Desktop.
//! Quitting removes this run's notifications, so a later launch never offers stale call actions.
mod activation;
pub(crate) mod registration;
pub(crate) mod toasts;
pub(crate) mod tray;

use super::Desktop;
use crate::notify::BannerFiles;
use gpui::{AsyncApp, WeakEntity};
use std::sync::Arc;

pub(super) async fn connect(
    desktop: WeakEntity<Desktop>,
    app_id: String,
    app_name: String,
    with_tray: bool,
    files: Option<BannerFiles>,
    cx: &mut AsyncApp,
) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(error) = connect_notifications(desktop.clone(), app_id, app_name.clone(), files, cx).await {
        problems.push(format!("no notifications: {error}"));
    }
    if with_tray && let Err(error) = connect_tray(desktop, app_name, cx).await {
        problems.push(format!("no tray item: {error}"));
    }
    problems
}
async fn connect_notifications(
    desktop: WeakEntity<Desktop>,
    app_id: String,
    name: String,
    files: Option<BannerFiles>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let (server, events) = toasts::Notifications::start(app_id, name, files).await?;
    let server = Arc::new(server);
    let held = Arc::clone(&server);
    cx.update(|cx| {
        cx.on_app_quit(move |_| {
            held.stop();
            async {}
        })
        .detach();
    });
    let _ = desktop.update(cx, |this, cx| this.use_server(server, cx));
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok((id, action)) = events.recv().await {
            if desktop.update(cx, |this, cx| this.activated(id, &action, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Ok(())
}
async fn connect_tray(desktop: WeakEntity<Desktop>, name: String, cx: &mut AsyncApp) -> Result<(), String> {
    let (item, events) = tray::NativeTray::start(name).await?;
    let item = Arc::new(item);
    let held = Arc::clone(&item);
    cx.update(|cx| {
        cx.on_app_quit(move |_| {
            held.stop();
            async {}
        })
        .detach();
    });
    let _ = desktop.update(cx, |this, cx| {
        this.native_tray = Some(Arc::clone(&item));
        this.sync_native_window(cx);
        this.tray.update(cx, |tray, cx| tray.set_icon(item, cx));
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(event) = events.recv().await {
            if desktop
                .update(cx, |this, cx| match event {
                    tray::Event::Asked(event) => this.tray_asked(event, cx),
                    tray::Event::Hosted(up) => this.set_in_tray(up, cx),
                })
                .is_err()
            {
                break;
            }
        }
        let _ = desktop.update(cx, |this, cx| this.set_in_tray(false, cx));
    })
    .detach();
    Ok(())
}
mod apartment;
use apartment::Apartment;
