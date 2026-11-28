//! Message extensions in a chat (`teams_ui::apps_flyout`,
//! `teams_ui::app_dialog`), on GPUI's test context over the fake engine and the synthetic apps of
//! `tests/fixtures/bots/extension-apps.json`: the compose box's Actions and apps (its list, its search, its keys, "Get
//! more apps"), an app's search and a result's card in the box, the message sent with the card, an action command's
//! dialog asked for, a message's "More actions" with the apps' commands, and none of it where the cloud serves no apps or in
//! read-only mode. Every app, bot and id here is synthetic.

use gpui::{Entity, Modifiers, VisualTestContext};
use serde_json::json;
use teams_core::Action;
use teams_proto::{InvokeAnswer, InvokeKind};
use teams_ui::chat_view::{ChatView, ChatViewEvent, MessageAction};

use super::common::{LAB_A, LAB_B, Rig};
use super::{AB, NOON, chat_window, draw, message, rig_with, trace};

const FINDER: &str = "00000000-0000-4000-8000-00000000e001";
const ID: &str = "701";

/// The rig, its cloud serving apps or not (`apps`).
fn apps_rig(apps: bool) -> Rig {
    apps_rig_with(apps, vec![message(ID, LAB_B, "<p>A made-up order</p>", NOON)])
}

fn apps_rig_with(apps: bool, messages: Vec<teams_store::Message>) -> Rig {
    let rig = rig_with(messages);
    if !apps {
        *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
            apps: false,
            ..teams_core::Capabilities::ALL
        });
    }
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    rig
}

fn window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let (written, _) = trace();
    chat_window(rig, written)
}

fn found(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let at = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
}

fn told(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> std::rc::Rc<std::cell::RefCell<Vec<ChatViewEvent>>> {
    let told: std::rc::Rc<std::cell::RefCell<Vec<ChatViewEvent>>> = std::rc::Rc::default();
    let kept = std::rc::Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    told
}

fn results() -> InvokeAnswer {
    InvokeAnswer::Results {
        grid: false,
        attachments: vec![
            json!({"contentType": "application/vnd.microsoft.card.adaptive",
                   "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 1"}]},
                   "preview": {"contentType": "application/vnd.microsoft.card.thumbnail",
                               "content": {"title": "Record 1", "text": "Made up",
                                           "tap": {"type": "invoke", "value": {"id": 1}}}}}),
            json!({"contentType": "application/vnd.microsoft.card.adaptive",
                   "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 2"}]}}),
        ],
    }
}

#[test]
fn the_plus_lists_the_apps_for_the_box_and_its_search_and_keys_work() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    click(cx, "compose-apps");
    // the apps whose commands run in the compose box, the account's only
    assert!(found(cx, &format!("apps-flyout-app-{FINDER}")));
    assert!(found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002"));
    assert!(
        !found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e003"),
        "a message action only"
    );
    assert!(
        !found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e004"),
        "the store's offer"
    );
    cx.simulate_input("praise");
    assert!(!found(cx, &format!("apps-flyout-app-{FINDER}")));
    assert!(found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002"));
    cx.simulate_input("zzz");
    assert!(found(cx, "apps-flyout-no-match"));
    click(cx, "apps-flyout-more");
    assert!(matches!(told.borrow().last(), Some(ChatViewEvent::ShowStore)));
    assert!(!found(cx, "apps-flyout"), "it closes");
    // Enter chooses the row Up and Down stand on; Escape goes back, then closes
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    down_past_the_actions(&view, cx);
    cx.simulate_keystrokes("down up enter");
    cx.run_until_parked();
    assert!(found(cx, "apps-flyout-pane-name"));
    assert!(found(cx, "apps-flyout-result-1"));
    cx.simulate_keystrokes("escape");
    assert!(!found(cx, "apps-flyout-pane-name"));
    cx.simulate_keystrokes("escape");
    assert!(!found(cx, "apps-flyout"));
}

/// The Finder again as made-up app `n`.
fn more_finder(rig: &Rig, n: usize) -> String {
    let mut apps = rig.core.extension_apps.lock().unwrap();
    let mut app = apps.iter().find(|app| app.id() == FINDER).cloned().expect("the Finder");
    let id = format!("00000000-0000-4000-8000-0000000f{n:04}");
    app.store.app.id.clone_from(&id);
    app.store.app.name = format!("Made-up Extra {n}");
    apps.push(app);
    id
}

fn inside(cx: &mut VisualTestContext, selector: &str, list: &str) -> bool {
    draw(cx);
    let row = cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()));
    let list = cx.debug_bounds(Box::leak(list.to_owned().into_boxed_str()));
    row.zip(list)
        .is_some_and(|(row, list)| row.top() >= list.top() && row.bottom() <= list.bottom())
}

#[test]
fn the_lit_row_of_a_long_flyout_stays_in_view() {
    let rig = apps_rig(true);
    let extra: Vec<String> = (1..=12).map(|n| more_finder(&rig, n)).collect();
    let (view, cx) = window(&rig);
    click(cx, "compose-apps");
    let rows = flyout_labels(&view, cx).len() + 1 + extra.len();
    let row = |id: &str| format!("apps-flyout-app-{id}");
    let last = row(extra.last().expect("an extra app"));
    assert!(
        !inside(cx, &last, "apps-flyout-list"),
        "the list is longer than its view"
    );
    for _ in 0..rows {
        cx.simulate_keystrokes("down");
    }
    assert!(inside(cx, &last, "apps-flyout-list"), "Down keeps the lit row in view");
    assert!(!inside(cx, &row(FINDER), "apps-flyout-list"), "the list scrolled");
    for _ in 0..rows {
        cx.simulate_keystrokes("up");
    }
    assert!(
        inside(cx, "compose-correct-row", "apps-flyout-list"),
        "Up keeps the lit row in view"
    );
}

/// The search ran at once, with no words, as its command says.
fn the_search_ran_at_once(rig: &Rig) {
    let asks = rig.core.bot_asks.lock().unwrap();
    let [ask] = asks.as_slice() else { panic!("{asks:?}") };
    assert!(matches!(&ask.kind, InvokeKind::Query { command, parameters, .. }
        if command == "findRecords" && parameters == &[("keyword".to_owned(), String::new())]));
    assert_eq!(ask.app_id, FINDER);
    assert_eq!(ask.conversation.as_deref(), Some(AB));
}

/// The text the chat's send carried.
fn sent(rig: &Rig) -> teams_proto::chat::MessageText {
    let actions = rig.core.actions.lock().unwrap().clone();
    actions
        .iter()
        .find_map(|action| match action {
            Action::Send { conversation, text } if conversation == AB => Some(text.clone()),
            _ => None,
        })
        .expect("a send")
}

#[test]
fn a_result_goes_into_the_box_and_out_with_the_message() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let chosen = InvokeAnswer::Results {
        grid: false,
        attachments: vec![json!({"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 1, chosen"}]}})],
    };
    rig.core.bot_answers.lock().unwrap().extend([Ok(results()), Ok(chosen)]);
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    the_search_ran_at_once(&rig);
    // a result whose preview names an invoke asks the app for its card
    click(cx, "apps-flyout-result-0");
    assert!(matches!(&rig.core.bot_asks.lock().unwrap()[1].kind,
        InvokeKind::SelectItem { value } if value == &json!({"id": 1})));
    assert!(!found(cx, "apps-flyout"));
    assert!(found(cx, "compose-app-card-0"));
    // the card is sent with the message, with no text of its own
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.run_until_parked();
    let sent = sent(&rig);
    assert_eq!(sent.cards().len(), 1);
    assert_eq!(sent.cards()[0].app_id, FINDER);
    assert_eq!(sent.cards()[0].content["body"][0]["text"], "Record 1, chosen");
    assert!(sent.html().contains("InputExtension"));
    assert!(!found(cx, "compose-app-card-0"), "the box is empty again");
}

#[test]
fn a_cards_close_mark_takes_it_out_of_the_box() {
    let rig = apps_rig(true);
    let (_view, cx) = window(&rig);
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    // a result with no invoke goes in as it is
    click(cx, "apps-flyout-result-1");
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 1);
    assert!(found(cx, "compose-app-card-0"));
    click(cx, "compose-app-card-remove-0");
    assert!(!found(cx, "compose-app-card-0"));
}

/// The search answer that the app holds back as `n`th, given now.
fn answer_held(rig: &Rig, n: usize, answer: InvokeAnswer) {
    let held = rig.core.held_bot_answers.lock().unwrap().remove(n);
    held.send(Ok(answer));
}

#[test]
fn a_late_answer_for_an_app_left_never_fills_the_next_apps_pane() {
    let rig = apps_rig(true);
    let other = more_finder(&rig, 1);
    *rig.core.hold_bot_answers.lock().unwrap() = true;
    let (_view, cx) = window(&rig);
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    cx.simulate_keystrokes("escape");
    click(cx, &format!("apps-flyout-app-{other}"));
    {
        let asks = rig.core.bot_asks.lock().unwrap();
        let apps: Vec<&str> = asks.iter().map(|ask| ask.app_id.as_str()).collect();
        assert_eq!(apps, [FINDER, other.as_str()], "each app's search went");
    }
    // the first app's answer comes after its pane is gone: the open pane still waits for its own
    answer_held(&rig, 0, results());
    cx.run_until_parked();
    assert!(found(cx, "apps-flyout-pane-name"));
    assert!(!found(cx, "apps-flyout-result-0"));
    let own = InvokeAnswer::Results {
        grid: false,
        attachments: vec![json!({"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Own record"}]}})],
    };
    answer_held(&rig, 0, own);
    cx.run_until_parked();
    assert!(found(cx, "apps-flyout-result-0"));
    assert!(!found(cx, "apps-flyout-result-1"), "the open app's one result only");
}

#[test]
fn an_action_command_asks_for_its_dialog_and_a_web_page_asks_nothing() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    click(cx, "apps-flyout-command-1");
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    assert_eq!(seed.app.name, "Made-up Finder");
    assert!(matches!(&seed.opened, teams_ui::app_dialog::Opened::Command { command, .. } if command.id == "newRecord"));
    // the one action of an app opens its dialog at once
    click(cx, "compose-apps");
    click(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002");
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    assert!(
        matches!(&seed.opened, teams_ui::app_dialog::Opened::Command { command, .. } if command.id == "sendPraise")
    );
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 1, "only the search asked");
}

#[test]
fn more_actions_lists_the_apps_commands_and_an_offer_opens_its_details() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    click(cx, "message-more-actions");
    let task = "message-app-action-00000000-0000-4000-8000-00000000e003-taskFromMessage";
    let ticket = "message-app-action-00000000-0000-4000-8000-00000000e004-fileTicket";
    assert!(found(cx, task));
    assert!(found(cx, ticket));
    assert!(found(cx, "message-create-workflows"), "Create workflows stays");
    click(cx, ticket);
    assert!(
        matches!(told.borrow().last(), Some(ChatViewEvent::AppDetails(store)) if store.app.name == "Made-up Tickets")
    );
    // the account's app opens its dialog on the message, described as Teams describes it
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    click(cx, "message-more-actions");
    click(cx, task);
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    let teams_ui::app_dialog::Opened::Command { message, context, .. } = &seed.opened else {
        panic!("a command");
    };
    assert_eq!(*context, teams_proto::CommandContext::Message);
    let payload = message.as_ref().expect("the message");
    assert_eq!(payload.id, ID);
    assert_eq!(payload.html, "<p>A made-up order</p>");
}

#[test]
fn without_the_apps_service_or_in_read_only_mode_there_is_no_app() {
    let rig = apps_rig(false);
    let (view, cx) = window(&rig);
    click(cx, "compose-apps");
    assert!(!found(cx, "apps-flyout"));
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).message_app_actions().is_empty()));
    let rig = apps_rig(true);
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx) = window(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    assert!(!found(cx, "apps-flyout"));
    assert!(cx.update(|_, cx| view.read(cx).message_app_actions().is_empty()));
}

/// A made-up result card, as a dialog puts it into the box.
fn made_up_card() -> teams_proto::chat::AppCard {
    teams_proto::chat::AppCard::new(
        FINDER.to_owned(),
        "Made-up Finder".to_owned(),
        None,
        "application/vnd.microsoft.card.adaptive".to_owned(),
        json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made up"}]}),
    )
}

#[test]
fn an_edit_waits_for_the_boxs_cards_and_never_carries_one() {
    const MINE: &str = "702";
    let rig = apps_rig_with(
        true,
        vec![
            message(ID, LAB_B, "<p>A made-up order</p>", NOON),
            message(MINE, LAB_A, "<p>Mine</p>", NOON + 1_000),
        ],
    );
    let (view, cx) = window(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    let editing = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| matches!(compose.read(cx).mode(), teams_ui::compose::Mode::Edit { .. }))
    };
    // a card in the box: no edit starts, from the menu or from Up
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.add_app_card(made_up_card(), window, cx)));
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(MessageAction::Edit, MINE, window, cx)));
    cx.run_until_parked();
    assert!(!editing(cx));
    assert!(cx.update(|_, cx| compose.read(cx).problem().is_some()));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert!(!editing(cx));
    // the card sent, an edit starts; Actions and apps doesn't open while it runs
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(MessageAction::Edit, MINE, window, cx)));
    cx.run_until_parked();
    assert!(editing(cx));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    assert!(!found(cx, "apps-flyout"));
    // a card that comes in during the edit (a dialog's result) stays out of it, and in the box after it
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.add_app_card(made_up_card(), window, cx)));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.run_until_parked();
    let edits: Vec<usize> = rig
        .core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Edit { text, .. } => Some(text.cards().len()),
            _ => None,
        })
        .collect();
    assert_eq!(edits, [0]);
    assert!(!editing(cx));
    assert_eq!(cx.update(|_, cx| compose.read(cx).app_cards().len()), 1);
}

/// A second made-up result card, different from [`made_up_card`].
fn other_card() -> teams_proto::chat::AppCard {
    teams_proto::chat::AppCard::new(
        FINDER.to_owned(),
        "Made-up Finder".to_owned(),
        None,
        "application/vnd.microsoft.card.adaptive".to_owned(),
        json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made up too"}]}),
    )
}

#[test]
fn a_blank_edit_beside_a_card_saves_nothing() {
    const MINE: &str = "702";
    let rig = apps_rig_with(
        true,
        vec![
            message(ID, LAB_B, "<p>A made-up order</p>", NOON),
            message(MINE, LAB_A, "<p>Mine</p>", NOON + 1_000),
        ],
    );
    let (view, cx) = window(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(MessageAction::Edit, MINE, window, cx)));
    cx.run_until_parked();
    assert!(
        !found(cx, "compose-send-greyed"),
        "Save is lit while the edit holds text"
    );
    // a card comes in during the edit, then all the text of the edit is deleted
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.add_app_card(made_up_card(), window, cx);
            compose.editor().update(cx, teams_ui::compose::Editor::clear);
        });
    });
    cx.run_until_parked();
    assert!(
        found(cx, "compose-send-greyed"),
        "the card does not light Save for a blank edit"
    );
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.run_until_parked();
    let edits = rig
        .core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| matches!(action, Action::Edit { .. }))
        .count();
    assert_eq!(edits, 0, "no blank edit goes out");
    assert!(cx.update(|_, cx| matches!(compose.read(cx).mode(), teams_ui::compose::Mode::Edit { .. })));
    assert_eq!(cx.update(|_, cx| compose.read(cx).app_cards().len()), 1);
}

#[test]
fn a_refused_send_gives_its_card_back_beside_a_card_that_came_in_meanwhile() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    rig.core
        .write_errors
        .lock()
        .unwrap()
        .push_back(teams_core::EngineError::Stopped);
    // the write is refused after the next dialog's card came into the emptied box
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.add_app_card(made_up_card(), window, cx);
            compose.send(window, cx);
            assert!(
                compose.app_cards().is_empty(),
                "the box empties once the write is asked for"
            );
            compose.add_app_card(other_card(), window, cx);
        });
    });
    cx.run_until_parked();
    let texts: Vec<String> = cx.update(|_, cx| {
        compose
            .read(cx)
            .app_cards()
            .iter()
            .map(|card| card.content["body"][0]["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    });
    assert_eq!(texts, ["Made up", "Made up too"], "the refused send's card first");
}

#[test]
fn a_hidden_button_s_action_is_over_the_apps_and_customize_is_at_the_foot() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let settings = view.read_with(cx, |view, cx| view.compose().read(cx).settings().clone());
    cx.update(|_, cx| {
        settings.update(cx, |model, cx| {
            model.change(
                |settings| settings.show_compose_tool(teams_ui::settings::ComposeTool::Emoji, false),
                cx,
            );
        });
    });
    assert!(!found(cx, "compose-emoji"));
    click(cx, "compose-apps");
    assert!(found(cx, "apps-flyout"));
    let emoji = cx
        .debug_bounds("compose-emoji-row")
        .expect("the hidden Emoji is a row of the flyout");
    let app = cx
        .debug_bounds(Box::leak(format!("apps-flyout-app-{FINDER}").into_boxed_str()))
        .unwrap();
    assert!(emoji.bottom() <= app.top(), "the actions are over the apps");
    assert!(found(cx, "compose-customize-row"));
    // a search lists apps alone
    cx.simulate_input("praise");
    assert!(!found(cx, "compose-emoji-row"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // Enter on the first row takes the hidden action: the emoji picker opens
    click(cx, "compose-apps");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!found(cx, "apps-flyout"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // Customize toolbar at the foot lists the buttons
    click(cx, "compose-apps");
    click(cx, "compose-customize-row");
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
}

/// The chat's box with `tool` hidden from its toolbar.
fn hiding(view: &Entity<ChatView>, cx: &mut VisualTestContext, tool: teams_ui::settings::ComposeTool) {
    let settings = view.read_with(cx, |view, cx| view.compose().read(cx).settings().clone());
    cx.update(|_, cx| {
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.show_compose_tool(tool, false), cx);
        });
    });
}

fn flyout_apart(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<bool> {
    view.read_with(cx, |view, cx| {
        let compose = view.compose().read(cx);
        let flyout = compose.apps_flyout().expect("the flyout is open").read(cx);
        flyout.actions().iter().map(|action| action.apart).collect()
    })
}

/// The hidden tools' actions come first: Down past them, onto the first app.
fn down_past_the_actions(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    for _ in 0..flyout_labels(view, cx).len() {
        cx.simulate_keystrokes("down");
    }
}

/// The space between the bottom of the row `above` and the top of the row `below`.
fn gap(cx: &mut VisualTestContext, above: &'static str, below: &'static str) -> gpui::Pixels {
    draw(cx);
    let above = cx.debug_bounds(above).expect("the row above");
    let below = cx.debug_bounds(below).expect("the row below");
    below.top() - above.bottom()
}

fn flyout_labels(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |view, cx| {
        let compose = view.compose().read(cx);
        let flyout = compose.apps_flyout().expect("the flyout is open").read(cx);
        flyout.actions().iter().map(|action| action.label.to_string()).collect()
    })
}

#[test]
fn a_hidden_action_in_the_flyout_shows_its_key_follows_the_box_and_gives_the_keyboard_to_the_text() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    hiding(&view, cx, teams_ui::settings::ComposeTool::Format);
    click(cx, "compose-apps");
    assert!(found(cx, "compose-format-row"));
    assert!(found(cx, "compose-format-row-key"), "the row shows its key");
    assert_eq!(flyout_labels(&view, cx)[0], "Show formatting options");
    // the AI tools' rows stand apart from the Teams action before them
    assert_eq!(flyout_apart(&view, cx), [false, true, false, false]);
    assert!(
        gap(cx, "compose-format-row", "compose-correct-row") > gap(cx, "compose-correct-row", "compose-translate-row"),
        "the flyout draws the line over Correct"
    );
    // the box's state changes while the flyout is open: the row says what its click does now
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_expanded(window, cx)));
    draw(cx);
    assert!(found(cx, "apps-flyout"));
    assert_eq!(flyout_labels(&view, cx)[0], "Hide formatting options");
    // the row taken: the flyout closes and the keyboard is in the text again
    click(cx, "compose-format-row");
    assert!(!found(cx, "apps-flyout"));
    assert!(!compose.read_with(cx, |compose, _| compose.is_expanded()));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn enter_on_the_focused_customize_button_opens_the_list_not_the_lit_row() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    hiding(&view, cx, teams_ui::settings::ComposeTool::Emoji);
    click(cx, "compose-apps");
    assert!(found(cx, "compose-emoji-row"));
    // Tab (the window's next focus) goes from the search field to the flyout's first button, "Customize toolbar"
    cx.update(gpui::Window::focus_next);
    draw(cx);
    // a key press as the platform sends it: a button takes its click on the key's release
    let enter = gpui::Keystroke::parse("enter").unwrap();
    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: enter.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent { keystroke: enter });
    cx.run_until_parked();
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    assert!(
        compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()),
        "the lit row is not taken"
    );
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
}
