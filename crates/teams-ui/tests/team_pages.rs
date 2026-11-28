//! The team pages on GPUI's test context, over a fake core: "See all your teams" and the page's rows and menu, Create team
//! and its Add members step, Delete team asking first, Hide all channels, and Manage team's settings writing the whole of
//! them.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::TeamWritten;
use teams_proto::{OwnTeamView, TeamWrite};
use teams_store::{ChannelKind, Team, TeamAbout, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::manage_team::ManageTab;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::team_app_dialogs::TeamPictureDialog;
use teams_ui::team_dialogs::{AddMembersDialog, ConfirmDialog, CreateTeamDialog};

const OWNED: &str = "19:team0001@thread.tacv2";
const MEMBER_OF: &str = "19:team0002@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

fn team(id: &str, name: &str, owner: bool) -> Team {
    Team {
        id: id.to_owned(),
        name: name.to_owned(),
        group_id: Some(GROUP.to_owned()),
        owner,
        can_add_channel: true,
        about: TeamAbout {
            description: "Made up".to_owned(),
            people: 3,
            ..TeamAbout::default()
        },
        channels: vec![TeamChannel {
            id: id.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
        ..Team::default()
    }
}

fn shell_window(rig: &Rig, teams: Vec<Team>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    *rig.core.teams.lock().unwrap() = Some(teams);
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

fn open_page(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    click(cx, "see-all-teams");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::TeamsPage);
}

fn open_row_menu(cx: &mut VisualTestContext, row: usize) {
    let found = bounds(cx, &format!("teams-row-{row}")).expect("the team's row");
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    click(cx, &format!("teams-row-more-{row}"));
}

#[test]
fn see_all_your_teams_lists_them_as_teams_does_and_a_row_opens_manage_team() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true), team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    assert!(bounds(cx, "teams-page-title").is_some());
    assert!(bounds(cx, "teams-row-0").is_some());
    assert!(bounds(cx, "teams-row-1").is_some());
    click(cx, "teams-filter-owned");
    assert!(bounds(cx, "teams-row-1").is_none(), "only the team the account owns");
    click(cx, "teams-filter-all");
    click(cx, "teams-row-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::ManageTeam(MEMBER_OF.to_owned())
    );
    assert!(bounds(cx, "manage-tab-settings").is_none(), "a member has no settings");
    assert!(bounds(cx, "manage-tab-members").is_some());
}

#[test]
fn a_member_isnt_offered_what_only_an_owner_can_do() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    assert!(bounds(cx, "team-manage-0").is_some());
    assert!(bounds(cx, "team-leave-0").is_some());
    assert!(bounds(cx, "team-delete-0").is_none());
    assert!(bounds(cx, "team-archive-0").is_none());
}

#[test]
fn delete_team_asks_first_and_goes_out_only_after_its_tick() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    click(cx, "team-delete-0");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ConfirmDialog>().ok())
        .expect("the dialog");
    assert!(!cx.update(|_, cx| dialog.read(cx).can_confirm()));
    cx.update(|_, cx| dialog.update(cx, teams_ui::team_dialogs::ConfirmDialog::confirm));
    cx.run_until_parked();
    assert!(rig.core.team_writes.lock().unwrap().is_empty(), "nothing goes unticked");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_ticked(true, cx)));
    click(cx, "team-confirm-ok");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(matches!(writes.as_slice(), [(TeamWrite::Delete { team }, _)] if team == OWNED));
    drop(writes);
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn hide_all_channels_is_the_accounts_own_view() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    click(cx, "team-hide-all-0");
    click(cx, "team-confirm-ok");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    assert_eq!(
        *rig.core.team_views.lock().unwrap(),
        [OwnTeamView::HideAll { team: OWNED.to_owned() }]
    );
}

#[test]
fn create_team_makes_a_private_team_by_default_and_goes_on_to_add_members() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten {
        created: Some((OWNED.to_owned(), Some(GROUP.to_owned()))),
        ..TeamWritten::default()
    });
    let (shell, cx) = shell_window(&rig, vec![team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    click(cx, "teams-create");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<CreateTeamDialog>().ok())
        .expect("the dialog");
    assert!(!cx.update(|_, cx| dialog.read(cx).can_create(cx)));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("New team", "About it", "Planning", window, cx);
        });
    });
    click(cx, "create-team-create");
    {
        let writes = rig.core.team_writes.lock().unwrap();
        let [(TeamWrite::Create(new), _)] = writes.as_slice() else {
            panic!("one new team");
        };
        assert_eq!(new.name, "New team");
        assert!(!new.public);
    }
    let next = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AddMembersDialog>().ok());
    assert!(next.is_some(), "its Add members step comes next");
    assert!(bounds(cx, "add-members-cancel").is_some());
}

#[test]
fn a_settings_box_writes_the_whole_of_the_teams_settings() {
    let rig = Rig::start();
    *rig.core.team_settings.lock().unwrap() =
        Some(r#"{"userSettings":{"createTopic":true,"updateTopic":true,"other":7}}"#.to_owned());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(OWNED.to_owned(), ManageTab::Settings, window, cx);
        });
    });
    click(cx, "manage-section-members");
    click(cx, "create-update-channel");
    let writes = rig.core.team_writes.lock().unwrap();
    let [(TeamWrite::Settings { team, settings }, _)] = writes.as_slice() else {
        panic!("one settings write: {writes:?}");
    };
    assert_eq!(team, OWNED);
    let written: serde_json::Value = serde_json::from_str(settings).expect("JSON");
    assert_eq!(written["userSettings"]["createTopic"], false);
    assert_eq!(written["userSettings"]["updateTopic"], false);
    assert_eq!(written["userSettings"]["other"], 7);
}

fn installed(id: &str, name: &str) -> teams_proto::InstalledApp {
    teams_proto::InstalledApp {
        id: id.to_owned(),
        name: name.to_owned(),
        developer: "Made-up Maker".to_owned(),
        description: String::new(),
        accent: Some(0x0062_64a7),
    }
}

fn manage(shell: &Entity<Shell>, tab: ManageTab, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(OWNED.to_owned(), tab, window, cx);
        });
    });
    cx.run_until_parked();
}

#[test]
fn the_apps_tab_lists_the_teams_apps_and_remove_asks_first() {
    let rig = Rig::start();
    *rig.core.team_apps.lock().unwrap() = vec![installed("a1", "Made-up Planner"), installed("a2", "Made-up Updates")];
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Apps, cx);
    assert!(bounds(cx, "manage-app-0").is_some());
    assert!(bounds(cx, "manage-app-1").is_some());
    click(cx, "manage-app-more-1");
    click(cx, "manage-app-remove");
    assert!(rig.core.team_writes.lock().unwrap().is_empty(), "it asks first");
    assert!(bounds(cx, "manage-app-remove-question").is_some());
    // what the tab reads again once the app is gone
    *rig.core.team_apps.lock().unwrap() = vec![installed("a1", "Made-up Planner")];
    click(cx, "manage-app-remove-confirm");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(writes.as_slice(), [(TeamWrite::RemoveApp { team, app }, None)] if team == OWNED && app == "a2"),
        "{writes:?}"
    );
    drop(writes);
    assert!(bounds(cx, "manage-app-1").is_none(), "gone from the list");
}

#[test]
fn get_more_apps_adds_one_to_the_team_with_its_definition() {
    let rig = Rig::start();
    let definition = serde_json::json!({"id": "a3", "name": "Made-up Pages", "developerName": "Made-up Maker"});
    *rig.core.store_apps.lock().unwrap() =
        teams_proto::AppDefinitions::read_answer(serde_json::json!([definition]).to_string().as_bytes());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Apps, cx);
    click(cx, "manage-get-apps");
    assert!(bounds(cx, "add-app-dialog").is_some());
    click(cx, "add-app-add-0");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(writes.as_slice(), [(TeamWrite::AddApp { team, app }, None)] if team == OWNED && app.id() == "a3"),
        "{writes:?}"
    );
}

#[test]
fn the_giphy_rating_writes_the_whole_of_the_settings() {
    let rig = Rig::start();
    *rig.core.team_settings.lock().unwrap() =
        Some(r#"{"userSettings":{"giphyEnabled":true,"giphyRating":1,"other":7}}"#.to_owned());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Settings, cx);
    click(cx, "manage-section-fun");
    click(cx, "giphy-rating");
    click(cx, "giphy-rating-2");
    let writes = rig.core.team_writes.lock().unwrap();
    let [(TeamWrite::Settings { settings, .. }, _)] = writes.as_slice() else {
        panic!("one settings write: {writes:?}");
    };
    let written: serde_json::Value = serde_json::from_str(settings).expect("JSON");
    assert_eq!(written["userSettings"]["giphyRating"], 2);
    assert_eq!(written["userSettings"]["other"], 7);
}

#[test]
fn analytics_reads_the_channel_picked() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Analytics, cx);
    click(cx, "manage-analytics-channel");
    click(cx, "manage-analytics-channel-1");
    assert_eq!(*rig.core.channel_analytics_asked.lock().unwrap(), [OWNED.to_owned()]);
}

#[test]
fn change_picture_opens_its_dialog_and_save_waits_for_a_picture() {
    let rig = Rig::start();
    *rig.core.team_settings.lock().unwrap() = Some("{}".to_owned());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Settings, cx);
    click(cx, "manage-change-picture");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<TeamPictureDialog>().ok())
        .expect("the dialog");
    click(cx, "team-picture-save");
    assert!(rig.core.team_writes.lock().unwrap().is_empty(), "nothing chosen yet");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.choose(b"GIF89a".to_vec(), cx)));
    assert!(bounds(cx, "team-picture-problem").is_some());
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.choose(b"\x89PNG\r\n\x1a\n".to_vec(), cx)));
    click(cx, "team-picture-save");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(writes.as_slice(), [(TeamWrite::SetPicture { team, group, picture }, None)]
            if team == OWNED && group == GROUP && picture.bytes().starts_with(b"\x89PNG")),
        "{writes:?}"
    );
}

#[test]
fn more_create_team_options_makes_a_team_from_a_template_or_another_team() {
    let rig = Rig::start();
    let card = teams_proto::TeamTemplates::read_answer(
        br#"{"value":[{"id":"made.up.template","name":"Manage a Made-up Project","shortDescription":"s","categories":["General"]}]}"#,
    );
    let details = teams_proto::TeamTemplateRead::read_answer(
        br#"{"value":{"teamDefinition":{"channels":[{"id":"General","displayName":"General"}],"apps":[{"id":"x"}]}}}"#,
    );
    *rig.core.templates.lock().unwrap() = (card, details);
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-create");
    click(cx, "create-team-more");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::CreateTeamMore);
    click(cx, "create-more-template-0");
    click(cx, "create-more-use-template");
    let page = cx
        .update(|_, cx| shell.read(cx).create_team_more().cloned())
        .expect("the page");
    cx.update(|window, cx| page.update(cx, |page, cx| page.fill("From a template", "", window, cx)));
    click(cx, "create-more-create");
    {
        let writes = rig.core.team_writes.lock().unwrap();
        let [(TeamWrite::FromTemplate(made), None)] = writes.as_slice() else {
            panic!("one team from a template: {writes:?}");
        };
        assert_eq!(made.template, "made.up.template");
        assert_eq!(made.channels, [("General".to_owned(), "General".to_owned())]);
    }
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::TeamsPage);
    click(cx, "teams-create");
    click(cx, "create-team-more");
    click(cx, "create-more-another-team");
    click(cx, "create-more-team-0");
    click(cx, "create-more-copy-people");
    click(cx, "create-more-create");
    let writes = rig.core.team_writes.lock().unwrap();
    let [_, (TeamWrite::Copy(copy), None)] = writes.as_slice() else {
        panic!("a copy: {writes:?}");
    };
    assert_eq!(copy.group, GROUP);
    assert_eq!(copy.name, "Alpha [copy]");
    assert!(copy.roster);
}

fn person(mri: &str, name: &str, owner: bool, guest: bool) -> teams_core::TeamMember {
    teams_core::TeamMember {
        mri: mri.to_owned(),
        name: name.to_owned(),
        email: Some(format!("{name}@example.invalid")),
        title: Some("Planning".to_owned()),
        location: Some("Remote".to_owned()),
        owner,
        guest,
        moderator: false,
    }
}

#[test]
fn edit_details_validates_the_form_and_keeps_changes_after_a_failed_save() {
    use teams_ui::team_dialogs::EditTeamDialog;
    let rig = Rig::start();
    *rig.core.team_settings.lock().unwrap() = Some("{}".to_owned());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Settings, cx);
    click(cx, "manage-edit-details");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .unwrap()
        .downcast::<EditTeamDialog>()
        .unwrap();
    assert!(!cx.update(|_, cx| dialog.read(cx).can_save(cx)));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("https://example.invalid", "www.example.invalid", window, cx);
        });
    });
    assert!(bounds(cx, "edit-team-name-problem").is_some());
    assert!(bounds(cx, "edit-team-description-problem").is_some());
    cx.update(|_, cx| dialog.update(cx, EditTeamDialog::save));
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("  Planning  ", "  Updated description  ", window, cx);
        });
    });
    click(cx, "edit-team-privacy");
    click(cx, "edit-team-public");
    click(cx, "edit-team-privacy");
    click(cx, "edit-team-private");
    click(cx, "edit-team-privacy");
    click(cx, "edit-team-public");
    click(cx, "edit-team-done");
    assert!(bounds(cx, "edit-team-failed").is_some());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    click(cx, "edit-team-done");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), 2, "the failed form can be retried");
    for (write, change) in writes.iter() {
        assert!(
            matches!(write, TeamWrite::Edit { team, name: Some(name), description, public: Some(true) }
            if team == OWNED && name == "Planning" && description == "Updated description")
        );
        assert!(
            matches!(change, Some(teams_core::TeamChange::Edited { name: Some(name), public: Some(true), .. })
            if name == "Planning")
        );
    }
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn new_tags_require_a_short_name_and_members_and_can_retry_without_losing_them() {
    use teams_ui::team_dialogs::TagDialog;
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![person("8:orgid:ada", "Ada", false, false)];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Tags, cx);
    assert!(bounds(cx, "manage-tags-empty").is_some());
    click(cx, "manage-create-tag");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .unwrap()
        .downcast::<TagDialog>()
        .unwrap();
    assert!(!cx.update(|_, cx| dialog.read(cx).can_save(cx)));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("Planning", "  Project planners  ", window, cx);
        });
    });
    assert!(
        !cx.update(|_, cx| dialog.read(cx).can_save(cx)),
        "a new tag needs members"
    );
    click(cx, "tag-member-box");
    cx.simulate_input("ad");
    click(cx, "tag-found-0");
    assert!(bounds(cx, "tag-picked-0").is_some());
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.pick("8:orgid:ada".to_owned(), "Ada".to_owned(), window, cx);
            dialog.fill(&"x".repeat(41), "Project planners", window, cx);
        });
    });
    assert!(bounds(cx, "tag-picked-1").is_none(), "a member is picked once");
    assert!(!cx.update(|_, cx| dialog.read(cx).can_save(cx)));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("  Planning  ", "  Project planners  ", window, cx);
        });
    });
    click(cx, "tag-save");
    assert!(bounds(cx, "tag-failed").is_some());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    click(cx, "tag-save");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    for (write, _) in writes.iter() {
        let TeamWrite::CreateTag {
            team,
            name,
            description,
            members,
        } = write
        else {
            panic!("create tag")
        };
        assert_eq!(
            (team.as_str(), name.as_str(), description.as_str()),
            (OWNED, "Planning", "Project planners")
        );
        assert_eq!(members, &[teams_proto::Mri::new("8:orgid:ada")]);
    }
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn manage_members_changes_roles_and_preserves_guest_permissions() {
    use teams_proto::MemberUpdate;
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    *rig.core.team_roster.lock().unwrap() = vec![
        person("8:orgid:owner", "Owner", true, false),
        person("8:orgid:ada", "Ada", false, false),
        person("8:orgid:guest", "Guest", false, true),
    ];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Members, cx);
    assert!(bounds(cx, "manage-member-8:orgid:owner").is_some());
    assert!(bounds(cx, "manage-member-8:orgid:guest").is_some());
    assert!(bounds(cx, "manage-role-8:orgid:guest").is_none());
    click(cx, "manage-owners");
    assert!(bounds(cx, "manage-member-8:orgid:owner").is_none());
    click(cx, "manage-owners");
    click(cx, "manage-others");
    assert!(bounds(cx, "manage-member-8:orgid:ada").is_none());
    click(cx, "manage-others");
    click(cx, "manage-role-8:orgid:ada");
    click(cx, "manage-role-owner");
    click(cx, "manage-role-8:orgid:owner");
    click(cx, "manage-role-member");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::UpdateMember { team, group, member, update: MemberUpdate::Promoted }
        if team == OWNED && group == GROUP && member == &teams_proto::Mri::new("8:orgid:ada"))
    );
    assert!(
        matches!(&writes[1].0, TeamWrite::UpdateMember { member, update: MemberUpdate::Demoted, .. }
        if member == &teams_proto::Mri::new("8:orgid:owner"))
    );
}

#[test]
fn pending_requests_accept_and_deny_the_person_chosen() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    *rig.core.team_requests.lock().unwrap() = vec![
        person("8:orgid:ada", "Ada", false, false),
        person("8:orgid:ben", "Ben", false, false),
    ];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::PendingRequests, cx);
    assert!(bounds(cx, "manage-request-1").is_some());
    rig.core.team_requests.lock().unwrap().remove(0);
    click(cx, "manage-request-accept-0");
    assert!(bounds(cx, "manage-request-1").is_none());
    rig.core.team_requests.lock().unwrap().clear();
    click(cx, "manage-request-deny-0");
    assert!(bounds(cx, "manage-request-0").is_none());
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::AnswerJoinRequest { group, member, accept: true }
        if group == GROUP && member == &teams_proto::Mri::new("8:orgid:ada"))
    );
    assert!(
        matches!(&writes[1].0, TeamWrite::AnswerJoinRequest { member, accept: false, .. }
        if member == &teams_proto::Mri::new("8:orgid:ben"))
    );
}

#[test]
fn team_codes_can_be_copied_reset_removed_and_generated() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    *rig.core.team_settings.lock().unwrap() = Some("{}".to_owned());
    *rig.core.team_code.lock().unwrap() = Some("madeup1".to_owned());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Settings, cx);
    click(cx, "manage-section-code");
    assert!(bounds(cx, "manage-team-code").is_some());
    click(cx, "manage-code-copy");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
        Some("madeup1".to_owned())
    );
    click(cx, "manage-code-reset");
    *rig.core.team_code.lock().unwrap() = None;
    click(cx, "manage-code-remove");
    assert!(bounds(cx, "manage-team-code").is_none());
    click(cx, "manage-code-generate");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(matches!(&writes[0].0, TeamWrite::ResetCode { team } if team == OWNED));
    assert!(matches!(&writes[1].0, TeamWrite::RemoveCode { team } if team == OWNED));
    assert!(matches!(&writes[2].0, TeamWrite::ResetCode { team } if team == OWNED));
}

#[test]
fn populated_analytics_show_each_period_and_the_selected_channel() {
    let rig = Rig::start();
    *rig.core.team_numbers.lock().unwrap() = Some(teams_proto::TeamAnalytics {
        team: OWNED.to_owned(),
        name: "Alpha".to_owned(),
        active_users: 3,
        active_users_by_day: vec![("2026-10-31".to_owned(), 0), ("2026-11-01".to_owned(), 3)],
        guests: 1,
        posts: 2,
        replies: 5,
        reactions: 7,
        mentions: 4,
        meetings: 1,
    });
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Analytics, cx);
    for period in ["last-thirty-days", "last-ninety-days", "last-seven-days"] {
        click(cx, "manage-period");
        click(cx, &format!("manage-period-{period}"));
        assert!(bounds(cx, "manage-analytics").is_some());
    }
    assert_eq!(
        *rig.core.team_analytics_asked.lock().unwrap(),
        [
            (GROUP.to_owned(), teams_proto::AnalyticsPeriod::LastSevenDays),
            (GROUP.to_owned(), teams_proto::AnalyticsPeriod::LastThirtyDays),
            (GROUP.to_owned(), teams_proto::AnalyticsPeriod::LastNinetyDays),
            (GROUP.to_owned(), teams_proto::AnalyticsPeriod::LastSevenDays),
        ],
        "period choices reload analytics with the selected span"
    );
    click(cx, "manage-analytics-channel");
    click(cx, "manage-analytics-channel-1");
    assert_eq!(*rig.core.channel_analytics_asked.lock().unwrap(), [OWNED.to_owned()]);
}

#[test]
fn create_failure_can_retry_with_the_first_channel_named_general() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-create");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .unwrap()
        .downcast::<CreateTeamDialog>()
        .unwrap();
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.fill("Planning", "Project", "Work", window, cx)));
    click(cx, "create-team-more-naming");
    click(cx, "create-team-general");
    click(cx, "create-team-create");
    assert!(bounds(cx, "create-team-failed").is_some());
    click(cx, "create-team-retry");
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten {
        created: Some(("19:new@thread.tacv2".to_owned(), Some(GROUP.to_owned()))),
        ..TeamWritten::default()
    });
    click(cx, "create-team-create");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    assert!(matches!(&writes[1].0, TeamWrite::Create(new) if new.name == "Planning" && new.first_channel == "General"));
    assert!(bounds(cx, "add-members-cancel").is_some());
}

#[test]
fn add_members_retries_only_people_the_service_did_not_add() {
    use teams_ui::team_menu::TeamChoice;
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten {
        not_added: vec!["8:orgid:ben".to_owned()],
        ..TeamWritten::default()
    });
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.team_chosen(team(OWNED, "Alpha", true), TeamChoice::AddMember, window, cx);
        });
    });
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .unwrap()
        .downcast::<AddMembersDialog>()
        .unwrap();
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            for (mri, name) in [("8:orgid:ada", "Ada"), ("8:orgid:ben", "Ben")] {
                dialog.pick(
                    teams_proto::TeamPerson {
                        mri: mri.to_owned(),
                        name: name.to_owned(),
                        email: Some(format!("{name}@example.invalid")),
                        job_title: None,
                    },
                    window,
                    cx,
                );
            }
        });
    });
    click(cx, "add-members-add");
    assert!(bounds(cx, "add-members-result").is_some());
    assert_eq!(cx.update(|_, cx| dialog.read(cx).picked().len()), 1);
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    click(cx, "add-members-retry");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(matches!(&writes[0].0, TeamWrite::AddMembers { members, .. } if members.len() == 2));
    assert!(matches!(&writes[1].0, TeamWrite::AddMembers { members, .. }
        if members.len() == 1 && members[0].0 == teams_proto::Mri::new("8:orgid:ben")));
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

/// Icon actions sit at the trailing edge of these rows; choose them through their rendered hit area.
fn click_row_end(cx: &mut VisualTestContext, selector: &str, inset: f32) {
    let row = bounds(cx, selector).expect("the action's row is drawn");
    let point = gpui::point(row.right() - gpui::px(inset), row.center().y);
    cx.simulate_mouse_move(point, None, Modifiers::none());
    cx.simulate_click(point, Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn existing_tags_keep_their_version_when_edited_and_can_be_deleted() {
    use teams_ui::team_dialogs::TagDialog;
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    rig.core.team_tags.lock().unwrap().tags = vec![teams_proto::TeamTag {
        id: "tag-1".to_owned(),
        name: "Planners".to_owned(),
        description: "Project".to_owned(),
        members: 2,
        version: 7,
    }];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Tags, cx);
    click_row_end(cx, "manage-tag-0", 48.0);
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .expect("edit opens a dialog")
        .downcast::<TagDialog>()
        .unwrap();
    assert!(
        bounds(cx, "tag-member-box").is_none(),
        "editing keeps existing membership"
    );
    assert!(cx.update(|_, cx| dialog.read(cx).can_save(cx)));
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.fill("Planning", " Updated ", window, cx)));
    click(cx, "tag-save");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    rig.core.team_tags.lock().unwrap().tags.clear();
    click_row_end(cx, "manage-tag-0", 12.0);
    assert!(bounds(cx, "manage-tags-empty").is_some());
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::EditTag { team, tag, version: 7, name, description }
        if team == OWNED && tag == "tag-1" && name == "Planning" && description == "Updated")
    );
    assert!(matches!(&writes[1].0, TeamWrite::DeleteTag { team, tag } if team == OWNED && tag == "tag-1"));
}

#[test]
fn removing_a_member_writes_the_chosen_identity_and_reports_a_refusal() {
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![person("8:orgid:ada", "Ada", false, false)];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Members, cx);
    click_row_end(cx, "manage-member-8:orgid:ada", 16.0);
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::UpdateMember { team, member, update: teams_proto::MemberUpdate::Removed, .. }
        if team == OWNED && member == &teams_proto::Mri::new("8:orgid:ada"))
    );
    drop(writes);
    assert!(
        bounds(cx, "manage-member-8:orgid:ada").is_some(),
        "a refusal reloads the unchanged roster"
    );
    assert!(bounds(cx, "manage-problem").is_some());
}

#[test]
fn archive_failure_retries_with_its_read_only_site_choice_and_leave_demotes_an_owner() {
    use teams_ui::team_menu::TeamChoice;
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.team_chosen(team(OWNED, "Alpha", true), TeamChoice::Archive, window, cx);
        });
    });
    click(cx, "team-confirm-tick");
    click(cx, "team-confirm-ok");
    assert!(bounds(cx, "team-confirm-failed").is_some());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    click(cx, "team-confirm-ok");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.team_chosen(team(OWNED, "Alpha", true), TeamChoice::Leave, window, cx);
        });
    });
    click(cx, "team-confirm-ok");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), 3);
    assert!(matches!(&writes[0].0, TeamWrite::Archive { group, site_read_only: true } if group == GROUP));
    assert!(matches!(
        &writes[1].0,
        TeamWrite::Archive {
            site_read_only: true,
            ..
        }
    ));
    assert!(matches!(
        &writes[2].0,
        TeamWrite::UpdateMember {
            update: teams_proto::MemberUpdate::DemotedAndLeft,
            ..
        }
    ));
}

#[test]
fn private_team_members_request_people_and_can_close_a_failed_request() {
    use teams_ui::team_menu::TeamChoice;
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(MEMBER_OF, "Beta", false)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.team_chosen(team(MEMBER_OF, "Beta", false), TeamChoice::AddMember, window, cx);
        });
    });
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .unwrap()
        .downcast::<AddMembersDialog>()
        .unwrap();
    assert!(cx.update(|_, cx| dialog.read(cx).sends_request()));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.pick(
                teams_proto::TeamPerson {
                    mri: "8:orgid:ada".to_owned(),
                    name: "Ada".to_owned(),
                    email: None,
                    job_title: None,
                },
                window,
                cx,
            );
        });
    });
    click(cx, "add-members-add");
    assert!(bounds(cx, "add-members-result").is_some());
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(matches!(&writes[0].0, TeamWrite::RequestMembers { group, members }
        if group == GROUP && members == &[teams_proto::Mri::new("8:orgid:ada")]));
    drop(writes);
    click(cx, "add-members-close");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn tag_permissions_write_independently_of_the_team_settings_snapshot() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    *rig.core.team_settings.lock().unwrap() = Some("{}".to_owned());
    rig.core.team_tags.lock().unwrap().owners_only = true;
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    manage(&shell, ManageTab::Settings, cx);
    click(cx, "manage-section-members");
    click(cx, "member-manage-tags-enabled");
    click(cx, "manage-section-members");
    click(cx, "manage-section-mentions");
    click(cx, "mention-settings-automatic-tags");
    click(cx, "manage-section-mentions");
    click(cx, "manage-section-tags");
    click(cx, "tag-settings-source-toggle");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), 3);
    assert!(
        matches!(&writes[0].0, TeamWrite::TagSettings { team, owners_only: false, automatic: false } if team == OWNED)
    );
    assert!(matches!(
        &writes[1].0,
        TeamWrite::TagSettings {
            owners_only: true,
            automatic: true,
            ..
        }
    ));
    assert!(matches!(
        &writes[2].0,
        TeamWrite::TagSettings {
            owners_only: true,
            automatic: true,
            ..
        }
    ));
}

fn numbers(name: &str, active_users: u64) -> teams_proto::TeamAnalytics {
    teams_proto::TeamAnalytics {
        team: format!("19:{name}@thread.tacv2"),
        name: name.to_owned(),
        active_users,
        posts: 4,
        replies: 2,
        ..teams_proto::TeamAnalytics::default()
    }
}

#[test]
fn the_analytics_tab_reads_every_team_s_numbers_for_the_period_picked() {
    use teams_proto::AnalyticsPeriod;
    let rig = Rig::start();
    *rig.core.all_analytics.lock().unwrap() = vec![numbers("Alpha", 3), numbers("Beta", 1)];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-tab-analytics");
    assert_eq!(
        *rig.core.all_analytics_asked.lock().unwrap(),
        [AnalyticsPeriod::LastSevenDays],
        "the last seven days first, as Teams has them"
    );
    assert!(bounds(cx, "teams-analytics").is_some());
    click(cx, "teams-period");
    click(cx, "teams-period-last-ninety-days");
    assert_eq!(
        rig.core.all_analytics_asked.lock().unwrap().last(),
        Some(&AnalyticsPeriod::LastNinetyDays)
    );
    assert!(
        bounds(cx, "teams-period-last-ninety-days").is_none(),
        "the menu closes on its pick"
    );
    // a period without numbers: the tab says so
    rig.core.all_analytics.lock().unwrap().clear();
    click(cx, "teams-period");
    click(cx, "teams-period-last-thirty-days");
    assert_eq!(rig.core.all_analytics_asked.lock().unwrap().len(), 3);
    // the period's menu closes on a click outside it
    click(cx, "teams-period");
    assert!(bounds(cx, "teams-period-last-seven-days").is_some());
    click(cx, "teams-analytics");
    assert!(bounds(cx, "teams-period-last-seven-days").is_none());
}

#[test]
fn a_menus_own_button_closes_it_and_leaves_it_closed() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-tab-analytics");
    let open = |cx: &mut VisualTestContext| bounds(cx, "teams-period-last-seven-days").is_some();
    click(cx, "teams-period");
    assert!(open(cx), "the button opens its closed menu");
    click(cx, "teams-period");
    assert!(
        !open(cx),
        "the button of the open menu closes it, not closing and opening it again"
    );
    click(cx, "teams-period");
    assert!(open(cx), "and opens it again after");
    click(cx, "teams-analytics");
    assert!(!open(cx), "a click elsewhere still closes it");
    // beside the button, in the row its menu hangs from, is elsewhere too
    click(cx, "teams-period");
    let button = bounds(cx, "teams-period").expect("the period's button");
    let beside = gpui::point(button.right() + gpui::px(40.0), button.center().y);
    cx.simulate_click(beside, Modifiers::none());
    assert!(!open(cx), "a click beside the button closes it");
    // the same in Manage team, whose menus keep their own state
    manage(&shell, ManageTab::Analytics, cx);
    let open = |cx: &mut VisualTestContext| bounds(cx, "manage-period-last-seven-days").is_some();
    click(cx, "manage-period");
    assert!(open(cx));
    click(cx, "manage-period");
    assert!(!open(cx), "Manage team's period button closes its open menu");
    click(cx, "manage-period");
    assert!(open(cx));
}

#[test]
fn a_rows_more_options_button_closes_its_open_menu_and_leaves_it_closed() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    let open = |cx: &mut VisualTestContext| bounds(cx, "team-menu-0").is_some();
    open_row_menu(cx, 0);
    assert!(open(cx), "the row's \"...\" opens its menu");
    click(cx, "teams-row-more-0");
    assert!(
        !open(cx),
        "the \"...\" of the open menu closes it, not closing and opening it again"
    );
    click(cx, "teams-row-more-0");
    assert!(open(cx), "and opens it again after");
    click(cx, "teams-page-title");
    assert!(!open(cx), "a click elsewhere still closes it");
}

#[test]
fn the_pending_invites_tab_lists_each_invitation_waiting() {
    let rig = Rig::start();
    *rig.core.invites.lock().unwrap() = vec![teams_proto::PendingInvite {
        id: "invite-1".to_owned(),
        channel: "Made-up shared".to_owned(),
        team: "Gamma".to_owned(),
        from: "Lab Bee".to_owned(),
    }];
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    assert!(bounds(cx, "teams-invites").is_none());
    click(cx, "teams-tab-pending-invites");
    assert!(bounds(cx, "teams-invites").is_some(), "the invitation's table");
}

#[test]
fn create_team_s_dropdown_offers_create_and_join() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-create-more");
    assert!(bounds(cx, "teams-create-menu-join").is_some());
    click(cx, "teams-create-menu-join");
    assert!(bounds(cx, "teams-create-menu-join").is_none(), "the dropdown closes");
    assert!(bounds(cx, "join-team").is_some(), "Join a team opens");
    click(cx, "join-back");
    open_page(&shell, cx);
    click(cx, "teams-create-more");
    click(cx, "teams-create-menu-create");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<CreateTeamDialog>().ok());
    assert!(dialog.is_some(), "Create team's dialog opens");
}

#[test]
fn a_cloud_without_apps_offers_no_apps_tab() {
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        outgoing_webhooks: false,
        ..teams_core::Capabilities::ALL
    });
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    click(cx, "teams-row-0");
    assert!(bounds(cx, "manage-tab-members").is_some());
    assert!(bounds(cx, "manage-tab-apps").is_none(), "no app catalogue to list");
}
