//! Synthetic card editor and workflow list interactions: Teams' cards for connector templates, the list's "..." menu,
//! and edits that keep a workflow's name and connection. No real identities, credentials or tenant writes.
#![cfg(test)]
#[expect(dead_code, reason = "shared fake engine helpers used by other view tests")]
mod common;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use teams_proto::{WorkflowRequest, WorkflowResponse};
use teams_store::{Team, TeamChannel};
use teams_ui::workflow_dialog::WorkflowDialog;

#[path = "support/workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;

fn view(
    rig: &common::Rig,
    answer: fn(&WorkflowRequest) -> WorkflowResponse,
) -> (gpui::Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    let channel = TeamChannel {
        id: "synthetic-channel".into(),
        name: "General".into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    let team = Team {
        id: "synthetic-team".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        channels: vec![channel.clone()],
        ..Team::default()
    };
    *rig.core.teams.lock().unwrap() = Some(vec![team.clone()]);
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let (view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new(core, team, channel, window, cx));
    cx.run_until_parked();
    (view, cx)
}

fn window(rig: &common::Rig, answer: fn(&WorkflowRequest) -> WorkflowResponse) -> &'static mut VisualTestContext {
    view(rig, answer).1
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
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Turns the wheel over the dialog's page: a negative `dy` goes down.
fn scroll(cx: &mut VisualTestContext, dy: f32) {
    let page = bounds(cx, "workflow-scroll").expect("the page is drawn");
    cx.simulate_event(gpui::ScrollEvent {
        position: page.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    });
    cx.run_until_parked();
}

fn requests(rig: &common::Rig) -> Vec<WorkflowRequest> {
    rig.core.workflow_requests.lock().unwrap().clone()
}

fn created(rig: &common::Rig) -> Option<serde_json::Value> {
    requests(rig).into_iter().find_map(|request| match request {
        WorkflowRequest::Create { properties, .. } => Some(properties),
        _ => None,
    })
}

#[test]
fn a_planner_template_is_teams_card_listing_the_connectors_choices() {
    let rig = common::Rig::start();
    let cx = window(&rig, fixtures::planner_answer);
    click(cx, "workflow-template-synthetic-planner");
    for shown in [
        "workflow-simple-card",
        "workflow-field-PlannerGroupId",
        "workflow-field-PlannerPlanId",
        "workflow-simple-team",
        "workflow-simple-channel",
    ] {
        assert!(bounds(cx, shown).is_some(), "{shown} is drawn");
    }
    assert!(bounds(cx, "workflow-name").is_none(), "Teams shows no name field");
    assert!(bounds(cx, "workflow-definition-toggle").is_none(), "nor the definition");
    let authorized: Vec<_> = requests(&rig)
        .into_iter()
        .filter_map(|request| match request {
            WorkflowRequest::AuthorizeConnection {
                connector, connection, ..
            } => Some((connector, connection)),
            _ => None,
        })
        .collect();
    assert!(
        authorized.contains(&("shared_planner".to_owned(), "shared-planner-left-over".to_owned())),
        "a Planner connection a failed sign-in left behind is signed in again, not another made: {authorized:?}"
    );
    assert_eq!(
        authorized
            .iter()
            .filter(|(connector, _)| connector == "shared_planner")
            .count(),
        1
    );
    click(cx, "workflow-field-PlannerPlanId");
    assert!(
        bounds(cx, "workflow-field-PlannerPlanId-synthetic-plan").is_none(),
        "the plans wait for a group"
    );
    click(cx, "workflow-field-PlannerGroupId");
    click(cx, "workflow-field-PlannerGroupId-synthetic-group");
    click(cx, "workflow-field-PlannerPlanId");
    click(cx, "workflow-field-PlannerPlanId-synthetic-plan");
    click(cx, "workflow-save");
    let created = created(&rig).expect("Save creates the workflow");
    assert_eq!(
        created["displayName"],
        "Notify General when a Planner task is completed in Synthetic plan"
    );
    let trigger = &created["definition"]["triggers"]["When_a_task_is_completed"]["inputs"]["parameters"];
    assert_eq!(trigger["groupId"], "synthetic-group");
    assert_eq!(trigger["id"], "synthetic-plan");
    assert_eq!(
        created["connectionReferences"]["shared_planner"]["connectionName"],
        "shared-planner-left-over"
    );
    assert!(
        created["connectionReferences"]["shared_teams"]["connectionName"]
            .as_str()
            .is_some_and(|name| name.starts_with("shared-teams-"))
    );
    let list = requests(&rig).into_iter().any(|request| {
        matches!(&request, WorkflowRequest::Choices { body, tree: false, .. }
            if body["parameters"]["groupId"] == "synthetic-group")
    });
    assert!(list, "the plans are listed for the chosen group");
}

#[test]
fn a_connector_definition_that_could_not_be_read_says_so_and_is_asked_again() {
    fn failing(request: &WorkflowRequest) -> WorkflowResponse {
        match request {
            WorkflowRequest::Operation { .. } => Err(teams_proto::WorkflowFailure::Service(503)),
            other => fixtures::planner_answer(other),
        }
    }
    let rig = common::Rig::start();
    let cx = window(&rig, failing);
    click(cx, "workflow-template-synthetic-planner");
    click(cx, "workflow-field-PlannerGroupId");
    assert!(
        bounds(cx, "workflow-choices-failed").is_some(),
        "the field says its options could not be loaded, not that there are none"
    );
    let operations = |rig: &common::Rig| {
        requests(rig)
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::Operation { .. }))
            .count()
    };
    let before = operations(&rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::planner_answer);
    click(cx, "workflow-field-PlannerPlanId");
    click(cx, "workflow-field-PlannerGroupId");
    assert_eq!(operations(&rig), before + 1, "opening a failed field asks again, once");
    click(cx, "workflow-field-PlannerGroupId-synthetic-group");
    click(cx, "workflow-field-PlannerPlanId");
    assert!(bounds(cx, "workflow-field-PlannerPlanId-synthetic-plan").is_some());
}

#[test]
fn an_edit_keeps_the_workflows_name_and_its_own_connection() {
    let rig = common::Rig::start();
    let cx = window(&rig, fixtures::saved_webhook_answer);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-edit");
    assert!(bounds(cx, "workflow-simple-card").is_some());
    click(cx, "workflow-save");
    let updated = requests(&rig)
        .into_iter()
        .find_map(|request| match request {
            WorkflowRequest::Update { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("Save updates the workflow");
    assert_eq!(
        updated["displayName"], "Renamed by hand",
        "Teams keeps a renamed workflow's name"
    );
    assert_eq!(
        updated["connectionReferences"]["shared_teams"]["connectionName"], "own-connection",
        "the workflow keeps its own connection"
    );
    assert!(
        !requests(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::AuthorizeConnection { .. })),
        "no connection is made for it"
    );
    assert!(
        bounds(cx, "workflow-toggle").is_some(),
        "Save from the Overview's Edit returns to it"
    );
}

#[test]
fn the_rows_menu_edits_and_turns_off_from_the_list_and_details_opens_the_overview() {
    let rig = common::Rig::start();
    let cx = window(&rig, fixtures::saved_webhook_answer);
    assert!(
        bounds(cx, "workflow-open-synthetic-flow").is_some(),
        "the channel lists its workflow"
    );
    click(cx, "workflow-row-more-synthetic-flow");
    click(cx, "workflow-row-edit");
    assert!(bounds(cx, "workflow-simple-card").is_some());
    click(cx, "workflow-save");
    assert!(
        bounds(cx, "workflow-search").is_some(),
        "an edit from the list comes back to the list"
    );
    click(cx, "workflow-row-more-synthetic-flow");
    click(cx, "workflow-row-toggle");
    assert!(requests(&rig).iter().any(|request| matches!(
        request,
        WorkflowRequest::SetEnabled { enabled: false, flow, .. } if flow == "synthetic-flow"
    )));
    assert!(bounds(cx, "workflow-search").is_some(), "Turn off stays on the list");
    click(cx, "workflow-row-more-synthetic-flow");
    click(cx, "workflow-row-details");
    assert!(bounds(cx, "workflow-toggle").is_some(), "Details is the Overview");
}

#[test]
fn delete_from_the_rows_menu_asks_over_the_list_and_stays_there() {
    let rig = common::Rig::start();
    let cx = window(&rig, fixtures::saved_webhook_answer);
    click(cx, "workflow-row-more-synthetic-flow");
    click(cx, "workflow-row-delete");
    assert!(bounds(cx, "workflow-delete-dialog").is_some());
    click(cx, "workflow-delete-cancel");
    assert!(bounds(cx, "workflow-delete-dialog").is_none());
    assert!(
        !requests(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Delete { .. })),
        "Cancel deletes nothing"
    );
    click(cx, "workflow-row-more-synthetic-flow");
    click(cx, "workflow-row-delete");
    click(cx, "workflow-delete-confirm");
    assert!(requests(&rig).iter().any(|request| matches!(
        request,
        WorkflowRequest::Delete { flow, .. } if flow == "synthetic-flow"
    )));
    assert!(bounds(cx, "workflow-search").is_some(), "the list stays");
}

#[test]
fn a_turned_off_workflow_is_not_listed_in_the_channel() {
    fn stopped(request: &WorkflowRequest) -> WorkflowResponse {
        match request {
            WorkflowRequest::Get { .. } => {
                let mut flow = fixtures::saved_webhook_flow();
                flow["properties"]["state"] = serde_json::json!("Stopped");
                Ok(flow)
            }
            other => fixtures::saved_webhook_answer(other),
        }
    }
    let rig = common::Rig::start();
    let cx = window(&rig, stopped);
    assert!(
        bounds(cx, "workflow-open-synthetic-flow").is_none(),
        "Teams' channel list shows the workflows that are on"
    );
    assert!(bounds(cx, "workflow-view-templates").is_some());
}

#[test]
fn a_co_owner_search_lists_only_people_starting_with_the_text() {
    let rig = common::Rig::start();
    let person = |id: &str, name: &str, email: &str| teams_proto::TeamPerson {
        mri: format!("8:orgid:{id}"),
        name: name.into(),
        email: Some(email.into()),
        job_title: None,
    };
    *rig.core.addable.lock().unwrap() = vec![
        person(
            "00000000-0000-4000-8000-000000000002",
            "Synthetic Two",
            "two@example.invalid",
        ),
        person(
            "00000000-0000-4000-8000-000000000003",
            "Synthetic Three",
            "three@example.invalid",
        ),
    ];
    let (view, cx) = view(&rig, fixtures::saved_webhook_answer);
    click(cx, "workflow-open-synthetic-flow");
    view.update_in(cx, |dialog, window, cx| dialog.search_co_owner("two", window, cx));
    assert!(bounds(cx, "workflow-owner-pick-8:orgid:00000000-0000-4000-8000-000000000002").is_some());
    assert!(
        bounds(cx, "workflow-owner-pick-8:orgid:00000000-0000-4000-8000-000000000003").is_none(),
        "a name or address not starting with the text isn't listed"
    );
    assert!(bounds(cx, "workflow-owners-learn-more").is_some());
}

#[test]
fn the_weekly_reminder_is_teams_schedule_card() {
    let rig = common::Rig::start();
    let cx = window(&rig, fixtures::cards_answer);
    click(cx, "workflow-template-synthetic-template");
    for shown in [
        "workflow-field-BuiltInInterval",
        "workflow-field-BuiltInStartDate",
        "workflow-field-BuiltInTimeOfDay",
        "workflow-field-BuiltInTimezone",
        "workflow-day-Monday",
        "workflow-field-TeamsMessageBody",
    ] {
        assert!(bounds(cx, shown).is_some(), "{shown} is drawn");
    }
    assert!(
        bounds(cx, "workflow-field-BuiltInFrequency").is_none(),
        "the frequency is fixed"
    );
    click(cx, "workflow-day-Wednesday");
    click(cx, "workflow-field-BuiltInInterval");
    cx.simulate_keystrokes("ctrl-a backspace");
    cx.simulate_input("0");
    scroll(cx, -2_000.0);
    click(cx, "workflow-field-TeamsMessageBody");
    cx.simulate_input("Synthetic reminder");
    click(cx, "workflow-save");
    assert!(created(&rig).is_none(), "Save stays off for an interval Teams refuses");
    scroll(cx, 2_000.0);
    click(cx, "workflow-field-BuiltInInterval");
    cx.simulate_keystrokes("ctrl-a backspace");
    cx.simulate_input("2");
    click(cx, "workflow-save");
    let created = created(&rig).expect("Save creates the workflow");
    let recurrence = &created["definition"]["triggers"]["Trigger"]["recurrence"];
    assert_eq!(recurrence["interval"], 2);
    assert_eq!(recurrence["frequency"], "Week");
    assert_eq!(
        recurrence["schedule"]["weekDays"],
        serde_json::json!(["Monday", "Wednesday"])
    );
    assert!(recurrence["schedule"]["hours"].is_array() && recurrence["schedule"]["minutes"].is_array());
    assert!(recurrence["timeZone"].as_str().is_some_and(|zone| !zone.is_empty()));
    assert!(
        recurrence["startTime"]
            .as_str()
            .is_some_and(|start| start.ends_with("T00:00:00"))
    );
    assert_eq!(
        created["definition"]["actions"]["Post"]["inputs"]["parameters"]["body/messageBody"],
        "Synthetic reminder"
    );
    assert!(
        created["displayName"]
            .as_str()
            .is_some_and(|name| name.starts_with("Send a message to General every 2 week(s) on selected days at "))
    );
}

#[test]
fn the_email_card_picks_a_mail_folder_and_people() {
    let rig = common::Rig::start();
    *rig.core.addable.lock().unwrap() = vec![
        teams_proto::TeamPerson {
            mri: "8:orgid:00000000-0000-4000-8000-000000000002".into(),
            name: "Synthetic Two".into(),
            email: Some("two@example.invalid".into()),
            job_title: None,
        },
        teams_proto::TeamPerson {
            mri: "8:orgid:00000000-0000-4000-8000-000000000003".into(),
            name: "Synthetic Three".into(),
            email: Some("three@example.invalid".into()),
            job_title: None,
        },
    ];
    let cx = window(&rig, fixtures::cards_answer);
    click(cx, "workflow-template-synthetic-email");
    click(cx, "workflow-field-OutlookMailFolderPath");
    assert!(bounds(cx, "workflow-field-OutlookMailFolderPath-Inbox").is_some());
    click(cx, "workflow-field-OutlookMailFolderPath-Archive");
    click(cx, "workflow-field-OutlookFrom");
    cx.simulate_input("two");
    assert!(
        bounds(cx, "workflow-person-pick-8:orgid:00000000-0000-4000-8000-000000000003").is_none(),
        "only people starting with the text"
    );
    click(cx, "workflow-person-pick-8:orgid:00000000-0000-4000-8000-000000000002");
    assert!(
        bounds(cx, "workflow-person-two@example.invalid").is_some(),
        "the person shows as a chip"
    );
    click(cx, "workflow-save");
    let created = created(&rig).expect("Save creates the workflow");
    let trigger = &created["definition"]["triggers"]["Trigger"]["inputs"]["parameters"];
    assert_eq!(trigger["folderPath"], "Id::synthetic-archive");
    assert_eq!(trigger["from"], "two@example.invalid");
    assert!(
        trigger.get("to").is_none(),
        "an empty optional field's input is taken out"
    );
    assert_eq!(
        created["displayName"],
        "Forward the message to General when the email meets your selected conditions"
    );
}
