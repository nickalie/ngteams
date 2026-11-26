//! Synthetic native workflow interactions. No real identities, credentials, external connections or tenant writes.
#![cfg(test)]
#[expect(dead_code, reason = "shared fake engine helpers used by other view tests")]
mod common;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use teams_proto::WorkflowRequest;
use teams_store::{Team, TeamChannel};
use teams_ui::team_dialogs::TeamDialogEvent;
use teams_ui::workflow_dialog::WorkflowDialog;

#[path = "support/workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;
use fixtures::{answer, webhook_answer};
fn window() -> (common::Rig, Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    window_with(None)
}

/// The dialog on a core whose cloud serves `capabilities` (all when `None`).
fn window_with(
    capabilities: Option<teams_core::Capabilities>,
) -> (common::Rig, Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    let rig = common::Rig::start();
    *rig.core.capabilities.lock().unwrap() = capabilities;
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let team = Team {
        id: "synthetic-team".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        ..Team::default()
    };
    let channel = TeamChannel {
        id: "synthetic-channel".into(),
        name: "General".into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    let (view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new(core, team, channel, window, cx));
    cx.run_until_parked();
    (rig, view, cx)
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

#[test]
fn templates_are_real_cards_and_configuration_is_native() {
    let (_rig, _view, cx) = window();
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    click(cx, "workflow-template-synthetic-template");
    assert!(bounds(cx, "workflow-name").is_some());
    assert!(bounds(cx, "workflow-save").is_some());
    assert!(
        bounds(cx, "workflow-definition").is_none(),
        "definition is an optional advanced view"
    );
}
#[test]
fn lifecycle_actions_keep_the_opened_account_environment_and_workflow() {
    let (rig, _view, cx) = window();
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-toggle");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request,WorkflowRequest::SetEnabled { environment,flow,enabled:false } if environment=="Default-synthetic" && flow=="synthetic-flow")));
}
#[test]
fn delete_requires_its_own_confirmation_and_history_is_a_read() {
    let (rig, _view, cx) = window();
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-more");
    click(cx, "workflow-delete");
    assert!(bounds(cx, "workflow-delete-dialog").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    click(cx, "workflow-delete-confirm");
    assert!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request,WorkflowRequest::Delete { flow,.. } if flow=="synthetic-flow"))
    );
}

#[test]
fn an_old_catalog_reply_cannot_replace_a_new_refresh() {
    let (rig, view, cx) = window();
    *rig.core.hold_workflow_catalog.lock().unwrap() = true;
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    let old = rig.core.held_workflow_catalog.lock().unwrap().pop().unwrap();
    *rig.core.hold_workflow_catalog.lock().unwrap() = false;
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    old.send(Ok(Ok(
        serde_json::json!({"value":[{"id":"stale","name":"Stale","description":"Synthetic"}]}),
    )));
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    assert!(bounds(cx, "workflow-template-stale").is_none());
}

#[test]
fn unknown_write_outcomes_are_checked_without_automatic_replay() {
    fn uncertain(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if request.writes() {
            Err(teams_proto::WorkflowFailure::Connection)
        } else {
            answer(request)
        }
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(uncertain);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-toggle");
    click(cx, "workflow-toggle");
    assert!(bounds(cx, "workflow-reconcile").is_some());
    click(cx, "workflow-reconcile");
    assert_eq!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.writes())
            .count(),
        1
    );
    assert!(bounds(cx, "workflow-reconcile").is_none());
}

#[test]
fn read_only_managers_offer_history_without_mutating_actions() {
    let (rig, view, cx) = window();
    *rig.core.read_only.lock().unwrap() = true;
    view.update(cx, |_, cx| cx.notify());
    assert!(bounds(cx, "workflow-build").is_none());
    click(cx, "workflow-open-synthetic-flow");
    assert!(bounds(cx, "workflow-toggle").is_none());
    assert!(bounds(cx, "workflow-more").is_none());
    assert!(
        bounds(cx, "workflow-run-synthetic-run").is_some(),
        "the Overview shows the run history"
    );
    click(cx, "workflow-history");
    click(cx, "workflow-run-synthetic-run");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request, WorkflowRequest::RunActions{flow,run,..} if flow=="synthetic-flow" && run=="synthetic-run")));
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

#[test]
fn existing_edits_preserve_original_destinations_and_unknown_definition_nodes() {
    fn original_destination(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        let mut result = answer(request)?;
        if matches!(request, WorkflowRequest::Get { .. }) {
            result["properties"]["definition"]["parameters"]["teams.channel"]["defaultValue"] =
                serde_json::json!("another-synthetic-channel");
            result["properties"]["definition"]["metadata"] =
                serde_json::json!({"synthetic-unknown-node":{"preserved":true}});
        }
        Ok(result)
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(original_destination);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-edit");
    click(cx, "workflow-save");
    let requests = rig.core.workflow_requests.lock().unwrap();
    let properties = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Update { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("the edit uses the native update operation");
    assert_eq!(
        properties
            .pointer("/definition/parameters/teams.channel/defaultValue")
            .unwrap(),
        "another-synthetic-channel"
    );
    assert_eq!(
        properties
            .pointer("/definition/metadata/synthetic-unknown-node/preserved")
            .unwrap(),
        true
    );
}

#[test]
fn required_trigger_inputs_are_native_and_missing_inputs_never_start_a_run() {
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::input_answer);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-run");
    assert!(bounds(cx, "workflow-run-input-text").is_some());
    click(cx, "workflow-run-submit");
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    click(cx, "workflow-run-input-text");
    cx.simulate_input("Synthetic reminder");
    click(cx, "workflow-run-submit");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request|matches!(request,WorkflowRequest::RunWithInputs {environment,flow,trigger,inputs} if environment=="Default-synthetic" && flow=="synthetic-flow" && trigger=="manual" && inputs["text"]=="Synthetic reminder")));
}

fn managed_window() -> (common::Rig, Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    let (rig, view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    (rig, view, cx)
}
fn choose_connection(cx: &mut VisualTestContext, index: usize) {
    click(cx, &format!("workflow-connection-{index}"));
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
}

#[test]
fn connector_accounts_must_be_chosen_and_are_bound_to_the_actual_saved_definition() {
    let (rig, _view, cx) = managed_window();
    click(cx, "workflow-template-synthetic-template");
    click(cx, "workflow-save");
    assert!(bounds(cx, "workflow-error").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-connections-refresh");
    click(cx, "workflow-save");
    let requests = rig.core.workflow_requests.lock().unwrap();
    let properties = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties),
            _ => None,
        })
        .unwrap();
    for key in ["legacy", "teams"] {
        assert_eq!(
            properties["connectionReferences"][key]["connectionName"],
            "synthetic-connection"
        );
        assert_eq!(
            properties["connectionReferences"][key]["id"],
            "/providers/Microsoft.PowerApps/apis/shared_teams"
        );
    }
}

#[test]
fn connection_sign_in_accepts_only_the_service_issued_microsoft_consent_host() {
    fn unsafe_consent(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if matches!(request, WorkflowRequest::ConnectionConsent { .. }) {
            Ok(serde_json::json!({"consentLink":"https://outside.example.test/collect"}))
        } else {
            managed_answer(request)
        }
    }
    let (rig, _view, cx) = managed_window();
    click(cx, "workflow-template-synthetic-template");
    *rig.core.workflow_handler.lock().unwrap() = Some(unsafe_consent);
    click(cx, "workflow-connect-0");
    assert!(bounds(cx, "workflow-error").is_some());
    let before = rig.core.workflow_requests.lock().unwrap().len();
    cx.run_until_parked();
    assert_eq!(
        rig.core.workflow_requests.lock().unwrap().len(),
        before,
        "consent errors do not replay creation"
    );
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    click(cx, "workflow-connect-0");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request, WorkflowRequest::ConnectionConsent { environment, connector, .. } if environment == "Default-synthetic" && connector == "shared_teams")));
    assert_eq!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::CreateConnection { .. }))
            .count(),
        2
    );
}

#[test]
fn failed_connections_can_be_refreshed_and_a_disconnected_saved_reference_cannot_be_reused() {
    fn unavailable(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if matches!(request, WorkflowRequest::Connections { .. }) {
            return Err(teams_proto::WorkflowFailure::Forbidden);
        }
        let mut value = managed_answer(request)?;
        if matches!(request, WorkflowRequest::Template { .. }) {
            value["properties"]["connectionReferences"]["legacy"]["connectionName"] = serde_json::json!("disconnected");
        }
        Ok(value)
    }
    let (rig, view, cx) = managed_window();
    *rig.core.workflow_handler.lock().unwrap() = Some(unavailable);
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    click(cx, "workflow-template-synthetic-template");
    click(cx, "workflow-save");
    assert!(bounds(cx, "workflow-error").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    click(cx, "workflow-connections-refresh");
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-save");
    assert!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Create { .. }))
    );
}

fn catalog_page(cx: &mut VisualTestContext, next: bool) {
    // The kit icon Buttons have no selector wrapper; their centers follow the visible templates button.
    let heading = bounds(cx, "workflow-all-templates").unwrap();
    cx.simulate_click(
        gpui::point(
            heading.origin.x + heading.size.width + gpui::px(if next { 64.0 } else { 24.0 }),
            heading.center().y,
        ),
        Modifiers::none(),
    );
    cx.run_until_parked();
}

#[test]
fn discovery_paging_search_and_tabs_keep_the_selected_actor_scope() {
    let (rig, _view, cx) = managed_window();
    catalog_page(cx, true);
    assert!(bounds(cx, "workflow-template-synthetic-extra").is_some());
    catalog_page(cx, false);
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    click(cx, "workflow-all-templates");
    click(cx, "workflow-search");
    cx.simulate_input("Additional");
    assert!(bounds(cx, "workflow-template-synthetic-extra").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-template").is_none());
    click(cx, "workflow-tab-ai");
    assert!(
        bounds(cx, "workflow-open-synthetic-flow").is_none(),
        "no license is presented as no matching AI workflow"
    );
    click(cx, "workflow-tab-all");
    assert!(bounds(cx, "workflow-open-synthetic-flow").is_some());
    let requests = rig.core.workflow_requests.lock().unwrap();
    assert!(requests.iter().all(|request| match request {
        WorkflowRequest::List { environment } | WorkflowRequest::Connections { environment } => {
            environment == "Default-synthetic"
        }
        _ => true,
    }));
}

#[test]
fn build_from_scratch_is_teams_builder_and_leads_to_power_automate_without_writing() {
    let (rig, _view, cx) = window();
    click(cx, "workflow-build");
    assert!(bounds(cx, "workflow-builder").is_some());
    assert!(bounds(cx, "workflow-build-trigger").is_some());
    assert!(bounds(cx, "workflow-build-action").is_some());
    click(cx, "workflow-build-trigger");
    click(cx, "workflow-build-schedule");
    assert!(
        bounds(cx, "workflow-build-ask").is_some(),
        "a choice asks to build in Power Automate"
    );
    click(cx, "workflow-build-cancel");
    assert!(bounds(cx, "workflow-build-ask").is_none());
    click(cx, "workflow-build-action");
    click(cx, "workflow-build-power-automate");
    click(cx, "workflow-build-go");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://make.powerautomate.com/environments/Default-synthetic/flows/new")
    );
    click(cx, "workflow-build-back");
    assert!(bounds(cx, "workflow-build").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

#[test]
fn copying_a_webhook_reads_only_its_address_and_never_invokes_or_runs_it() {
    fn webhook(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        let mut value = answer(request)?;
        match request {
            WorkflowRequest::Get { .. } => {
                value["properties"]["definition"]["triggers"] =
                    serde_json::json!({"manual":{"type":"Request","kind":"TeamsWebhookV2"}});
            }
            WorkflowRequest::WebhookAddress { .. } => {
                value =
                    serde_json::json!({"response":{"value":"https://synthetic.example.test/service-issued-webhook"}});
            }
            _ => {}
        }
        Ok(value)
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(webhook);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-copy-webhook");
    assert_eq!(
        rig.cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text())),
        Some("https://synthetic.example.test/service-issued-webhook".into())
    );
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

/// Discovery and connector answers used to exercise native account binding and environment navigation.
fn managed_answer(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
    let mut value = answer(request)?;
    match request {
        WorkflowRequest::Catalog => {
            value["value"].as_array_mut().expect("the synthetic catalog is an array").push(serde_json::json!({"id":"synthetic-extra","name":"Additional synthetic template","description":"A seventh native template"}));
        }
        WorkflowRequest::Environments => {
            value = serde_json::json!({"value":[
                {"name":"Default-synthetic","properties":{"displayName":"Synthetic environment"}},
                {"name":"Other-synthetic","properties":{"displayName":"Other synthetic environment"}}
            ]});
        }
        WorkflowRequest::Connections { .. } => {
            value = serde_json::json!({"value":[
                {"name":"synthetic-connection","properties":{"displayName":"Synthetic connected account","apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}},
                {"name":"disconnected","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Disconnected"}]}},
                {"properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}},
                {"name":"unrelated","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_other","statuses":[{"status":"Connected"}]}}
            ]});
        }
        WorkflowRequest::Template { .. } | WorkflowRequest::Get { .. } => {
            value["properties"]["connectionReferences"] = serde_json::json!({
                "legacy":{"id":"/providers/Microsoft.PowerApps/apis/shared_teams"},
                "teams":{"api":{"name":"shared_teams","properties":{"displayName":"Microsoft Teams"}}}
            });
            value["properties"]["lastModifiedTime"] = serde_json::json!("synthetic-version");
            if let WorkflowRequest::Get { flow, .. } = request {
                value["name"] = serde_json::json!(flow);
            }
        }
        WorkflowRequest::CreateConnection { .. } => value = serde_json::json!({}),
        WorkflowRequest::ConnectionConsent { .. } => {
            value = serde_json::json!({"consentLink":"https://global.consent.azure-apim.net/redirect/teams?state=synthetic"});
        }
        _ => {}
    }
    Ok(value)
}

#[test]
fn advanced_definition_saves_its_current_connection_references_without_losing_unknown_fields() {
    let (rig, _view, cx) = managed_window();
    cx.simulate_resize(gpui::size(gpui::px(1280.0), gpui::px(1600.0)));
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-edit");
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-definition-toggle");
    let mut properties = managed_answer(&WorkflowRequest::Get {
        environment: "Default-synthetic".into(),
        flow: "synthetic-flow".into(),
    })
    .unwrap()["properties"]
        .clone();
    properties["connectionReferences"] = serde_json::json!({
        "added":{"id":"/providers/Microsoft.PowerApps/apis/shared_teams", "connectionName":"synthetic-connection", "unknown":{"preserve":true}}
    });
    properties["definition"]["actions"]["new_action"] =
        serde_json::json!({"type":"OpenApiConnection", "inputs":{"host":{"connectionName":"added"}}});
    click(cx, "workflow-definition");
    cx.update(|window, cx| window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx));
    cx.simulate_input(&properties.to_string());
    cx.update(|window, cx| {
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        window.dispatch_action(Box::new(gpui_component::input::Copy), cx);
    });
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
        Some(properties.to_string())
    );
    let body = bounds(cx, "workflow-scroll").unwrap();
    cx.simulate_event(gpui::ScrollEvent {
        position: gpui::point(body.left() + gpui::px(8.0), body.center().y),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-1200.0))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollEvent::default()
    });
    click(cx, "workflow-save");
    assert!(
        bounds(cx, "workflow-error").is_none(),
        "the edited document must validate"
    );
    let requests = rig.core.workflow_requests.lock().unwrap();
    let saved = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Update { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("the advanced definition is saved");
    assert_eq!(
        saved["connectionReferences"]["added"]["connectionName"],
        "synthetic-connection"
    );
    assert_eq!(saved["connectionReferences"]["added"]["unknown"]["preserve"], true);
    assert!(saved["connectionReferences"].get("legacy").is_none());
    assert!(saved["definition"]["actions"].get("new_action").is_some());
}

#[test]
fn connection_choices_survive_switching_to_advanced_definition_and_back() {
    let (rig, _view, cx) = managed_window();
    click(cx, "workflow-template-synthetic-template");
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-definition-toggle");
    assert!(bounds(cx, "workflow-connection-0").is_none());
    assert!(bounds(cx, "workflow-config-0").is_none());
    click(cx, "workflow-save");
    let first = rig
        .core
        .workflow_requests
        .lock()
        .unwrap()
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties.clone()),
            _ => None,
        })
        .expect("selected form connections survive advanced-mode save");
    for key in ["legacy", "teams"] {
        assert_eq!(
            first["connectionReferences"][key]["connectionName"],
            "synthetic-connection"
        );
    }
    *rig.core.workflow_handler.lock().unwrap() = Some(existing_account_answer);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-edit");
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-definition-toggle");
    click(cx, "workflow-definition-toggle");
    assert!(bounds(cx, "workflow-connection-0").is_some());
    click(cx, "workflow-save");
    let requests = rig.core.workflow_requests.lock().unwrap();
    let updated = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Update { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("an existing workflow's changed account is saved");
    for key in ["legacy", "teams"] {
        assert_eq!(
            updated["connectionReferences"][key]["connectionName"],
            "synthetic-connection"
        );
    }
}

fn existing_account_answer(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
    let mut value = managed_answer(request)?;
    if matches!(request, WorkflowRequest::Get { .. }) {
        for key in ["legacy", "teams"] {
            value["properties"]["connectionReferences"][key]["connectionName"] =
                serde_json::json!("previous-connection");
        }
    }
    Ok(value)
}

fn webhook_window(rig: &common::Rig) -> &'static mut VisualTestContext {
    let channel = |id: &str, name: &str| TeamChannel {
        id: id.into(),
        name: name.into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    let team = Team {
        id: "synthetic-team".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        channels: vec![
            channel("synthetic-channel", "General"),
            channel("synthetic-alerts", "Alerts"),
        ],
        ..Team::default()
    };
    *rig.core.teams.lock().unwrap() = Some(vec![team.clone()]);
    *rig.core.workflow_handler.lock().unwrap() = Some(webhook_answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let channel = team.channels[0].clone();
    let (_view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new(core, team, channel, window, cx));
    cx.run_until_parked();
    cx
}
fn authorized(rig: &common::Rig) -> Option<String> {
    rig.core
        .workflow_requests
        .lock()
        .unwrap()
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::AuthorizeConnection {
                connector, connection, ..
            } if connector == "shared_teams" => Some(connection.clone()),
            _ => None,
        })
}

#[test]
fn the_webhook_template_is_teams_card_with_a_team_and_a_channel_and_connects_by_itself() {
    let rig = common::Rig::start();
    let cx = webhook_window(&rig);
    click(cx, "workflow-template-synthetic-webhook");
    for shown in [
        "workflow-simple-card",
        "workflow-simple-team",
        "workflow-simple-channel",
        "workflow-save",
    ] {
        assert!(bounds(cx, shown).is_some(), "{shown} is drawn");
    }
    for hidden in [
        "workflow-name",
        "workflow-definition-toggle",
        "workflow-connection-0",
        "workflow-connect-0",
        "workflow-config-0",
    ] {
        assert!(bounds(cx, hidden).is_none(), "Teams shows no {hidden}");
    }
    let connection = authorized(&rig).expect("the Teams connection is made behind the scenes");
    click(cx, "workflow-collapse-all");
    assert!(
        bounds(cx, "workflow-simple-team").is_none(),
        "Collapse all folds the card"
    );
    click(cx, "workflow-card-toggle");
    click(cx, "workflow-simple-channel");
    click(cx, "workflow-simple-channel-synthetic-alerts");
    click(cx, "workflow-save");
    let created = rig
        .core
        .workflow_requests
        .lock()
        .unwrap()
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties.clone()),
            _ => None,
        })
        .expect("Save creates the workflow");
    assert_saved_as_teams_does(&created, &connection);
    assert!(
        bounds(cx, "workflow-copy-webhook").is_some(),
        "Save leads to the Overview"
    );
    assert!(bounds(cx, "workflow-detail-back").is_some());
}

/// What Teams' Save sends for the webhook template, with Alerts chosen.
fn assert_saved_as_teams_does(created: &serde_json::Value, connection: &str) {
    assert_eq!(created["displayName"], "Send webhook alerts to Alerts");
    assert_eq!(created["templateName"], "synthetic-webhook");
    assert_eq!(
        created["connectionReferences"]["shared_teams"]["connectionName"],
        connection
    );
    let post = &created["definition"]["actions"]["Post"];
    assert_eq!(
        post["inputs"]["parameters"]["body/recipient/groupId"],
        "synthetic-group"
    );
    assert_eq!(
        post["inputs"]["parameters"]["body/recipient/channelId"],
        "synthetic-alerts"
    );
    assert_eq!(
        post["runAfter"],
        serde_json::json!({"Do_Not_Remove_FlowIL": ["Succeeded"]})
    );
    let kept: serde_json::Value = serde_json::from_str(
        created["definition"]["actions"]["Do_Not_Remove_FlowIL"]["metadata"]["userIntentMarkup"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        kept["automation"]["template"]["parameters"][1]["valueDisplayName"],
        "Alerts"
    );
    assert!(
        created.get("state").is_none(),
        "Teams leaves a saved webhook workflow on"
    );
    assert_eq!(
        created["definition"]["description"],
        fixtures::webhook_template()["properties"]["description"],
        "the Overview's description is the one the saved definition keeps"
    );
}

#[test]
fn an_existing_teams_connection_is_reused_and_back_leaves_without_writing() {
    fn connected(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if matches!(request, WorkflowRequest::Connections { .. }) {
            return Ok(serde_json::json!({"value":[{"name":"synthetic-teams","properties":{
                "apiId":"/providers/Microsoft.PowerApps/apis/shared_teams",
                "statuses":[{"status":"Connected"}]
            }}]}));
        }
        webhook_answer(request)
    }
    let rig = common::Rig::start();
    let cx = webhook_window(&rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(connected);
    click(cx, "workflow-template-synthetic-webhook");
    assert!(bounds(cx, "workflow-simple-card").is_some());
    assert_eq!(authorized(&rig), None);
    click(cx, "workflow-edit-cancel");
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

fn requests(rig: &common::Rig) -> Vec<WorkflowRequest> {
    rig.core.workflow_requests.lock().unwrap().clone()
}

/// The webhook window over the synthetic team and a second team with its own channel.
fn two_teams_window(rig: &common::Rig) -> &'static mut VisualTestContext {
    let _ = webhook_window(rig);
    let channel = TeamChannel {
        id: "synthetic-other-channel".into(),
        name: "Planning".into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    let mut teams = rig.core.teams.lock().unwrap().clone().unwrap();
    teams.push(Team {
        id: "synthetic-other-team".into(),
        group_id: Some("synthetic-other-group".into()),
        name: "Other team".into(),
        channels: vec![channel],
        ..Team::default()
    });
    *rig.core.teams.lock().unwrap() = Some(teams.clone());
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let team = teams[0].clone();
    let channel = team.channels[0].clone();
    let (_view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new(core, team, channel, window, cx));
    cx.run_until_parked();
    cx
}

#[test]
fn the_team_and_channel_fields_filter_as_typed_and_a_new_team_clears_the_channel() {
    let rig = common::Rig::start();
    let cx = two_teams_window(&rig);
    click(cx, "workflow-template-synthetic-webhook");
    click(cx, "workflow-simple-team");
    for team in ["synthetic-group", "synthetic-other-group"] {
        assert!(
            bounds(cx, &format!("workflow-simple-team-{team}")).is_some(),
            "opening the field lists every team"
        );
    }
    cx.simulate_keystrokes("ctrl-a backspace");
    cx.simulate_input("oth");
    assert!(bounds(cx, "workflow-simple-team-synthetic-other-group").is_some());
    assert!(
        bounds(cx, "workflow-simple-team-synthetic-group").is_none(),
        "typing filters the teams to the names holding the text"
    );
    click(cx, "workflow-simple-team-synthetic-other-group");
    click(cx, "workflow-save");
    assert!(
        !requests(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Create { .. })),
        "a new team clears the channel, and Save waits for one"
    );
    click(cx, "workflow-simple-channel");
    assert!(
        bounds(cx, "workflow-simple-channel-synthetic-alerts").is_none(),
        "the channels are the new team's"
    );
    click(cx, "workflow-simple-channel-synthetic-other-channel");
    click(cx, "workflow-save");
    let created = requests(&rig)
        .into_iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("Save creates the workflow once both are chosen");
    assert_eq!(created["displayName"], "Send webhook alerts to Planning");
    assert_eq!(
        created["definition"]["actions"]["Post"]["inputs"]["parameters"]["body/recipient/groupId"],
        "synthetic-other-group"
    );
}

fn overview_window(rig: &common::Rig) -> &'static mut VisualTestContext {
    let cx = webhook_window(rig);
    for (id, name) in [
        (fixtures::OWNER, "Synthetic Maker"),
        (fixtures::CO_OWNER, "Synthetic Helper"),
    ] {
        rig.core
            .names
            .lock()
            .unwrap()
            .insert(format!("8:orgid:{id}"), name.to_owned());
    }
    // The synthetic webhook names no channel, so it is listed under Workflows, not this channel.
    click(cx, "workflow-tab-all");
    click(cx, "workflow-open-synthetic-flow");
    cx
}

#[test]
fn rename_and_turn_off_stay_on_the_overview_and_send_what_teams_sends() {
    let rig = common::Rig::start();
    let cx = overview_window(&rig);
    click(cx, "workflow-rename");
    assert!(bounds(cx, "workflow-rename-field").is_some());
    cx.simulate_keystrokes("ctrl-a backspace");
    click(cx, "workflow-rename-save");
    assert!(
        !requests(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Rename { .. })),
        "an empty name is never saved"
    );
    click(cx, "workflow-rename-field");
    cx.simulate_input("Synthetic alerts");
    click(cx, "workflow-rename-save");
    assert!(requests(&rig).iter().any(|request| matches!(request,
        WorkflowRequest::Rename { flow, name, .. } if flow == "synthetic-flow" && name == "Synthetic alerts")));
    assert!(
        bounds(cx, "workflow-rename").is_some(),
        "the Overview stays, its name shown again"
    );
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::stopped_webhook_answer);
    click(cx, "workflow-toggle");
    assert!(requests(&rig).iter().any(|request| matches!(request,
        WorkflowRequest::SetEnabled { flow, enabled: false, .. } if flow == "synthetic-flow")));
    assert!(
        bounds(cx, "workflow-copy-webhook").is_some() && bounds(cx, "workflow-template-synthetic-webhook").is_none(),
        "Turn off stays on the Overview"
    );
    assert!(bounds(cx, "workflow-state").is_some());
    assert!(
        requests(&rig)
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::Get { .. }))
            .count()
            >= 2,
        "Turn off reads the workflow again, as Teams does"
    );
}

#[test]
fn owners_are_read_and_a_co_owner_is_added_and_removed_as_teams_does() {
    let rig = common::Rig::start();
    *rig.core.addable.lock().unwrap() = vec![teams_proto::TeamPerson {
        mri: "8:orgid:00000000-0000-4000-8000-00000000000c".into(),
        name: "Synthetic Newcomer".into(),
        email: Some("newcomer@example.test".into()),
        job_title: None,
    }];
    let cx = overview_window(&rig);
    assert!(bounds(cx, &format!("workflow-owner-{}", fixtures::OWNER)).is_some());
    assert!(
        bounds(cx, &format!("workflow-owner-remove-{}", fixtures::OWNER)).is_none(),
        "the maker is the Owner, not removable"
    );
    to_the_end(cx);
    click(cx, "workflow-owner-search");
    cx.simulate_input("new");
    click(cx, "workflow-owner-pick-8:orgid:00000000-0000-4000-8000-00000000000c");
    assert!(requests(&rig).iter().any(|request| matches!(request,
        WorkflowRequest::AddOwner { flow, owner, .. }
            if flow == "synthetic-flow" && owner.id == "00000000-0000-4000-8000-00000000000c"
                && owner.email.as_deref() == Some("newcomer@example.test"))));
    click(cx, &format!("workflow-owner-remove-{}", fixtures::CO_OWNER));
    assert!(requests(&rig).iter().any(|request| matches!(request,
        WorkflowRequest::RemoveOwner { owner, .. } if owner == fixtures::CO_OWNER)));
    assert!(
        requests(&rig)
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::Owners { .. }))
            .count()
            >= 3,
        "each change reads the owners again"
    );
}

#[test]
fn a_co_owner_search_finding_nobody_says_so_and_adds_nobody() {
    let rig = common::Rig::start();
    let cx = overview_window(&rig);
    to_the_end(cx);
    click(cx, "workflow-owner-search");
    cx.simulate_input("zzq");
    assert!(bounds(cx, "workflow-owner-found").is_some());
    assert!(
        !requests(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::AddOwner { .. }))
    );
}

#[test]
fn workflows_home_and_see_all_leave_the_dialog_for_the_workflows_app() {
    let rig = common::Rig::start();
    let cx = webhook_window(&rig);
    let view = cx.update(|window, _| {
        window
            .root::<WorkflowDialog>()
            .flatten()
            .expect("the dialog is the window's root")
            .clone()
    });
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let kept = std::rc::Rc::clone(&asked);
    let _watch = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TeamDialogEvent, _| {
            if let TeamDialogEvent::Workflows { see_all } = event {
                kept.borrow_mut().push(*see_all);
            }
        })
    });
    click(cx, "workflow-home");
    assert_eq!(*asked.borrow(), [false], "Workflows Home opens the app's Home");
    assert!(bounds(cx, "workflow-environment").is_none());
}

/// Scrolls the dialog's page to its end, where the Owners are.
fn to_the_end(cx: &mut VisualTestContext) {
    let body = bounds(cx, "workflow-scroll").unwrap();
    cx.simulate_event(gpui::ScrollEvent {
        position: gpui::point(body.left() + gpui::px(8.0), body.center().y),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-3000.0))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollEvent::default()
    });
}

#[test]
fn a_cloud_without_copilot_offers_no_ai_workflows_and_asks_no_eligibility() {
    let (rig, _dialog, cx) = window_with(Some(teams_core::Capabilities {
        copilot: false,
        ..teams_core::Capabilities::ALL
    }));
    assert!(bounds(cx, "workflow-tab-all").is_some());
    assert!(bounds(cx, "workflow-tab-ai").is_none());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, WorkflowRequest::CopilotEligibility)),
        "no Copilot ask to a cloud without it"
    );
}
