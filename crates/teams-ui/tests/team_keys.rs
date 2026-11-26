//! The teams' keyboard on GPUI's test context, over a fake core, in the kit's root (which takes Tab): the sidebar's
//! rows and a team's "..." menu by keyboard, the "Create a channel" dialog's Tab order and its layout cards, "Your teams
//! and channels" by keyboard, Manage team's tabs, Create team's kind cards, and the Ctrl+K commands for a team.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{
    AppContext as _, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
};
use teams_proto::{ChannelLayout, NewChannelKind, TeamPerson, TeamRole};
use teams_store::{ChannelKind, Team, TeamAbout, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::manage_team::ManageTab;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::team_dialogs::{AddMembersDialog, CreateTeamDialog};
use teams_ui::team_menu::{TeamChoice, entries};
use teams_ui::teams_page::PageTab;
use teams_ui::window::KitRoot;

const ALPHA: &str = "19:team0001@thread.tacv2";
const BETA: &str = "19:team0002@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

fn team(id: &str, name: &str, owner: bool) -> Team {
    Team {
        id: id.to_owned(),
        name: name.to_owned(),
        group_id: Some(GROUP.to_owned()),
        owner,
        can_add_channel: true,
        can_add_private_channel: true,
        about: TeamAbout {
            description: "Made up".to_owned(),
            people: 3,
            ..TeamAbout::default()
        },
        channels: vec![
            TeamChannel {
                id: id.to_owned(),
                name: "General".to_owned(),
                general: true,
                kind: ChannelKind::Standard,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
            TeamChannel {
                id: format!("{id}-two"),
                name: "Made-up Channel".to_owned(),
                general: false,
                kind: ChannelKind::Standard,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
        ],
        ..Team::default()
    }
}

/// The shell in the kit's root, as the app opens it, the shell holding the keyboard.
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
    let (root, cx) = app.add_window_view(|window, cx| {
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        KitRoot::new(shell, window, cx)
    });
    let shell = cx.update(|_, cx| root.read(cx).view().clone().downcast::<Shell>().unwrap());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.take_focus(window, cx)));
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
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Presses each key of `keys` (down and up, as a person does: a focused control is clicked on the key's release).
fn press(cx: &mut VisualTestContext, keys: &'static str) {
    for spec in keys.split(' ') {
        let keystroke = Keystroke::parse(spec).unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
    }
}

fn sidebar_has_keyboard(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| shell.read(cx).sidebar().read(cx).has_keyboard(window))
}

/// Ctrl+2 to the list, then from "See all your teams" (End) up to the team's line through its channel (Up, Left).
fn keyboard_to_last_team(cx: &mut VisualTestContext) {
    press(cx, "ctrl-2 end up left");
}

#[test]
fn a_teams_menu_opens_by_keyboard_and_gives_the_keyboard_back_on_escape() {
    let rig = Rig::start();
    let alpha = team(ALPHA, "Alpha", true);
    let (shell, cx) = shell_window(&rig, vec![alpha.clone()]);
    keyboard_to_last_team(cx);
    assert!(sidebar_has_keyboard(&shell, cx), "Ctrl+2 gives the list the keyboard");
    assert!(bounds(cx, "sidebar-cursor").is_some(), "the keyboard's row is ringed");
    assert!(bounds(cx, "team-more-0").is_some(), "its \"...\" shows");
    // Left on the team's line folds it, Right unfolds it
    press(cx, "left");
    assert!(!cx.update(|_, cx| shell.read(cx).sidebar().read(cx).team_open(ALPHA)));
    press(cx, "right");
    assert!(cx.update(|_, cx| shell.read(cx).sidebar().read(cx).team_open(ALPHA)));

    press(cx, "shift-f10");
    assert!(bounds(cx, "team-menu-0").is_some(), "Shift+F10 opens the menu");
    assert!(!sidebar_has_keyboard(&shell, cx), "the menu has it");
    press(cx, "escape");
    assert!(bounds(cx, "team-menu-0").is_none(), "Escape closes it");
    assert!(
        sidebar_has_keyboard(&shell, cx),
        "and gives the keyboard back to the team's line"
    );

    // the menu key, Down to Add channel, Enter
    press(cx, "menu");
    let to_add = entries(&alpha, false)
        .iter()
        .position(|choice| *choice == TeamChoice::AddChannel)
        .expect("an owner adds channels");
    for _ in 0..to_add {
        press(cx, "down");
    }
    press(cx, "enter");
    assert!(
        cx.update(|_, cx| shell.read(cx).channel_dialog().is_some()),
        "Add channel opened"
    );
    press(cx, "escape");
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()));
    assert!(
        sidebar_has_keyboard(&shell, cx),
        "the dialog gives the keyboard back to the team's line"
    );
}

#[test]
fn enter_on_a_channel_opens_it_and_on_see_all_your_teams_opens_the_page() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    press(cx, "ctrl-2 end up enter");
    assert!(
        cx.update(|_, cx| matches!(shell.read(cx).main(), Main::Channel(id) if id == &format!("{ALPHA}-two"))),
        "the channel opened"
    );
    press(cx, "ctrl-2 end enter");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::TeamsPage);
}

#[test]
fn the_channel_dialogs_tab_order_goes_round_its_fields_and_the_layouts_are_a_radio_group() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", false)]);
    keyboard_to_last_team(cx);
    press(cx, "menu");
    let to_add = entries(&team(ALPHA, "Alpha", false), false)
        .iter()
        .position(|choice| *choice == TeamChoice::AddChannel)
        .expect("a member adds channels here");
    for _ in 0..to_add {
        press(cx, "down");
    }
    press(cx, "enter");
    let dialog = cx
        .update(|_, cx| shell.read(cx).channel_dialog().cloned())
        .expect("the dialog");
    // the name has the keyboard
    cx.simulate_input("Ops");
    // Tab: the description
    press(cx, "tab");
    cx.simulate_input("About");
    // Tab: the types' info, Tab: the type's box, which Enter opens and Enter picks from
    press(cx, "tab tab enter");
    assert!(bounds(cx, "channel-types").is_some(), "Enter opened the types");
    press(cx, "enter");
    assert!(bounds(cx, "channel-types").is_none());
    assert!(
        cx.update(|_, cx| dialog.read(cx).can_create(cx)),
        "named, typed: Create is on"
    );
    // Tab: the chosen layout, whose arrows choose the other
    press(cx, "tab");
    assert!(bounds(cx, "channel-layout-threads").is_some());
    press(cx, "right");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).layout()), ChannelLayout::Posts);
    press(cx, "left");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).layout()), ChannelLayout::Threads);
    press(cx, "down");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).layout()), ChannelLayout::Posts);
    // Tab: Cancel, Tab: Create, Tab: round to the name; Shift+Tab back to Create, Shift+Tab to Cancel, which Enter
    // clicks
    press(cx, "tab tab tab");
    assert!(
        cx.update(|_, cx| shell.read(cx).channel_dialog().is_some()),
        "the dialog keeps Tab"
    );
    press(cx, "shift-tab shift-tab enter");
    assert!(
        cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()),
        "Cancel closed it"
    );
    assert!(rig.core.channels_asked.lock().unwrap().is_empty(), "nothing created");
    assert!(sidebar_has_keyboard(&shell, cx));
}

#[test]
fn the_channel_dialogs_create_is_reached_last_once_it_is_on() {
    let rig = Rig::start();
    *rig.core.channel_answer.lock().unwrap() = Some(Err(teams_core::ChannelProblem::Other));
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", false)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_channel_dialog(team(ALPHA, "Alpha", false), window, cx);
        });
    });
    cx.simulate_input("Ops");
    // Shift+Tab from the name: Create is off, so Cancel
    press(cx, "shift-tab enter");
    assert!(
        cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()),
        "Cancel, Create passed while off"
    );

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_channel_dialog(team(ALPHA, "Alpha", false), window, cx);
        });
    });
    let dialog = cx
        .update(|_, cx| shell.read(cx).channel_dialog().cloned())
        .expect("the dialog");
    cx.simulate_input("Ops");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.pick_kind(NewChannelKind::Standard, cx)));
    press(cx, "shift-tab enter");
    let asked = rig.core.channels_asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "Shift+Tab from the name is Create once it is on");
    assert_eq!(asked[0].channel.name, "Ops");
}

#[test]
fn the_teams_page_takes_the_keyboard_on_its_rows_menus_and_tabs() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true), team(BETA, "Beta", false)]);
    click(cx, "see-all-teams");
    assert!(
        bounds(cx, "teams-row-cursor").is_some(),
        "the page opens with the keyboard on its rows"
    );
    // Down to Beta, Shift+F10: its menu, its first entry lit
    press(cx, "down shift-f10");
    assert!(bounds(cx, "team-menu-1").is_some());
    press(cx, "escape");
    assert!(bounds(cx, "team-menu-1").is_none());
    assert!(
        bounds(cx, "teams-row-cursor").is_some(),
        "the keyboard is back on the row"
    );

    // Shift+Tab: Search teams, Shift+Tab: Sort, which Enter opens; Down, Down: Z-A, Enter
    press(cx, "shift-tab shift-tab enter");
    assert!(bounds(cx, "teams-sort-za").is_some(), "the sort's menu opened");
    press(cx, "down down enter");
    assert!(bounds(cx, "teams-sort-za").is_none(), "it closed");
    let first = bounds(cx, "teams-row-0").expect("a row");
    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    // the keyboard is back on Sort: Enter opens the menu again, Escape closes it
    press(cx, "enter");
    assert!(bounds(cx, "teams-sort-az").is_some(), "the keyboard came back to Sort");
    press(cx, "escape");
    assert!(bounds(cx, "teams-sort-az").is_none());

    // Shift+Tab: "N more", which Enter opens and Down, Enter picks from
    press(cx, "shift-tab enter down enter");
    assert!(
        bounds(cx, "teams-filter-hidden").is_some(),
        "the first of the rest is the filter"
    );

    // back past the filter now chosen, Teams you own, All, Create team's dropdown and Create team to the tabs: Right
    // shows Pending invites, Left back to Teams
    press(cx, "shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab right");
    let tab =
        |cx: &mut VisualTestContext| cx.update(|_, cx| shell.read(cx).teams_page().map(|page| page.read(cx).tab()));
    assert_eq!(tab(cx), Some(PageTab::PendingInvites));
    press(cx, "left");
    assert_eq!(tab(cx), Some(PageTab::Teams));

    // from the rows, Enter: Manage team
    cx.update(|window, cx| {
        let page = shell.read(cx).teams_page().cloned().expect("the page");
        page.update(cx, |page, cx| {
            page.set_filter(teams_ui::teams_page::Filter::All, cx);
            page.focus_rows(window, cx);
        });
    });
    press(cx, "enter");
    assert!(cx.update(|_, cx| matches!(shell.read(cx).main(), Main::ManageTeam(_))));
}

#[test]
fn manage_teams_tabs_move_by_arrows() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(ALPHA.to_owned(), ManageTab::Channels, window, cx);
        });
    });
    let tab =
        |cx: &mut VisualTestContext| cx.update(|_, cx| shell.read(cx).manage_team().map(|page| page.read(cx).tab()));
    click(cx, "manage-tab-channels");
    press(cx, "right");
    assert_eq!(tab(cx), Some(ManageTab::Members));
    press(cx, "left left");
    assert_ne!(tab(cx), Some(ManageTab::Members), "Left wraps to the last tab");
    press(cx, "home");
    assert_eq!(tab(cx), Some(ManageTab::Channels));
}

#[test]
fn create_teams_kind_cards_are_reached_and_picked_by_keyboard() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    click(cx, "see-all-teams");
    click(cx, "teams-create");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<CreateTeamDialog>().ok())
        .expect("the dialog");
    // Tab: the description, Tab: the team type, Enter: its step, the keyboard on Private
    press(cx, "tab tab enter");
    assert!(bounds(cx, "create-team-private").is_some());
    // Down: Public, Enter picks it and goes back to the form
    press(cx, "down enter");
    assert!(cx.update(|_, cx| dialog.read(cx).public()));
    assert!(bounds(cx, "create-team-name").is_some(), "back on the form");
    // the keyboard is back on the team type: Enter opens its step again
    press(cx, "enter");
    assert!(bounds(cx, "create-team-public").is_some());
    press(cx, "escape");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn ctrl_k_offers_a_teams_commands_and_runs_them() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true), team(BETA, "Beta", false)]);
    press(cx, "ctrl-k");
    let switcher = cx
        .update(|_, cx| shell.read(cx).switcher().cloned())
        .expect("the switcher");
    let commands = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            switcher
                .read(cx)
                .commands()
                .into_iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        })
    };
    let listed = commands(cx);
    assert!(listed.contains(&"Create team".to_owned()));
    assert!(listed.contains(&"Join team".to_owned()));
    assert!(
        !listed.iter().any(|name| name.starts_with("Add channel")),
        "no team's command until something is typed"
    );
    cx.simulate_input("add channel beta");
    assert_eq!(commands(cx).first().map(String::as_str), Some("Add channel: Beta"));
    press(cx, "enter");
    let dialog = cx.update(|_, cx| shell.read(cx).channel_dialog().cloned());
    assert!(dialog.is_some(), "Add channel opened on Beta");
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
}

#[test]
fn the_read_only_mode_offers_no_team_command_that_writes() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    press(cx, "ctrl-k");
    cx.simulate_input("team");
    let switcher = cx
        .update(|_, cx| shell.read(cx).switcher().cloned())
        .expect("the switcher");
    let listed: Vec<String> = cx.update(|_, cx| {
        switcher
            .read(cx)
            .commands()
            .into_iter()
            .map(|name| name.to_string())
            .collect()
    });
    assert!(!listed.contains(&"Create team".to_owned()));
    assert!(!listed.iter().any(|name| name.starts_with("Add channel")));
    assert!(listed.contains(&"Manage team: Alpha".to_owned()));
}

fn person(mri: &str, name: &str) -> TeamPerson {
    TeamPerson {
        mri: mri.to_owned(),
        name: name.to_owned(),
        email: Some("someone@lab.example.com".to_owned()),
        job_title: None,
    }
}

#[test]
fn add_members_finds_picks_and_sets_a_role_by_keyboard() {
    let rig = Rig::start();
    *rig.core.addable.lock().unwrap() = vec![person(common::LAB_B, "Lab B")];
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.team_chosen(team(ALPHA, "Alpha", true), TeamChoice::AddMember, window, cx);
        });
    });
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AddMembersDialog>().ok())
        .expect("the dialog");
    cx.simulate_input("lab");
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    assert!(bounds(cx, "add-members-found-0").is_some());
    // Tab: the one found, Enter picks them, the keyboard back in the box
    press(cx, "tab enter");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).picked().len()), 1);
    // Tab: their role, Enter: its menu, Down, Down: Owner, Enter
    press(cx, "tab enter");
    assert!(
        bounds(cx, "add-members-role-0-owner").is_some(),
        "the role's menu opened"
    );
    press(cx, "down down enter");
    assert!(bounds(cx, "add-members-role-0-owner").is_none());
    assert_eq!(cx.update(|_, cx| dialog.read(cx).picked()[0].role), TeamRole::Owner);
    // the keyboard is back on the role: Enter opens its menu again, Escape closes it, Escape the dialog
    press(cx, "enter");
    assert!(bounds(cx, "add-members-role-0-member").is_some());
    press(cx, "escape");
    assert!(bounds(cx, "add-members-role-0-member").is_none());
    assert!(
        cx.update(|_, cx| shell.read(cx).team_dialog().is_some()),
        "Escape closed only the menu"
    );
    press(cx, "escape");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn a_private_channels_people_are_found_and_picked_by_keyboard() {
    let rig = Rig::start();
    *rig.core.team_found.lock().unwrap() = vec![person(common::LAB_A, "Lab A"), person(common::LAB_B, "Lab B")];
    let (shell, cx) = shell_window(&rig, vec![team(ALPHA, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_channel_dialog(team(ALPHA, "Alpha", true), window, cx);
        });
    });
    let dialog = cx
        .update(|_, cx| shell.read(cx).channel_dialog().cloned())
        .expect("the dialog");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.people_step(GROUP.to_owned(), None, cx)));
    assert!(bounds(cx, "channel-people").is_some());
    cx.simulate_input("lab");
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    assert!(bounds(cx, "channel-person-1").is_some());
    // Tab: the first found, Down: the second, Enter picks them
    press(cx, "tab down enter");
    assert!(bounds(cx, "channel-picked-0").is_some());
    assert!(bounds(cx, "channel-person-0").is_none(), "the box was cleared");
}
