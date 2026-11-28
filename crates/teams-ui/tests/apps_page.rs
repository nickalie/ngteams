//! The rail's Store on GPUI's test context, over a fake core (`teams_ui::apps_page`): it shows only while the cloud
//! serves apps; its sections and search; an app's details and Add, which adds it for the account; "Add to a chat"
//! and its picker's Go; "Manage your apps" and a chat's apps, whose Remove asks first; nothing offered that writes in
//! the read-only mode. A bot's chat with the bot its id names, by its app's name and tile, its intro, About and typing
//! line; a group chat's picker offering its apps' bots and the bot's mention it sends.
//!
//! Every app, chat and id here is made up.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use teams_core::sync::CatchUpReport;
use teams_core::{AppListing, Cause, Event, TeamWritten};
use teams_proto::{StoreApp, StoreSections, TeamWrite};
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::ViewTrace;
use teams_ui::apps_page::AppsPage;
use teams_ui::apps_page::chat_apps::ChatAppsDialog;
use teams_ui::apps_page::details::AppDetailsDialog;
use teams_ui::chat_view::MessageAction;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};

const HELPER: &str = "00000000-0000-4000-8000-00000000a004";
const POLLS: &str = "00000000-0000-4000-8000-00000000a005";
const GROUP_CHAT: &str = "19:made-up-group@thread.v2";
const POLLS_TILE: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-polls_largeImage.png";
const BOT_CHAT: &str = "19:made-up-helper-chat@unq.gbl.spaces";

fn apps() -> Vec<StoreApp> {
    teams_proto::AppDefinitions::read_answer(
        json!([
            {"id": HELPER, "name": "Made-up Helper", "developerName": "Made-up Maker", "shortDescription": "Helps",
             "version": "1.0.0", "permissions": ["Identity"],
             "bots": [{"id": "b4", "scopes": ["personal"]}], "staticTabs": [{"entityId": "home"}]},
            {"id": POLLS, "name": "Made-up Polls", "developerName": "Made-up Maker", "shortDescription": "Polls",
             "largeImageUrl": POLLS_TILE,
             "bots": [{"id": "b5", "scopes": ["groupChat", "team"]}]},
        ])
        .to_string()
        .as_bytes(),
    )
}

fn listing() -> AppListing {
    let sections = StoreSections::read(
        json!({"appCategoriesDictionary": {
            "adminPreInstalledApps": {"apps": [{"id": HELPER, "rating": {"count": 1200, "average": 4.3}}]},
            "popularApps": {"apps": [{"id": HELPER}, {"id": POLLS}]},
        }})
        .to_string()
        .as_bytes(),
    );
    AppListing { sections, apps: apps() }
}

fn app(id: &str) -> StoreApp {
    apps()
        .into_iter()
        .find(|store| store.app.id == id)
        .expect("a made-up app")
}

/// The cloud of an account that serves neither apps nor outgoing webhooks.
const NO_APPS: teams_core::Capabilities = teams_core::Capabilities {
    apps: false,
    outgoing_webhooks: false,
    ..teams_core::Capabilities::ALL
};

/// The shell, with the account's cloud serving apps or not (`apps`).
fn shell_window(rig: &Rig, apps: bool) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_window_with(rig, apps, Vec::new())
}

fn shell_window_with(
    rig: &Rig,
    apps: bool,
    more: Vec<Conversation>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    if !apps {
        *rig.core.capabilities.lock().unwrap() = Some(NO_APPS);
    }
    *rig.core.app_listing.lock().unwrap() = listing();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let mut conversations = vec![Conversation {
        id: GROUP_CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: Some("Made-up group".to_owned()),
        version: 1,
        last_activity: Some(100),
    }];
    conversations.extend(more);
    *rig.core.conversations.lock().unwrap() = conversations;
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (shell, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn writes(rig: &Rig) -> Vec<TeamWrite> {
    rig.core
        .team_writes
        .lock()
        .unwrap()
        .iter()
        .map(|(write, _)| write.clone())
        .collect()
}

fn open_store(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    click(cx, "rail-apps");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

#[test]
fn the_store_shows_only_while_the_cloud_serves_apps() {
    let rig = Rig::start();
    let (_, cx) = shell_window(&rig, false);
    assert!(bounds(cx, "rail-apps").is_none());
    assert!(bounds(cx, "rail-chats").is_some());
}

/// An own server that does not name `appsService` gets no Store, no app details and no chat's apps, as with Workflows.
#[test]
fn an_own_server_without_apps_offers_no_store_and_no_chat_apps() {
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    assert!(bounds(cx, "rail-apps").is_none(), "no Store on the rail");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_apps(window, cx);
            shell.open_app_details(app(HELPER), window, cx);
            shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx);
            shell.open_chat(GROUP_CHAT.to_owned(), window, cx);
        });
    });
    cx.run_until_parked();
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
    assert!(cx.update(|_, cx| shell.read(cx).apps().is_none()));
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(bounds(cx, "chat-header").is_some());
    assert!(bounds(cx, "chat-apps").is_none(), "no Apps in this chat");
    *rig.core.capabilities.lock().unwrap() = None;
    cx.update(|_, cx| shell.update(cx, |_, cx| cx.notify()));
    assert!(
        bounds(cx, "rail-apps").is_some(),
        "the Store once the server serves apps"
    );
    assert!(bounds(cx, "chat-apps").is_some());
}

#[test]
fn the_store_lists_the_org_s_and_popular_apps_and_searches_them() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    assert!(bounds(cx, "apps-org").is_some(), "Added by your org");
    assert!(bounds(cx, "apps-org-0").is_some());
    assert!(bounds(cx, "apps-tile-popular-1").is_some());
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    cx.update(|window, cx| page.update(cx, |page, cx| page.set_search("polls", window, cx)));
    assert!(bounds(cx, "apps-results").is_some());
    assert!(bounds(cx, "apps-tile-found-0").is_some());
    assert!(bounds(cx, "apps-tile-found-1").is_none(), "only the app named so");
    cx.update(|window, cx| page.update(cx, |page, cx| page.set_search("nothing like it", window, cx)));
    assert!(bounds(cx, "apps-none").is_some());
    click(cx, "rail-chats");
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

/// A section with more apps than its row shows (Teams' "Popular on Teams" holds up to a hundred) has "See all", which
/// shows every one of them, and a way back.
#[test]
fn see_all_shows_every_app_of_a_section() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    let many: Vec<serde_json::Value> = (0..30)
        .map(|at| json!({"id": format!("made-up-{at:02}"), "name": format!("Made-up App {at:02}")}))
        .collect();
    let apps = teams_proto::AppDefinitions::read_answer(serde_json::Value::Array(many).to_string().as_bytes());
    let popular: Vec<serde_json::Value> = apps.iter().map(|store| json!({"id": store.app.id})).collect();
    let sections = StoreSections::read(
        json!({"appCategoriesDictionary": {
            "adminPreInstalledApps": {"apps": [{"id": HELPER}]},
            "popularApps": {"apps": popular},
        }})
        .to_string()
        .as_bytes(),
    );
    let mut all = apps;
    all.extend(self::apps());
    *rig.core.app_listing.lock().unwrap() = AppListing { sections, apps: all };
    open_store(&shell, cx);
    assert!(bounds(cx, "apps-tile-popular-23").is_some());
    assert!(bounds(cx, "apps-tile-popular-24").is_none(), "the row shows 24");
    assert!(
        bounds(cx, "apps-org-see-all").is_some(),
        "as Teams, every section has it"
    );
    click(cx, "apps-popular-see-all");
    assert!(bounds(cx, "apps-all").is_some());
    assert!(bounds(cx, "apps-popular").is_none());
    assert!(bounds(cx, "apps-tile-all-29").is_some(), "the 30th app");
    click(cx, "apps-tile-all-0");
    assert!(bounds(cx, "app-details").is_some());
    click(cx, "app-details-close");
    click(cx, "apps-all-back");
    assert!(bounds(cx, "apps-popular").is_some());
    assert!(bounds(cx, "apps-all").is_none());
}

/// An app nobody rated keeps the facts row, "Works in Teams" too, with "No ratings yet", as its tile says.
#[test]
fn an_unrated_app_s_details_say_no_ratings_yet() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-tile-popular-1");
    assert!(bounds(cx, "app-details").is_some());
    assert!(bounds(cx, "app-details-stats").is_some(), "the facts row");
    assert!(bounds(cx, "app-details-works-in").is_some());
    assert!(bounds(cx, "app-details-no-ratings").is_some());
    assert!(bounds(cx, "app-details-rating").is_none());
    click(cx, "app-details-close");
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details-rating").is_some(), "a rated app's rating");
    assert!(bounds(cx, "app-details-no-ratings").is_none());
}

#[test]
fn an_app_s_details_add_it_for_the_account_with_its_definition() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details").is_some());
    assert!(bounds(cx, "app-details-capabilities").is_some());
    assert!(bounds(cx, "app-details-permissions").is_some());
    assert!(bounds(cx, "app-details-stats").is_some(), "its rating");
    assert!(
        bounds(cx, "app-details-add-chat").is_none(),
        "a personal app goes nowhere else"
    );
    click(cx, "app-details-add");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::AddPersonalApp { app }] if app.id() == HELPER),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "app-details-added").is_some(), "the app is added");
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    click(cx, "app-details-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(
        bounds(cx, "apps-added-check").is_some(),
        "the store reads the account's apps again"
    );
}

#[test]
fn an_added_bot_app_s_details_open_the_chat_with_its_bot() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    rig.core.members.lock().unwrap().insert(
        BOT_CHAT.to_owned(),
        vec![Member::new(&rig.core.me, None), Member::new("28:b4", None)],
    );
    let bot_chat = Conversation {
        id: BOT_CHAT.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(50),
    };
    let (shell, cx) = shell_window_with(&rig, true, vec![bot_chat]);
    // a catch-up drops the list's built rows: the bot's chat is still found while they are built again
    cx.update(|_, cx| {
        rig.models.apply(
            Event::CaughtUp {
                cause: Cause::Connected,
                report: CatchUpReport::default(),
            },
            cx,
        );
    });
    assert_eq!(
        cx.update(|_, cx| shell
            .read(cx)
            .list()
            .read(cx)
            .one_to_one_with("28:b4")
            .map(str::to_owned)),
        Some(BOT_CHAT.to_owned())
    );
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details-added").is_none());
    click(cx, "app-details-open");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).list().read(cx).selected().map(str::to_owned)),
        Some(BOT_CHAT.to_owned())
    );
}

#[test]
fn an_add_before_add_to_a_chat_shows_the_app_added_in_the_store() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    click(cx, "app-details-add");
    assert!(bounds(cx, "app-details-added").is_some());
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.add_to(false, cx)));
    cx.run_until_parked();
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    assert!(
        cx.update(|_, cx| page.read(cx).added(HELPER)),
        "the store reads the account's apps again"
    );
}

#[test]
fn add_to_a_chat_waits_for_an_add_in_flight() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.add(cx);
            dialog.add_to(false, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        bounds(cx, "app-place-dialog").is_none(),
        "the details stay while Add runs"
    );
    assert!(cx.update(|_, cx| dialog.read(cx).added()));
}

#[test]
fn the_details_stay_open_while_an_add_runs() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.add(cx);
            dialog.close(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_some()),
        "the details stay while Add runs"
    );
    assert!(cx.update(|_, cx| dialog.read(cx).added()));
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    click(cx, "app-details-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    assert!(
        bounds(cx, "apps-added-check").is_some(),
        "the store reads the account's apps again"
    );
}

#[test]
fn add_to_a_chat_picks_a_chat_and_go_adds_the_app_there() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-tile-popular-1");
    assert!(
        bounds(cx, "app-details-add").is_none(),
        "an app for chats and teams only isn't added for oneself"
    );
    click(cx, "app-details-add-chat");
    assert!(bounds(cx, "app-place-dialog").is_some());
    click(cx, "app-place-0");
    click(cx, "app-place-go");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::AddChatApp { chat, app }] if chat == GROUP_CHAT && app.id() == POLLS),
        "{:?}",
        writes(&rig)
    );
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_none()),
        "the picker closes"
    );
}

#[test]
fn manage_your_apps_removes_an_app_after_asking() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    assert!(writes(&rig).is_empty(), "it asks first");
    assert!(bounds(cx, "apps-remove-question").is_some());
    click(cx, "apps-remove-confirm");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::RemovePersonalApp { app }] if app == HELPER),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "apps-mine-0").is_none(), "gone from the list");
    assert!(bounds(cx, "apps-mine-none").is_some());
}

#[test]
fn a_refused_removal_says_so_and_keeps_the_app() {
    let rig = Rig::start();
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    *rig.core.team_written.lock().unwrap() = None;
    open_store(&shell, cx);
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    click(cx, "apps-remove-confirm");
    assert!(bounds(cx, "apps-remove-problem").is_some());
    assert!(bounds(cx, "apps-mine-0").is_some());
}

fn manage_page(rig: &Rig, mine: Vec<StoreApp>) -> (Entity<AppsPage>, &'static mut VisualTestContext) {
    *rig.core.user_apps.lock().unwrap() = mine;
    let (shell, cx) = shell_window(rig, true);
    open_store(&shell, cx);
    click(cx, "apps-manage");
    let page = cx.update(|_, cx| shell.read(cx).apps().cloned()).expect("the page");
    (page, cx)
}

fn removes(rig: &Rig) -> usize {
    writes(rig)
        .iter()
        .filter(|write| {
            matches!(
                write,
                TeamWrite::RemovePersonalApp { .. } | TeamWrite::RemoveChatApp { .. }
            )
        })
        .count()
}

#[test]
fn a_second_remove_before_the_reply_sends_no_second_write() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.remove(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(removes(&rig), 1, "{:?}", writes(&rig));
    assert!(cx.update(|_, cx| page.read(cx).removing().is_none()));
}

#[test]
fn a_refresh_keeps_the_remove_in_flight() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.refresh(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| page.read(cx).removing().is_none()),
        "the reply came through: the question closed"
    );
}

#[test]
fn a_failed_remove_can_be_tried_again() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER)]);
    *rig.core.team_written.lock().unwrap() = None;
    click(cx, "apps-mine-0-expand");
    click(cx, "apps-mine-0-remove");
    click(cx, "apps-remove-confirm");
    assert!(bounds(cx, "apps-remove-problem").is_some());
    click(cx, "apps-remove-confirm");
    assert_eq!(removes(&rig), 2, "{:?}", writes(&rig));
    assert!(cx.update(|_, cx| page.read(cx).removing().is_some_and(|(_, failed)| failed)));
}

#[test]
fn a_remove_s_reply_leaves_the_question_about_another_app() {
    let rig = Rig::start();
    let (page, cx) = manage_page(&rig, vec![app(HELPER), app(POLLS)]);
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(HELPER), cx);
            page.remove(cx);
            page.keep(cx);
            page.ask_remove(app(POLLS), cx);
        });
    });
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| page.read(cx).added(HELPER)), "the first app went");
    assert!(
        cx.update(|_, cx| page
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == POLLS && !failed)),
        "the question about the second app stays, with no problem"
    );
    *rig.core.team_written.lock().unwrap() = None;
    cx.update(|_, cx| {
        page.update(cx, |page, cx| {
            page.ask_remove(app(POLLS), cx);
            page.remove(cx);
            page.ask_remove(app(HELPER), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| page
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == HELPER && !failed)),
        "a failure of the other app's write does not show on this question"
    );
}

fn chat_apps(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatAppsDialog> {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    cx.run_until_parked();
    cx.update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ChatAppsDialog>().ok())
        .expect("the chat's apps")
}

#[test]
fn a_chat_s_second_remove_before_the_reply_sends_no_second_write() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.remove(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(removes(&rig), 1, "{:?}", writes(&rig));
}

#[test]
fn a_chat_s_apps_stay_open_while_a_remove_runs() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.close(cx);
            dialog.get_more(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_some()),
        "the chat's apps stay while Remove runs"
    );
    assert_ne!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
    assert!(bounds(cx, "chat-apps-none").is_some(), "the app went");
    click(cx, "chat-apps-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn a_chat_s_remove_reply_leaves_the_question_about_another_app() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS), app(HELPER)];
    let (shell, cx) = shell_window(&rig, true);
    let dialog = chat_apps(&shell, cx);
    cx.update(|_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.ask_remove(app(POLLS), cx);
            dialog.remove(cx);
            dialog.ask_remove(app(HELPER), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| dialog
            .read(cx)
            .removing()
            .is_some_and(|(store, failed)| store.app.id == HELPER && !failed)),
        "the question about the second app stays"
    );
    assert!(
        cx.update(|_, cx| dialog.read(cx).shown(cx).iter().all(|store| store.app.id != POLLS)),
        "the first app went"
    );
}

#[test]
fn a_chat_s_apps_are_removed_after_asking() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    assert!(bounds(cx, "chat-apps-0").is_some());
    click(cx, "chat-apps-0-remove");
    assert!(writes(&rig).is_empty(), "it asks first");
    click(cx, "chat-apps-confirm");
    assert!(
        matches!(writes(&rig).as_slice(), [TeamWrite::RemoveChatApp { chat, app }] if chat == GROUP_CHAT && app == POLLS),
        "{:?}",
        writes(&rig)
    );
    assert!(bounds(cx, "chat-apps-none").is_some());
    click(cx, "chat-apps-get-more");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Apps);
}

#[test]
fn a_chat_s_apps_show_their_pictures() {
    let rig = Rig::start();
    *rig.core.chat_apps.lock().unwrap() = vec![app(POLLS)];
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(48, 48, image::Rgba([60, 90, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    rig.core.app_pictures.lock().unwrap().insert(
        POLLS_TILE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes,
        }),
    );
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat_apps(GROUP_CHAT.to_owned(), window, cx));
    });
    assert!(bounds(cx, "chat-apps-0").is_some());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ChatAppsDialog>().ok())
        .expect("the chat's apps");
    assert!(cx.update(|_, cx| dialog.read(cx).shows_picture(POLLS)));
}

#[test]
fn the_read_only_mode_offers_nothing_that_adds_or_removes() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    click(cx, "apps-org-0");
    assert!(bounds(cx, "app-details").is_some());
    assert!(
        bounds(cx, "app-details-add").is_none(),
        "a personal app not added has no Add"
    );
    click(cx, "app-details-close");
    *rig.core.user_apps.lock().unwrap() = vec![app(HELPER)];
    cx.update(|_, cx| {
        shell
            .read(cx)
            .apps()
            .cloned()
            .expect("the page")
            .update(cx, AppsPage::refresh);
    });
    click(cx, "apps-tile-popular-1");
    assert!(bounds(cx, "app-details-add-chat").is_none());
    assert!(bounds(cx, "app-details-add-team").is_none());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the details");
    cx.update(|_, cx| dialog.update(cx, AppDetailsDialog::add));
    assert!(writes(&rig).is_empty());
    click(cx, "app-details-close");
    click(cx, "apps-manage");
    click(cx, "apps-mine-0-expand");
    assert!(bounds(cx, "apps-mine-0-places").is_some());
    assert!(bounds(cx, "apps-mine-0-remove").is_none());
}

// --- bot chats and bot mentions ---

/// Made-up Helper's bot (`b4`) and Made-up Polls' (`b5`), and the account's 1:1 chats with them as Teams names them.
const HELPER_BOT: &str = "28:b4";
const POLLS_BOT: &str = "28:b5";
const HELPER_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_b4@unq.gbl.spaces";
const POLLS_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_b5@unq.gbl.spaces";
/// A 1:1 chat with a person whose members aren't stored.
const PERSON_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn one_to_one(id: &str, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(last),
    }
}

fn text_message(conversation: &str, id: i64, sender: &str, name: Option<&str>) -> teams_store::Message {
    teams_store::Message {
        conversation_id: conversation.to_owned(),
        id: id.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: name.map(str::to_owned),
        message_type: "RichText/Html".to_owned(),
        content: "<p>made up</p>".to_owned(),
        properties: None,
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

/// The bots' apps the fake engine knows, by the bot's bare id.
fn know_bots(rig: &Rig) {
    let mut bots = rig.core.bot_apps.lock().unwrap();
    bots.insert("b4".to_owned(), app(HELPER));
    bots.insert("b5".to_owned(), app(POLLS));
}

fn row(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) -> teams_ui::chat_list::ChatRow {
    cx.update(|_, cx| shell.read(cx).list().read(cx).row(id).cloned())
        .unwrap_or_else(|| panic!("{id} has a row"))
}

/// The helper bot's chat and a person's, each with a message.
fn helper_and_person() -> (Rig, Entity<Shell>, &'static mut VisualTestContext) {
    let rig = Rig::start();
    know_bots(&rig);
    rig.core.messages.lock().unwrap().insert(
        HELPER_CHAT.to_owned(),
        vec![text_message(HELPER_CHAT, 300, HELPER_BOT, Some("Helper bot"))],
    );
    rig.core.messages.lock().unwrap().insert(
        PERSON_CHAT.to_owned(),
        vec![text_message(PERSON_CHAT, 200, common::LAB_B, None)],
    );
    let (shell, cx) = shell_window_with(
        &rig,
        true,
        vec![one_to_one(HELPER_CHAT, 300), one_to_one(PERSON_CHAT, 200)],
    );
    (rig, shell, cx)
}

/// A bot's chat whose members aren't stored is with the bot its id names, not with a made-up person of the bot's id:
/// named by its app, drawn with its tile, with no call; a person's is as it was.
#[test]
fn a_bot_s_chat_is_with_the_bot_named_by_its_app_with_its_tile() {
    let (_rig, shell, cx) = helper_and_person();
    let helper = row(&shell, cx, HELPER_CHAT);
    assert_eq!((helper.name.as_ref(), helper.named), ("Made-up Helper", true));
    let bot = helper.bot.as_ref().expect("a bot's chat");
    assert_eq!(bot.mri, HELPER_BOT);
    assert_eq!(bot.app.as_ref().map(|app| app.app.app.id.as_str()), Some(HELPER));
    assert_eq!(helper.partner, None, "a bot has no presence");
    assert!(!cx.update(|_, cx| shell.read(cx).list().read(cx).can_call(HELPER_CHAT)));
    assert_eq!(
        cx.update(|_, cx| shell
            .read(cx)
            .list()
            .read(cx)
            .one_to_one_with(HELPER_BOT)
            .map(str::to_owned)),
        Some(HELPER_CHAT.to_owned())
    );
    assert!(bounds(cx, "avatar-app").is_some(), "its app's tile in the list");
    let person = row(&shell, cx, PERSON_CHAT);
    assert!(person.bot.is_none());
    assert_eq!(person.partner.as_deref(), Some(common::LAB_B));
}

/// A bot's chat's header has its tile and its Chat tab, no people and no profile card; its messages its avatar.
#[test]
fn a_bot_s_chat_header_is_its_app_s() {
    let (_rig, shell, cx) = helper_and_person();
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(HELPER_CHAT.to_owned(), window, cx)));
    assert!(bounds(cx, "chat-header-avatar-app").is_some(), "its tile in the header");
    assert!(bounds(cx, "chat-bot-tab-chat shown").is_some());
    assert!(
        bounds(cx, "chat-bot-tab-about").is_some(),
        "About opens the Store's details"
    );
    assert!(bounds(cx, "chat-header-people").is_none());
    assert!(bounds(cx, "profile-card-from-header-name").is_none());
    assert!(bounds(cx, "avatar-300").is_some(), "the bot's message has its avatar");
}

/// Without a stored member or a message, a 1:1 chat's other one is a bot when the engine knows its app; a `28:` sender
/// without an app is a bot by the name its message carried.
#[test]
fn a_bot_s_chat_is_found_by_its_app_or_its_messages() {
    let rig = Rig::start();
    know_bots(&rig);
    let unknown = "19:00000000-0000-4000-8000-00000000000a_bb@unq.gbl.spaces";
    rig.core.messages.lock().unwrap().insert(
        unknown.to_owned(),
        vec![text_message(unknown, 100, "28:bb", Some("Beeper"))],
    );
    // the engine names a sender by the name its newest message carried
    rig.core
        .names
        .lock()
        .unwrap()
        .insert("28:bb".to_owned(), "Beeper".to_owned());
    let (shell, cx) = shell_window_with(&rig, true, vec![one_to_one(POLLS_CHAT, 200), one_to_one(unknown, 100)]);
    let polls = row(&shell, cx, POLLS_CHAT);
    assert_eq!(polls.bot.map(|bot| bot.mri), Some(POLLS_BOT.to_owned()));
    assert_eq!(polls.name.as_ref(), "Made-up Polls");
    let beeper = row(&shell, cx, unknown);
    assert_eq!(
        beeper.bot.as_ref().map(|bot| (bot.mri.as_str(), bot.app.is_none())),
        Some(("28:bb", true))
    );
    assert_eq!(beeper.name.as_ref(), "Beeper");
}

/// A bot's chat nobody wrote in shows its app's intro; About opens the app's details, added, Open going back to it.
#[test]
fn an_empty_bot_chat_shows_its_app_s_intro_and_about_opens_its_details() {
    let rig = Rig::start();
    know_bots(&rig);
    let (shell, cx) = shell_window_with(&rig, true, vec![one_to_one(HELPER_CHAT, 300)]);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(HELPER_CHAT.to_owned(), window, cx)));
    assert!(bounds(cx, "chat-bot-intro").is_some());
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).chat_bot().map(str::to_owned)),
        Some(HELPER_BOT.to_owned())
    );
    click(cx, "chat-bot-tab-about");
    let details = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AppDetailsDialog>().ok())
        .expect("the app's details");
    assert!(
        cx.update(|_, cx| details.read(cx).added()),
        "the app of a chat the account has"
    );
}

/// A bot typing is named by its app, never by its MRI.
#[test]
fn a_bot_s_typing_line_names_its_app() {
    let rig = Rig::start();
    know_bots(&rig);
    rig.core.messages.lock().unwrap().insert(
        POLLS_CHAT.to_owned(),
        vec![text_message(POLLS_CHAT, 100, POLLS_BOT, None)],
    );
    let (shell, cx) = shell_window_with(&rig, true, vec![one_to_one(POLLS_CHAT, 100)]);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(POLLS_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    for (bot, said) in [(POLLS_BOT, "Made-up Polls is typing…"), ("28:cc", "Bot is typing…")] {
        rig.send(Event::Typing {
            conversation: POLLS_CHAT.to_owned(),
            from: Some(bot.to_owned()),
            typing: true,
        });
        cx.run_until_parked();
        let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
        assert_eq!(cx.update(|_, cx| chat.read(cx).typing_text(cx)).as_deref(), Some(said));
        rig.send(Event::Typing {
            conversation: POLLS_CHAT.to_owned(),
            from: Some(bot.to_owned()),
            typing: false,
        });
        cx.run_until_parked();
    }
}

fn picker_in_group(apps: bool) -> (Rig, Entity<Shell>, &'static mut VisualTestContext) {
    let rig = Rig::start();
    rig.core.members.lock().unwrap().insert(
        GROUP_CHAT.to_owned(),
        vec![Member::new(common::LAB_A, None), Member::new(common::LAB_B, None)],
    );
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(common::LAB_B.to_owned(), "Lab Bee".to_owned());
    rig.core
        .mentionable_bots
        .lock()
        .unwrap()
        .insert(GROUP_CHAT.to_owned(), vec![app(POLLS)]);
    let (shell, cx) = shell_window(&rig, apps);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_input("Who's in? @");
    cx.run_until_parked();
    (rig, shell, cx)
}

fn offered(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Vec<String> {
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    cx.update(|_, cx| {
        compose
            .read(cx)
            .picker()
            .map(|picker| {
                picker
                    .matches
                    .iter()
                    .map(|candidate| candidate.name.to_string())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// A group chat's picker offers the bots of its apps after its members and before Everyone; the bot picked is sent
/// as a bot's mention with its app's id. In a cloud that serves no apps, no bot is offered.
#[test]
fn a_group_chat_s_picker_offers_its_apps_bots_and_sends_a_bot_s_mention() {
    let (_rig, shell, cx) = picker_in_group(false);
    assert_eq!(offered(&shell, cx), ["Lab Bee", "Everyone"]);

    let (rig, shell, cx) = picker_in_group(true);
    assert_eq!(offered(&shell, cx), ["Lab Bee", "Made-up Polls", "Everyone"]);
    assert!(bounds(cx, "mention-bot").is_some());
    cx.simulate_input("poll");
    cx.run_until_parked();
    assert_eq!(offered(&shell, cx), ["Made-up Polls"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    let Some(teams_core::Action::Send { text, .. }) = actions.first() else {
        panic!("a post: {actions:?}");
    };
    assert!(
        text.html().contains("itemid=\"0\">Made-up Polls</span>"),
        "{}",
        text.html()
    );
    assert_eq!(
        text.bots().get(&teams_proto::Mri::new(POLLS_BOT)).map(String::as_str),
        Some(POLLS)
    );
    assert!(text.participants().contains(&teams_proto::Mri::new(POLLS_BOT)));
}

/// A bot's mention copied from one chat's box and pasted into another's, whose picker never read its bots, is sent
/// with its app's id: the box asks the engine for the app of a bot it doesn't know.
#[test]
fn a_pasted_bot_s_mention_is_sent_with_its_app() {
    let (rig, cx) = bot_mention_pasted_in_other_group(|_, _| {});
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    let Some(teams_core::Action::Send { conversation, text }) = actions.first() else {
        panic!("a post: {actions:?}");
    };
    assert_eq!(conversation, OTHER_GROUP);
    assert_eq!(
        text.bots().get(&teams_proto::Mri::new(POLLS_BOT)).map(String::as_str),
        Some(POLLS)
    );
}

/// An ask for a pasted bot's app that failed is asked again at the next change of the text, and the mention is then
/// sent with its app's id.
#[test]
fn a_pasted_bot_s_app_that_could_not_be_read_is_asked_again() {
    let (rig, cx) = bot_mention_pasted_in_other_group(|rig, _| *rig.core.bot_app_failures.lock().unwrap() = 1);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, teams_core::Action::Send { .. })),
        "not sent while the app is unknown: {actions:?}"
    );
    cx.simulate_input(" ");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    let Some(teams_core::Action::Send { text, .. }) = actions.first() else {
        panic!("a post: {actions:?}");
    };
    assert_eq!(
        text.bots().get(&teams_proto::Mri::new(POLLS_BOT)).map(String::as_str),
        Some(POLLS)
    );
}

/// An edit of the account's own message that mentions the Polls bot with its app (as browser Teams sends it): the
/// edit sends the bot's mention with its app's id again.
#[test]
fn an_edit_of_a_bot_s_mention_keeps_its_app() {
    let rig = Rig::start();
    let mut mine = text_message(GROUP_CHAT, 400, common::LAB_A, Some("Lab A"));
    mine.content = "<p><readonly class=\"skipProofing\" itemtype=\"http://schema.skype.com/Mention\" \
                    spellcheck=\"false\"><span itemtype=\"http://schema.skype.com/Mention\" itemscope \
                    itemid=\"0\">Made-up Polls</span></readonly>&nbsp;who's in?</p>"
        .to_owned();
    mine.properties = Some(
        json!({ "mentions": [{
            "@type": "http://schema.skype.com/Mention",
            "itemid": "0",
            "mri": POLLS_BOT,
            "mentionType": "bot",
            "displayName": "Made-up Polls",
            "appId": POLLS,
        }] })
        .to_string(),
    );
    rig.core
        .messages
        .lock()
        .unwrap()
        .insert(GROUP_CHAT.to_owned(), vec![mine]);
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    cx.update(|window, cx| chat.update(cx, |chat, cx| chat.act(MessageAction::Edit, "400", window, cx)));
    cx.run_until_parked();
    cx.simulate_input(" now");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    let edits: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            teams_core::Action::Edit { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    let [text] = edits.as_slice() else {
        panic!("one edit: {actions:?}");
    };
    assert_eq!(
        text.bots().get(&teams_proto::Mri::new(POLLS_BOT)).map(String::as_str),
        Some(POLLS),
        "{}",
        text.html()
    );
}

const OTHER_GROUP: &str = "19:made-up-other-group@thread.v2";

/// The Polls bot's mention picked in the group's box, copied, and pasted into the box of another group, after
/// `before_paste`; nothing sent yet.
fn bot_mention_pasted_in_other_group(
    before_paste: impl FnOnce(&Rig, &mut VisualTestContext),
) -> (Rig, &'static mut VisualTestContext) {
    let (rig, shell, cx) = picker_in_group(true);
    know_bots(&rig);
    cx.simulate_input("poll");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-a ctrl-c");
    rig.core.conversations.lock().unwrap().push(Conversation {
        id: OTHER_GROUP.to_owned(),
        kind: ConversationKind::Group,
        topic: Some("Made-up other group".to_owned()),
        version: 1,
        last_activity: Some(90),
    });
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(OTHER_GROUP.to_owned(), window, cx)));
    cx.run_until_parked();
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    assert_eq!(cx.update(|_, cx| chat.read(cx).conversation().to_owned()), OTHER_GROUP);
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    before_paste(&rig, cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.run_until_parked();
    (rig, cx)
}

const CHANNEL: &str = "19:made-up-channel@thread.tacv2";
const POST: &str = "1700000000001";

/// A made-up team whose General is [`CHANNEL`].
fn channel_team() -> teams_store::Team {
    teams_store::Team {
        id: CHANNEL.to_owned(),
        name: "Made-up team".to_owned(),
        channels: vec![teams_store::TeamChannel {
            id: CHANNEL.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: teams_store::ChannelKind::Standard,
            shown: true,
            about: teams_store::ChannelAbout::default(),
        }],
        ..teams_store::Team::default()
    }
}

/// A `task/fetch` dialog of Polls' bot in `conversation`: its card's first button submits, its second fetches the
/// next dialog.
fn polls_dialog(conversation: &str) -> teams_ui::app_dialog::DialogSeed {
    use teams_ui::app_dialog::{DialogApp, DialogSeed, Opened};
    DialogSeed {
        app: DialogApp::of(&app(POLLS)),
        target: teams_ui::bot_card::Target {
            bot: teams_proto::Mri::new("28:b5"),
            app_id: POLLS.to_owned(),
            conversation: conversation.to_owned(),
        },
        opened: Opened::Task {
            dialog: teams_proto::TaskDialog {
                title: Some("Made-up poll".to_owned()),
                card: Some(
                    json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Ask"}],
                    "actions": [
                        {"type": "Action.Submit", "title": "Send", "data": {"step": "send"}},
                        {"type": "Action.Submit", "title": "More", "data": {"msteams": {"type": "task/fetch"}}},
                    ]}),
                ),
                url: None,
                width: None,
                height: None,
            },
            message: Some(POST.to_owned()),
        },
        in_teams: None,
    }
}

/// A result with one made-up card.
fn poll_result() -> teams_proto::InvokeAnswer {
    teams_proto::InvokeAnswer::Results {
        grid: false,
        attachments: vec![json!({"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made-up poll"}]}})],
    }
}

/// The dialog open over the shell.
fn app_dialog(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<teams_ui::app_dialog::AppDialog> {
    cx.update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<teams_ui::app_dialog::AppDialog>().ok())
        .expect("the app's dialog")
}

/// Opens `seed`'s dialog and presses its button `action`, the bot answering `answer`.
fn submit_dialog(
    rig: &Rig,
    shell: &Entity<Shell>,
    seed: teams_ui::app_dialog::DialogSeed,
    action: usize,
    answer: teams_proto::InvokeAnswer,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_app_dialog(seed, window, cx)));
    cx.run_until_parked();
    let dialog = app_dialog(shell, cx);
    assert_eq!(cx.update(|_, cx| dialog.read(cx).stage_name()), "card");
    rig.core.bot_answers.lock().unwrap().push_back(Ok(answer));
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.press(action, cx)));
    cx.run_until_parked();
}

#[test]
fn a_dialogs_result_goes_into_the_chats_box() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    submit_dialog(&rig, &shell, polls_dialog(GROUP_CHAT), 0, poll_result(), cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_none()),
        "the dialog closes"
    );
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    let cards = cx.update(|_, cx| compose.read(cx).app_cards().to_vec());
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].app_id, POLLS);
    assert_eq!(cards[0].content["body"][0]["text"], "Made-up poll");
}

#[test]
fn a_channel_dialogs_result_goes_into_the_posts_thread_box() {
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_channel_id(CHANNEL.to_owned(), window, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(CHANNEL.to_owned())
    );
    let chain = teams_proto::reply_chain_id(CHANNEL, POST).expect("a chain");
    submit_dialog(&rig, &shell, polls_dialog(&chain), 0, poll_result(), cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_none()),
        "the dialog closes"
    );
    let channel = cx
        .update(|_, cx| shell.read(cx).channel().cloned())
        .expect("the channel");
    let thread = cx
        .update(|_, cx| {
            channel
                .read(cx)
                .thread()
                .map(|(post, view)| (post.to_owned(), view.clone()))
        })
        .expect("the post's thread opens");
    assert_eq!(thread.0, POST);
    let compose = cx.update(|_, cx| thread.1.read(cx).compose().clone());
    assert_eq!(cx.update(|_, cx| compose.read(cx).app_cards().len()), 1);
    let posts = cx.update(|_, cx| channel.read(cx).posts().read(cx).compose().clone());
    assert!(cx.update(|_, cx| posts.read(cx).app_cards().is_empty()));
    assert!(cx.update(|_, cx| shell.read(cx).toast().is_none()));
}

#[test]
fn a_result_with_no_box_shown_says_so() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    open_store(&shell, cx);
    submit_dialog(&rig, &shell, polls_dialog(GROUP_CHAT), 0, poll_result(), cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some(teams_ui::app_dialog::NO_BOX.into())
    );
}

#[test]
fn a_result_for_a_chat_not_shown_never_goes_into_the_shown_chats_box() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    submit_dialog(&rig, &shell, polls_dialog(OTHER_GROUP), 0, poll_result(), cx);
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    assert!(cx.update(|_, cx| compose.read(cx).app_cards().is_empty()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some(teams_ui::app_dialog::NO_BOX.into())
    );
}

#[test]
fn a_result_for_another_channels_post_never_goes_into_the_shown_channels_boxes() {
    const OTHER_CHANNEL: &str = "19:made-up-other-channel@thread.tacv2";
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_channel_id(CHANNEL.to_owned(), window, cx)));
    cx.run_until_parked();
    let chain = teams_proto::reply_chain_id(OTHER_CHANNEL, POST).expect("a chain");
    submit_dialog(&rig, &shell, polls_dialog(&chain), 0, poll_result(), cx);
    let channel = cx
        .update(|_, cx| shell.read(cx).channel().cloned())
        .expect("the channel");
    assert!(
        cx.update(|_, cx| channel.read(cx).thread().is_none()),
        "no thread opens"
    );
    let posts = cx.update(|_, cx| channel.read(cx).posts().read(cx).compose().clone());
    assert!(cx.update(|_, cx| posts.read(cx).app_cards().is_empty()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()),
        Some(teams_ui::app_dialog::NO_BOX.into())
    );
}

#[test]
fn a_task_fetch_button_in_a_dialog_opens_the_next_dialog_in_its_place() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    let next = teams_proto::InvokeAnswer::Dialog(teams_proto::TaskDialog {
        title: Some("Made-up next step".to_owned()),
        card: Some(
            json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Check"}],
            "actions": [{"type": "Action.Submit", "title": "Done", "data": {"step": "check"}}]}),
        ),
        url: None,
        width: None,
        height: None,
    });
    submit_dialog(&rig, &shell, polls_dialog(GROUP_CHAT), 1, next, cx);
    assert!(matches!(
        rig.core.bot_asks.lock().unwrap().last().map(|ask| &ask.kind),
        Some(teams_proto::InvokeKind::TaskFetch(_))
    ));
    // the next dialog's own button submits its own data
    let dialog = app_dialog(&shell, cx);
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.press(0, cx)));
    cx.run_until_parked();
    let asks = rig.core.bot_asks.lock().unwrap();
    let Some(teams_proto::InvokeKind::TaskSubmit(card)) = asks.last().map(|ask| &ask.kind) else {
        panic!("a task/submit: {asks:?}");
    };
    assert_eq!(card.data["step"], "check");
}

/// A card asking for a name (required), a number and a note, with one submit.
fn form_card() -> serde_json::Value {
    json!({"type": "AdaptiveCard", "body": [
        {"type": "Input.Text", "id": "name", "label": "Name", "isRequired": true},
        {"type": "Input.Number", "id": "count", "label": "Count"},
        {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true}
    ], "actions": [{"type": "Action.Submit", "title": "Create", "data": {"step": "create"}}]})
}

/// A dialog of [`form_card`].
fn form_dialog(conversation: &str) -> teams_ui::app_dialog::DialogSeed {
    let mut seed = polls_dialog(conversation);
    if let teams_ui::app_dialog::Opened::Task { dialog, .. } = &mut seed.opened {
        dialog.card = Some(form_card());
    }
    seed
}

/// A click just inside the left edge of `selector`'s frame.
fn click_edge(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    let at = gpui::point(found.left() + gpui::px(2.0), found.center().y);
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// Keys typed one at a time, as a keyboard sends them: each goes through the window's key handling first.
fn type_keys(cx: &mut VisualTestContext, keys: &str) {
    for key in keys.chars() {
        cx.simulate_keystrokes(&key.to_string());
    }
    cx.run_until_parked();
}

#[test]
fn a_dialogs_inputs_take_typed_keys_into_its_submit() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_app_dialog(form_dialog(GROUP_CHAT), window, cx);
        });
    });
    cx.run_until_parked();
    let field = |id: &str| format!("card-app-dialog-1-c0-in-{id}");
    // a click on the field's frame, beside its text, takes the keys as well
    click_edge(cx, &field("name"));
    // the chat's own keys (`a` reacts, `r` replies, `1` picks) are text here too
    type_keys(cx, "ra1");
    click(cx, &field("count"));
    type_keys(cx, "42");
    click_edge(cx, &field("note"));
    type_keys(cx, "ok");
    click(cx, "card-app-dialog-1-c0a0");
    let asks = rig.core.bot_asks.lock().unwrap();
    let Some(teams_proto::InvokeKind::TaskSubmit(card)) = asks.last().map(|ask| &ask.kind) else {
        panic!("a task/submit: {asks:?}");
    };
    assert_eq!(
        card.data,
        json!({"step": "create", "name": "ra1", "count": "42", "note": "ok"})
    );
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).expect("the chat");
    let compose = cx.update(|_, cx| chat.read(cx).compose().clone());
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).value(cx).to_string()),
        "",
        "nothing goes to the box behind"
    );
}

#[test]
fn an_apps_message_after_its_error_shows_alone() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    let error = teams_proto::InvokeAnswer::Error {
        code: Some("Conflict".to_owned()),
        message: "Made-up error.".to_owned(),
    };
    submit_dialog(&rig, &shell, polls_dialog(GROUP_CHAT), 0, error, cx);
    let dialog = app_dialog(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).error().map(str::to_owned)).as_deref(),
        Some("Made-up error.")
    );
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(teams_proto::InvokeAnswer::Message("Made-up thanks.".to_owned())));
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.press(0, cx)));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| dialog.read(cx).stage_name()), "note");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).error().map(str::to_owned)), None);
    assert!(
        bounds(cx, "app-dialog-error").is_none(),
        "no old error under the message"
    );
}

#[test]
fn a_dialog_from_the_plus_takes_typed_keys_after_a_refused_create() {
    let rig = Rig::start();
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (shell, cx) = shell_window(&rig, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(GROUP_CHAT.to_owned(), window, cx)));
    cx.run_until_parked();
    let dialog = teams_proto::InvokeAnswer::Dialog(teams_proto::TaskDialog {
        title: Some("New record".to_owned()),
        card: Some(form_card()),
        url: None,
        width: None,
        height: None,
    });
    {
        let mut answers = rig.core.bot_answers.lock().unwrap();
        answers.push_back(Ok(teams_proto::InvokeAnswer::Results {
            grid: false,
            attachments: Vec::new(),
        }));
        answers.push_back(Ok(dialog));
    }
    click(cx, "compose-apps");
    click(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e001");
    click(cx, "apps-flyout-command-1");
    let shown = app_dialog(&shell, cx);
    assert_eq!(cx.update(|_, cx| shown.read(cx).stage_name()), "card");
    let field = |id: &str| format!("card-app-dialog-1-c0-in-{id}");
    click(cx, "card-app-dialog-1-c0a0");
    assert!(bounds(cx, &format!("{}-error", field("name"))).is_some());
    click_edge(cx, &field("name"));
    type_keys(cx, "ra1");
    click(cx, "card-app-dialog-1-c0a0");
    let asks = rig.core.bot_asks.lock().unwrap();
    let Some(teams_proto::InvokeKind::SubmitAction { data, .. }) = asks.last().map(|ask| &ask.kind) else {
        panic!("a submitAction: {asks:?}");
    };
    assert_eq!(data["name"], "ra1");
}
