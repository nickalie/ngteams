//! Workflows run from a message over synthetic data, as browser Teams has them: a template with no choices shows its
//! instructions card and Save; the workflow's Overview says how to run it; a message's "More options" lists it and
//! runs it on the message through its own question card; "More actions" → "Create workflows" opens the Workflows
//! dialog for the message. Also the list's next runs and a kept message's text. No real identities, credentials or
//! tenant writes.
#![cfg(test)]
#[expect(dead_code, reason = "shared fake engine helpers used by other view tests")]
mod common;
#[path = "support/workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;
#[path = "support/workflow_message_fixtures.rs"]
mod message_fixtures;
#[path = "common/parts.rs"]
mod parts;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{LAB_A, LAB_B};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use message_fixtures::{FOLLOW, failing_answer, message_answer};
use serde_json::{Value, json};
use teams_proto::{MessageSender, WorkflowFailure, WorkflowRequest, WorkflowResponse};
use teams_store::{Message, Team, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, ChatViewEvent};
use teams_ui::settings::Settings;
use teams_ui::team_dialogs::TeamDialogEvent;
use teams_ui::workflow_dialog::{FlowRef, WorkflowDialog};
use teams_ui::workflow_run::{MessageRef, WorkflowRun};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    draw(cx);
}

fn asked(rig: &common::Rig) -> Vec<WorkflowRequest> {
    rig.core.workflow_requests.lock().unwrap().clone()
}

fn team() -> (Team, TeamChannel) {
    (
        Team {
            id: "synthetic-team".into(),
            group_id: Some("synthetic-group".into()),
            name: "Synthetic team".into(),
            ..Team::default()
        },
        TeamChannel {
            id: "synthetic-channel".into(),
            name: "General".into(),
            general: true,
            kind: teams_store::ChannelKind::Standard,
            shown: true,
            about: teams_store::ChannelAbout::default(),
        },
    )
}

fn dialog(
    rig: &common::Rig,
    answer: fn(&WorkflowRequest) -> WorkflowResponse,
    message: Option<MessageRef>,
) -> (Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let (team, channel) = team();
    let (view, cx) = app.add_window_view(|window, cx| match message {
        Some(message) => WorkflowDialog::new_for_message(core, Some((team, channel)), message, window, cx),
        None => WorkflowDialog::new(core, team, channel, window, cx),
    });
    cx.simulate_resize(size(px(1200.0), px(900.0)));
    draw(cx);
    (view, cx)
}

fn stored(id: &str, sender: &str, content: &str) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Lab Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: 1_767_268_800_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn chat_message() -> MessageRef {
    MessageRef {
        conversation: AB.to_owned(),
        id: "1".to_owned(),
        root: None,
        in_channel: false,
        team: None,
    }
}

#[test]
fn a_template_run_from_a_message_shows_its_instructions_and_saves_as_teams_does() {
    let rig = common::Rig::start();
    let (view, cx) = dialog(&rig, message_answer, None);
    cx.update(|window, cx| view.update(cx, |view, cx| view.select_template(FOLLOW, window, cx)));
    draw(cx);
    assert!(
        bounds(cx, "workflow-message-instructions").is_some(),
        "the instructions card"
    );
    for absent in ["workflow-card-toggle", "workflow-collapse-all"] {
        assert!(bounds(cx, absent).is_none(), "{absent}: the card has no fields to fold");
    }
    click(cx, "workflow-save");
    let created = asked(&rig)
        .into_iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("Save creates the workflow");
    assert_eq!(created["templateName"], FOLLOW);
    assert_eq!(created["displayName"], "Follow up on a message");
    assert_eq!(
        created["connectionReferences"]["shared_teams"]["connectionName"],
        "own-teams"
    );
    assert!(created["definition"]["actions"]["Do_Not_Remove_FlowIL"].is_object());
    // the Overview says how to run it; there is nothing to edit
    assert!(bounds(cx, "workflow-how-to-run").is_some());
    click(cx, "workflow-edit");
    assert!(bounds(cx, "workflow-simple-card").is_none(), "Edit stays off");
}

#[test]
fn the_dialog_for_a_message_lists_its_workflows_and_runs_one_on_it() {
    let rig = common::Rig::start();
    let (view, cx) = dialog(&rig, message_answer, Some(chat_message()));
    assert!(
        asked(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Gallery { category, .. } if category == "home_featured")),
        "the templates are the gallery's, those run from a message first"
    );
    assert!(bounds(cx, "workflow-template-synthetic-follow").is_some());
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TeamDialogEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    cx.update(|_, cx| view.update(cx, WorkflowDialog::scroll_to_end));
    click(cx, "workflow-row-run-synthetic-follow-flow");
    assert_eq!(
        *events.borrow(),
        [TeamDialogEvent::RunWorkflow {
            flow: FlowRef {
                environment: "Default-synthetic".into(),
                id: "synthetic-follow-flow".into(),
                name: "Follow up on a message".into(),
            },
            message: chat_message(),
        }]
    );
}

fn run_window(
    rig: &common::Rig,
    answer: fn(&WorkflowRequest) -> WorkflowResponse,
) -> (
    Entity<WorkflowRun>,
    Rc<RefCell<Vec<TeamDialogEvent>>>,
    &'static mut VisualTestContext,
) {
    run_window_on(rig, answer, stored("1", LAB_B, "<p>Synthetic <b>post</b></p>"))
}

fn run_window_on(
    rig: &common::Rig,
    answer: fn(&WorkflowRequest) -> WorkflowResponse,
    message: Message,
) -> (
    Entity<WorkflowRun>,
    Rc<RefCell<Vec<TeamDialogEvent>>>,
    &'static mut VisualTestContext,
) {
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), vec![message]);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let flow = FlowRef {
        environment: "Default-synthetic".into(),
        id: "synthetic-follow-flow".into(),
        name: "Follow up on a message".into(),
    };
    let (view, cx) = app.add_window_view(|window, cx| WorkflowRun::new(core, flow, chat_message(), window, cx));
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TeamDialogEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    draw(cx);
    (view, events, cx)
}

fn type_in(cx: &mut VisualTestContext, selector: &str, text: &str) {
    click(cx, selector);
    cx.simulate_input(text);
    draw(cx);
}

#[test]
fn a_run_marks_unanswered_or_unreadable_questions_and_sends_nothing() {
    let rig = common::Rig::start();
    let (_view, _events, cx) = run_window(&rig, message_answer);
    assert!(bounds(cx, "workflow-run-title").is_some());
    assert!(bounds(cx, "workflow-run-input-when").is_some());
    // an unanswered required question says so, and nothing runs
    click(cx, "workflow-run-submit");
    assert!(bounds(cx, "workflow-run-missing-note").is_some());
    // a time that reads as none is marked too
    type_in(cx, "workflow-run-input-note", "Synthetic note");
    type_in(cx, "workflow-run-input-when", "soon");
    click(cx, "workflow-run-submit");
    assert!(bounds(cx, "workflow-run-missing-when").is_some());
    let runs = asked(&rig)
        .iter()
        .filter(|request| matches!(request, WorkflowRequest::RunOnMessage { .. }))
        .count();
    assert_eq!(runs, 0);
}

/// The run the dialog sent: its answers and its message.
fn sent_run(rig: &common::Rig) -> (serde_json::Map<String, Value>, teams_proto::MessageRun) {
    asked(rig)
        .into_iter()
        .find_map(|request| match request {
            WorkflowRequest::RunOnMessage {
                answers,
                message,
                environment,
                flow,
                trigger,
            } if environment == "Default-synthetic" && flow == "synthetic-follow-flow" && trigger == "manual" => {
                Some((answers, *message))
            }
            _ => None,
        })
        .expect("the run goes")
}

#[test]
fn a_run_goes_once_on_the_message_with_the_cards_answers() {
    let rig = common::Rig::start();
    let (_view, events, cx) = run_window(&rig, message_answer);
    type_in(cx, "workflow-run-input-note", "Synthetic note");
    // a time as typed in Teams' words goes as the card's 24-hour time
    type_in(cx, "workflow-run-input-when", "8:30 PM");
    click(cx, "workflow-run-input-tell");
    click(cx, "workflow-run-submit");
    let (answers, message) = sent_run(&rig);
    assert_eq!(
        Value::Object(answers),
        json!({"note": "Synthetic note", "when": "20:30", "tell": "true"})
    );
    assert_eq!((message.id.as_str(), message.conversation.as_str()), ("1", AB));
    assert_eq!(message.reply_to, None);
    assert_eq!(message.text, "Synthetic post");
    assert_eq!(message.html, "<p>Synthetic <b>post</b></p>");
    assert!(matches!(&message.sender, MessageSender::User { name, .. } if name == "Lab Bee"));
    assert_eq!(message.actor, LAB_A);
    assert_eq!(message.tenant.as_deref(), Some("synthetic"));
    assert_eq!(
        *events.borrow(),
        [TeamDialogEvent::Closed],
        "the dialog closes once the run went"
    );
}

#[test]
fn a_failed_run_keeps_the_dialog_and_says_so() {
    let rig = common::Rig::start();
    let (_view, events, cx) = run_window(&rig, failing_answer);
    type_in(cx, "workflow-run-input-note", "Synthetic note");
    click(cx, "workflow-run-submit");
    assert!(bounds(cx, "workflow-run-error").is_some());
    assert!(events.borrow().is_empty());
    let runs = asked(&rig)
        .iter()
        .filter(|request| matches!(request, WorkflowRequest::RunOnMessage { .. }))
        .count();
    assert_eq!(runs, 1, "a failed run is never sent again by itself");
}

/// As [`message_answer`], the workflow's questions inside a container and a column set, as the designer nests them.
fn nested_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::Get { .. } => Ok(message_fixtures::message_flow(
            message_fixtures::FOLLOW_FLOW,
            "Follow up on a message",
            &json!({"type":"AdaptiveCard","version":"1.3","body":[
                {"type":"Container","items":[
                    {"type":"TextBlock","text":"Synthetic heading"},
                    {"type":"Input.Text","id":"note","label":"Note:","isRequired":true}
                ]},
                {"type":"ColumnSet","columns":[{"type":"Column","items":[
                    {"type":"Input.Toggle","id":"tell","title":"Notify myself"}
                ]}]}
            ]}),
        )),
        other => message_answer(other),
    }
}

/// As [`message_answer`], the workflow asking a required question of a kind the app doesn't draw.
fn undrawn_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::Get { .. } => Ok(message_fixtures::message_flow(
            message_fixtures::FOLLOW_FLOW,
            "Follow up on a message",
            &json!({"type":"AdaptiveCard","version":"1.3","body":[
                {"type":"Container","items":[{"type":"Input.Rating","id":"stars","isRequired":true}]}
            ]}),
        )),
        other => message_answer(other),
    }
}

/// As [`message_answer`], the workflow asking for a choice, several days, people from the directory and a time.
fn choices_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::Get { .. } => Ok(message_fixtures::message_flow(
            message_fixtures::FOLLOW_FLOW,
            "Follow up on a message",
            &json!({"type":"AdaptiveCard","version":"1.3","body":[
                {"type":"Input.ChoiceSet","id":"colour","label":"Colour","isRequired":true,
                 "errorMessage":"Pick a colour","choices":[
                    {"title":"Red","value":"red"},{"title":"Blue","value":"blue"}]},
                {"type":"Input.ChoiceSet","id":"days","isMultiSelect":true,"value":"mon","choices":[
                    {"title":"Mon","value":"mon"},{"title":"Tue","value":"tue"},{"title":"Wed","value":"wed"}]},
                {"type":"Input.ChoiceSet","id":"who","label":"Who","isMultiSelect":true,"choices":[],
                 "choices.data":{"type":"Data.Query","dataset":"graph.microsoft.com/users"}},
                {"type":"Input.Time","id":"at"}
            ]}),
        )),
        other => message_answer(other),
    }
}

fn person(mri: &str, name: &str) -> teams_proto::TeamPerson {
    teams_proto::TeamPerson {
        mri: mri.to_owned(),
        name: name.to_owned(),
        email: Some(format!("{}@example.invalid", name.to_lowercase().replace(' ', "."))),
        job_title: None,
    }
}

#[test]
fn a_cards_choices_people_and_listed_times_answer_as_teams_sends_them() {
    const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
    let rig = common::Rig::start();
    *rig.core.addable.lock().unwrap() = vec![
        person(LAB_B, "Lab Bee"),
        person(LAB_C, "Lab Cee"),
        person(LAB_A, "Other"),
    ];
    let (_view, _events, cx) = run_window(&rig, choices_answer);
    // a required choice left unpicked is marked with the card's own words
    click(cx, "workflow-run-submit");
    assert!(bounds(cx, "workflow-run-missing-colour").is_some());
    // one of a single choice: the last picked
    click(cx, "workflow-run-input-colour-blue");
    click(cx, "workflow-run-input-colour-red");
    assert!(
        bounds(cx, "workflow-run-missing-colour").is_none(),
        "picking clears the mark"
    );
    // several: each pick flips its own, the card's own value picked to start with
    click(cx, "workflow-run-input-days-wed");
    click(cx, "workflow-run-input-days-mon");
    click(cx, "workflow-run-input-days-tue");
    // people: the directory searched, the found picked (once each), one removed again
    type_in(cx, "workflow-run-input-who", "lab");
    assert!(bounds(cx, &format!("workflow-run-input-who-found-{LAB_B}")).is_some());
    assert!(
        bounds(cx, &format!("workflow-run-input-who-found-{LAB_A}")).is_none(),
        "only those whose name or address starts with the words"
    );
    click(cx, &format!("workflow-run-input-who-found-{LAB_B}"));
    assert!(
        bounds(cx, &format!("workflow-run-input-who-found-{LAB_C}")).is_none(),
        "a pick clears the search"
    );
    type_in(cx, "workflow-run-input-who", "lab c");
    click(cx, &format!("workflow-run-input-who-found-{LAB_C}"));
    type_in(cx, "workflow-run-input-who", "lab");
    click(cx, &format!("workflow-run-input-who-found-{LAB_C}"));
    click(cx, &format!("workflow-run-input-who-remove-{LAB_B}"));
    assert!(bounds(cx, &format!("workflow-run-input-who-remove-{LAB_B}")).is_none());
    // a search cleared finds nobody
    type_in(cx, "workflow-run-input-who", "x");
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert!(bounds(cx, &format!("workflow-run-input-who-found-{LAB_B}")).is_none());
    // a time from the list under the chevron
    click(cx, "workflow-run-input-at-times");
    click(cx, "workflow-run-time-1230AM");
    click(cx, "workflow-run-submit");
    let (answers, _) = sent_run(&rig);
    assert_eq!(
        Value::Object(answers),
        json!({
            "colour": "red",
            "days": "wed,tue",
            "who": "00000000-0000-4000-8000-00000000000c",
            "at": "00:30"
        })
    );
}

#[test]
fn a_cards_nested_questions_are_asked_and_answered() {
    let rig = common::Rig::start();
    let (_view, _events, cx) = run_window(&rig, nested_answer);
    assert!(
        bounds(cx, "workflow-run-input-note").is_some(),
        "the container's question"
    );
    assert!(bounds(cx, "workflow-run-input-tell").is_some(), "the column's question");
    type_in(cx, "workflow-run-input-note", "Synthetic note");
    click(cx, "workflow-run-submit");
    let (answers, _) = sent_run(&rig);
    assert_eq!(
        Value::Object(answers),
        json!({"note": "Synthetic note", "tell": "false"})
    );
}

#[test]
fn a_card_with_a_required_question_the_app_cant_draw_says_so_and_offers_no_submit() {
    let rig = common::Rig::start();
    let (_view, _events, cx) = run_window(&rig, undrawn_answer);
    assert!(bounds(cx, "workflow-run-failed").is_some());
    assert!(bounds(cx, "workflow-run-submit").is_none());
}

#[test]
fn a_plain_text_message_goes_as_its_text_and_its_html_escaped() {
    let rig = common::Rig::start();
    let mut plain = stored("1", LAB_B, "a <b> tag & more");
    plain.message_type = "Text".to_owned();
    let (_view, _events, cx) = run_window_on(&rig, message_answer, plain);
    type_in(cx, "workflow-run-input-note", "Synthetic note");
    click(cx, "workflow-run-submit");
    let (_, message) = sent_run(&rig);
    assert_eq!(message.text, "a <b> tag & more");
    assert_eq!(message.html, "a &lt;b&gt; tag &amp; more");
}

#[test]
fn a_read_only_app_offers_no_submit() {
    let rig = common::Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (_view, _events, cx) = run_window(&rig, message_answer);
    assert!(bounds(cx, "workflow-run-submit").is_none());
}

fn chat(
    rig: &common::Rig,
) -> (
    Entity<ChatView>,
    Rc<RefCell<Vec<ChatViewEvent>>>,
    &'static mut VisualTestContext,
) {
    *rig.core.workflow_handler.lock().unwrap() = Some(message_answer);
    rig.core
        .messages
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![stored("1", LAB_B, "<p>Synthetic post</p>")]);
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(900.0), px(700.0)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    draw(cx);
    (view, events, cx)
}

fn open_menu(cx: &mut VisualTestContext) {
    let row = bounds(cx, "message-1").expect("the message is drawn");
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    click(cx, "message-more");
}

#[test]
fn a_messages_menu_lists_the_workflows_run_from_a_message_and_runs_one_on_it() {
    let rig = common::Rig::start();
    let (_view, events, cx) = chat(&rig);
    open_menu(cx);
    assert!(bounds(cx, "message-workflow-synthetic-follow-flow").is_some());
    for other in ["message-workflow-synthetic-weekly", "message-workflow-synthetic-off"] {
        assert!(bounds(cx, other).is_none(), "{other}: not run from a message, or off");
    }
    click(cx, "message-workflow-synthetic-follow-flow");
    assert_eq!(
        *events.borrow(),
        [ChatViewEvent::RunWorkflow {
            flow: FlowRef {
                environment: "Default-synthetic".into(),
                id: "synthetic-follow-flow".into(),
                name: "Follow up on a message".into(),
            },
            message: chat_message(),
        }]
    );
    assert!(bounds(cx, "message-menu").is_none(), "the menu closes");
}

#[test]
fn more_actions_holds_create_workflows() {
    let rig = common::Rig::start();
    let (_view, events, cx) = chat(&rig);
    open_menu(cx);
    click(cx, "message-more-actions");
    assert!(bounds(cx, "message-more-actions-menu").is_some());
    click(cx, "message-create-workflows");
    assert_eq!(*events.borrow(), [ChatViewEvent::CreateWorkflows(chat_message())]);
}

/// Twelve workflows run from a message.
fn many_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::List { .. } => {
            let flows: Vec<Value> = (0..12)
                .map(|at| {
                    json!({"name": format!("synthetic-many-{at}"), "properties":{
                        "displayName": format!("Synthetic many {at}"), "state":"Started",
                        "definitionSummary":{"triggers":[{"type":"Request","kind":"ApiConnection",
                            "swaggerOperationId":"ForASelectedMessageV2"}]}}})
                })
                .collect();
            Ok(json!({ "value": flows }))
        }
        other => message_answer(other),
    }
}

#[test]
fn a_tall_more_actions_list_fits_the_window_and_leaves_the_menu_in_it() {
    let rig = common::Rig::start();
    let (_view, _events, cx) = chat(&rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(many_answer);
    cx.simulate_resize(size(px(900.0), px(260.0)));
    draw(cx);
    open_menu(cx);
    let menu = bounds(cx, "message-menu").expect("the menu");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: menu.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-3000.0))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollWheelEvent::default()
    });
    draw(cx);
    click(cx, "message-more-actions");
    draw(cx);
    let window = gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), size(px(900.0), px(260.0)));
    let list = bounds(cx, "message-more-actions-menu").expect("the list");
    let menu = bounds(cx, "message-menu").expect("the menu");
    assert!(list.size.height <= px(260.0), "the list fits the window: {list:?}");
    assert!(
        menu.bottom() <= window.bottom() && menu.top() >= window.top(),
        "the menu stays in the window: {menu:?}"
    );
}

/// The 12 workflows' menu open in a 260 px window with client-side decorations (a 20 px inset), "More actions"' list
/// beside it.
fn tall_more_actions(
    rig: &common::Rig,
) -> (
    Entity<ChatView>,
    Rc<RefCell<Vec<ChatViewEvent>>>,
    &'static mut VisualTestContext,
) {
    let (view, events, cx) = chat(rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(many_answer);
    cx.simulate_resize(size(px(900.0), px(260.0)));
    cx.update(|window, _| window.set_client_inset(px(20.0)));
    draw(cx);
    open_menu(cx);
    let menu = bounds(cx, "message-menu").expect("the menu");
    wheel(cx, menu.center(), 3000.0);
    click(cx, "message-more-actions");
    (view, events, cx)
}

fn wheel(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, down: f32) {
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-down))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollWheelEvent::default()
    });
    draw(cx);
}

/// The inside of the 260 px window: its 20 px inset and the 8 px snap margin.
fn snapped(at: gpui::Bounds<gpui::Pixels>) -> bool {
    at.top() >= px(28.0) && at.bottom() <= px(232.0)
}

#[test]
fn the_wheel_brings_a_clipped_more_actions_row_into_view_and_a_click_on_it_runs_it() {
    let rig = common::Rig::start();
    let (_view, events, cx) = tall_more_actions(&rig);
    let list = bounds(cx, "message-more-actions-menu").expect("the list");
    assert!(snapped(list), "the list stays in the window inside its inset: {list:?}");
    let create = bounds(cx, "message-create-workflows").expect("the row");
    assert!(
        create.top() >= list.bottom(),
        "the rows keep their height: the last is out of view: {create:?}"
    );
    wheel(cx, list.center(), 3000.0);
    let create = bounds(cx, "message-create-workflows").expect("the row");
    assert!(snapped(create), "the wheel brings it into view: {create:?}");
    click(cx, "message-create-workflows");
    assert_eq!(*events.borrow(), [ChatViewEvent::CreateWorkflows(chat_message())]);
}

#[test]
fn the_keys_scroll_the_more_actions_list_to_their_row() {
    let rig = common::Rig::start();
    let (view, _events, cx) = chat(&rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(many_answer);
    cx.simulate_resize(size(px(900.0), px(260.0)));
    cx.update(|window, _| window.set_client_inset(px(20.0)));
    cx.update(|window, cx| {
        let compose = view.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.focus(window, cx));
    });
    draw(cx);
    // Escape out of the box and Up select the message; the menu's last row is Customize, the one above it More
    // actions; Right opens its list, the keys on its first row
    for key in ["escape", "up", "shift-f10", "end", "up", "right", "end"] {
        cx.simulate_keystrokes(key);
        draw(cx);
    }
    let create = bounds(cx, "message-create-workflows").expect("the list is open");
    assert!(snapped(create), "End scrolls the list to its last row: {create:?}");
    cx.simulate_keystrokes("down");
    draw(cx);
    let first = bounds(cx, "message-workflow-synthetic-many-3").expect("the list's first workflow");
    assert!(
        snapped(first),
        "Down past the end wraps the list back to its first row: {first:?}"
    );
}

#[test]
fn a_read_only_app_offers_no_workflow_on_a_message() {
    let rig = common::Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (_view, _events, cx) = chat(&rig);
    open_menu(cx);
    assert!(bounds(cx, "message-more-actions").is_none());
    assert!(
        !asked(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::List { .. })),
        "nothing is read for a menu that offers no workflow"
    );
}

static NEXT_RUN_GONE: AtomicBool = AtomicBool::new(false);

/// A weekly workflow whose triggers name its next run, until [`NEXT_RUN_GONE`]: then the read fails.
fn scheduled_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::List { .. } => Ok(json!({"value":[{"name":"synthetic-weekly"}]})),
        WorkflowRequest::Get { .. } => Ok(json!({"name":"synthetic-weekly","properties":{
            "displayName":"Synthetic weekly","state":"Started",
            "definition":{"triggers":{"Recurrence":{"type":"Recurrence"}},"actions":{}}}})),
        WorkflowRequest::Triggers { .. } if NEXT_RUN_GONE.load(Ordering::SeqCst) => Err(WorkflowFailure::Service(503)),
        WorkflowRequest::Triggers { .. } => Ok(json!({"value":[{"name":"Recurrence","properties":{
            "nextExecutionTime":"2026-01-05T09:00:00Z"}}]})),
        other => fixtures::answer(other),
    }
}

#[test]
fn a_next_run_that_cant_be_read_again_is_no_longer_shown() {
    let rig = common::Rig::start();
    NEXT_RUN_GONE.store(false, Ordering::SeqCst);
    let (view, cx) = dialog(&rig, scheduled_answer, None);
    let next =
        |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).next_run("synthetic-weekly").map(str::to_owned));
    assert!(next(cx).is_some(), "the next run is shown");
    NEXT_RUN_GONE.store(true, Ordering::SeqCst);
    cx.update(|_, cx| view.update(cx, WorkflowDialog::refresh));
    draw(cx);
    assert_eq!(next(cx), None, "a failed read takes it back");
}

/// Two pages of weekly workflows.
fn paged_answer(request: &WorkflowRequest) -> WorkflowResponse {
    let weekly = |id: &str| {
        json!({"name": id, "properties":{"displayName": id, "state":"Started",
            "definition":{"triggers":{"Recurrence":{"type":"Recurrence"}},"actions":{}}}})
    };
    match request {
        WorkflowRequest::List { .. } => Ok(json!({"value":[{"name":"synthetic-first"}],
            "nextLink":"https://api.flow.microsoft.com/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows?api-version=2016-11-01&$skiptoken=synthetic"})),
        WorkflowRequest::Page { .. } => Ok(json!({"value":[{"name":"synthetic-second"}]})),
        WorkflowRequest::Get { flow, .. } => Ok(weekly(flow)),
        WorkflowRequest::Triggers { .. } => Ok(json!({"value":[]})),
        other => fixtures::answer(other),
    }
}

#[test]
fn load_more_reads_only_its_own_rows_next_runs() {
    let rig = common::Rig::start();
    let (view, cx) = dialog(&rig, paged_answer, None);
    let triggers = |rig: &common::Rig, wanted: &str| {
        asked(rig)
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::Triggers { flow, .. } if flow == wanted))
            .count()
    };
    assert_eq!(triggers(&rig, "synthetic-first"), 1);
    cx.update(|_, cx| view.update(cx, WorkflowDialog::scroll_to_end));
    click(cx, "workflow-load-more");
    assert_eq!(triggers(&rig, "synthetic-second"), 1);
    assert_eq!(
        triggers(&rig, "synthetic-first"),
        1,
        "the rows already listed aren't read again"
    );
}
