//! A look at the app's views in the UI sandbox, on the mock (`harness = false`: GPUI runs on the process's main
//! thread): the screenshots a change of the UI is checked by, kept under `target/ui-shots/tour-*.png`.
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`], only the view trace's pipe passed
//! through) as the app of lab account a, its engine on the mock chat service and its Trouter (`teams-mock`, through
//! the `test-support` mock origins, a test roster and log). Once the window is drawn and the chats are listed, the
//! child walks the views one step at a time (no chat open; the a–b chat with a message selected and its actions
//! shown; a reply begun; the account menu; every page of the settings, Calls with made-up devices; the appearance
//! and the chat again in Teams Dark; a message with files, its cards and their thumbnails, in Teams Dark and Light,
//! its first video played in its card and paused; a message of lines, small pictures and a block of code, in Teams
//! Dark and Light; a picture opened in the picture viewer, fitted and zoomed; the 12-hour time format and a chat's
//! times on it; a group chat's presence; the "Chats" section's See more, what it brings and a chat opened from below
//! the rows shown; New message, its To: line's suggestions, the chat two people already have and a group name, in
//! Teams Dark and Light; a favourite channel opened from "Favorites", a post's hover bar and its thread beside the posts; forwarded messages; a call's own window, its participants' pane, its Call health, beside the main window
//! with its in-call strip, and in Teams Dark; a group call with Raise and React, its People pane, its speaker view and
//! its chat, its organizer's Mute all, People added to the chat and Controls, an attendee's greyed controls and
//! "You've been muted", in Teams Light and Dark; a person's profile card, compact beside a group chat's and a channel
//! post's face and full on each of its tabs, in Teams Light and Dark) and says
//! `shot <name>` after each; the parent screenshots the window and lets it go on. The views are driven from inside
//! the app, never by screen coordinates.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Context, Entity, ListOffset, Window, WindowHandle, WindowingModes, px};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::VideoOutput;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_media::devices::catalog::{DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::devices::pattern::TestPattern;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AMS_BASE, AX_CHAT, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C,
    LokiCard, MIDDLE_TIER_BASE, OUTSIDER, PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock,
    USER_PROFILE_BASE,
};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::VideoFeed;
use teams_ui::call_stage::CallStage;
use teams_ui::chat_people::{ChatPeople, HistoryChoice, Picked};
use teams_ui::chat_view::{Item, MessageAction};
use teams_ui::compose::Compose;
use teams_ui::global_search::{Mode as SearchMode, Tab};
use teams_ui::new_chat::{NewChat, Recipient, Suggested};
use teams_ui::profile_card::{CardOf, CardTab, Form};
use teams_ui::settings::VideoEffect;
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::sidebar::Sidebar;
use teams_ui::update::UpdateStatus;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{event_form, picture_viewer};
use tokio::sync::broadcast;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// The mock's team.
const TEAM: &str = "19:channel0001@thread.tacv2";
/// The post of the mock's team channel (`channel-messages.json`) with replies under it.
const CHANNEL_POST: &str = "1760000300000";
/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_TOUR_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "NGTEAMS_TOUR_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent makes once it took a step's screenshot.
const NEXT: &str = "tour-next";
/// How long a step's views get to settle before their screenshot.
const SETTLE: Duration = Duration::from_millis(1500);

/// The video effects' camera when whoever runs the tour put a picture at [`PictureCamera::SAMPLE`] (a person's, to see
/// the effects' edge; it stays in `target/`, never in the repo): that picture, still, at the camera's pace. Without
/// one the effects preview shows the test pattern.
struct PictureCamera(teams_media::devices::VideoFrame);

impl PictureCamera {
    const SAMPLE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/tour-camera.png");

    fn sample() -> Option<Self> {
        let picture = image::open(Self::SAMPLE).ok()?.to_rgb8();
        let (width, height) = picture.dimensions();
        Some(Self(teams_media::devices::VideoFrame {
            width,
            height,
            format: teams_media::devices::PixelFormat::Rgb,
            stride: width * 3,
            data: picture.into_raw(),
        }))
    }
}

/// A speaker that never opens: the settings' previews are offered, and nothing plays.
struct NoDevices;

impl teams_media::devices::Devices for NoDevices {
    fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }

    fn speaker(&self) -> Result<Box<dyn teams_media::devices::AudioSink>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }

    fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
}

impl teams_media::devices::Devices for PictureCamera {
    fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }

    fn speaker(&self) -> Result<Box<dyn teams_media::devices::AudioSink>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }

    fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
        Ok(Box::new(PictureCamera(self.0.clone())))
    }
}

impl teams_media::devices::VideoSource for PictureCamera {
    fn next_frame(
        &mut self,
        _: Duration,
    ) -> Result<Option<teams_media::devices::VideoFrame>, teams_media::devices::DeviceError> {
        thread::sleep(Duration::from_millis(33));
        Ok(Some(self.0.clone()))
    }
}

/// A step of the tour: its screenshot's name and what it does to the shell shown.
type Step = (&'static str, fn(&Entity<Shell>, &mut Window, &mut App));

/// Where the tour's made-up channel meeting is: the mock's team channel `channel`, a made-up mailbox.
fn channel_meeting_place(channel: &str) -> event_form::MeetingPlace {
    event_form::MeetingPlace {
        channel: teams_core::MeetingChannel {
            channel: channel.to_owned(),
            mailbox: "made-up-team@lab.example.test".to_owned(),
        },
        team: "Made-up Team".to_owned(),
        name: "General".to_owned(),
    }
}

/// A made-up channel meeting the account scheduled, as its team's calendar (`mailbox`) gives it.
fn channel_meeting(mailbox: &str) -> Option<teams_core::FullEvent> {
    let data = serde_json::json!({"cid": TEAM, "mid": 1_760_000_400_100_u64, "uid": "8:orgid:00000000-0000-4000-8000-00000000000a"});
    let event = serde_json::json!({
        "objectId": "AQMkMadeUp",
        "iCalUID": "0c0d",
        "subject": "Made-up planning",
        "startTime": "2026-11-04T12:00:00+00:00",
        "endTime": "2026-11-04T12:30:00+00:00",
        "isOnlineMeeting": true,
        "isOrganizer": true,
        "location": "Microsoft Teams Meeting",
        "bodyContent": "<p>Made-up agenda</p>",
        "skypeTeamsData": data.to_string(),
    });
    teams_proto::ChannelEventRead::new(mailbox, "0c0d")?
        .read_answer(&serde_json::to_vec(&event).ok()?)
        .ok()
}

/// The tour.
const STEPS: [Step; 5 + 284] = [
    ("no-chat", |_, _, _| {}),
    ("chat", |shell, window, cx| {
        // the mock answers no one's presence: b's as UPS would tell it, for the avatar's badge
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::Presence {
                mri: LAB_B.to_owned(),
                availability: teams_core::Availability::Away,
            },
            cx,
        );
        shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
    }),
    // the rail's Activity: the feed in the list pane, unread items bold with a dot, the rail's count gone once shown
    ("activity", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.show_activity(window, cx));
    }),
    // an item's "More options" (Mark as read or unread, Remove), shown in place of its date
    ("activity-item-menu", |shell, _, cx| {
        with_activity(shell, cx, |view, cx| {
            let first = view.feed().items.first().map(|item| item.id.clone());
            view.show_item_menu(first, cx);
        });
    }),
    // the header's "More options": Mark all as read
    ("activity-header-menu", |shell, _, cx| {
        with_activity(shell, cx, |view, cx| view.show_header_menu(true, cx));
    }),
    // the Unread chip chosen
    ("activity-unread", |shell, _, cx| {
        with_activity(shell, cx, |view, cx| {
            view.show_header_menu(false, cx);
            view.choose_filter(teams_core::activity::ActivityFilter::Unread, cx);
        });
    }),
    // the search's box, its words keeping the items that have them
    ("activity-search", |shell, window, cx| {
        with_activity(shell, cx, |view, cx| {
            view.choose_filter(teams_core::activity::ActivityFilter::Unread, cx);
            view.search_for("Lab C", window, cx);
        });
    }),
    // in Teams Dark, a mention opened: its chat at the message on the right, the item read and selected
    ("activity-dark-opened", |shell, window, cx| {
        fixed_theme(shell, Some(teams_ui::theme::DARK_KEY), cx);
        with_activity(shell, cx, |view, cx| {
            view.search_for("", window, cx);
            let mention = view
                .feed()
                .items
                .iter()
                .find(|item| matches!(item.kind, teams_core::activity::ActivityKind::Mention(_)))
                .map(|item| item.id.clone());
            if let Some(id) = mention {
                view.open(&id, cx);
            }
        });
    }),
    // back to the chats, the theme following the system again as before, for the steps after
    ("activity-left", |shell, window, cx| {
        fixed_theme(shell, None, cx);
        shell.update(cx, |shell, cx| {
            shell.show_chats(window, cx);
            shell.open_chat(AB_CHAT.to_owned(), window, cx);
        });
    }),
    // the title bar's update button (`teams_ui::update`), its states as the app's updater sets them
    ("update-available", |_, _, cx| {
        update_shown(available(), cx);
    }),
    ("update-downloading", |_, _, cx| update_shown(downloading(), cx)),
    ("update-ready", |_, _, cx| {
        update_shown(UpdateStatus::Ready(TOUR_RELEASE.into()), cx);
    }),
    ("update-failed", |_, _, cx| update_shown(update_failed(), cx)),
    ("chat-hover-theirs", |shell, _, cx| {
        update_shown(UpdateStatus::None, cx);
        hover_last(shell, false, cx);
    }),
    ("chat-hover-mine", |shell, _, cx| hover_last(shell, true, cx)),
    ("chat-reactors", |shell, _, cx| show_reactors(shell, cx)),
    ("chat-reactors-dark", |shell, _, cx| {
        fixed_theme(shell, Some(teams_ui::theme::DARK_KEY), cx);
    }),
    ("chat-reaction-focus", |shell, window, cx| {
        focus_reaction(shell, window, cx);
    }),
    ("chat-actions", |shell, window, cx| {
        fixed_theme(shell, None, cx);
        hover_none(shell, cx);
        select_last_message(shell, window, cx);
    }),
    ("chat-reply", |shell, window, cx| {
        if let Some((chat, message)) = last_message(shell, cx) {
            chat.update(cx, |chat, cx| chat.act(MessageAction::Reply, &message, window, cx));
        }
    }),
    // the compose box's emoji picker, opened from its button over the box
    ("chat-emoji", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| {
            compose.cancel(window, cx);
            compose.toggle_emoji(window, cx);
        });
    }),
    // the box with the focus and text in it: its accent line, Send in the brand colour
    ("chat-compose", |shell, window, cx| {
        type_in_compose(shell, "Lunch at noon?", window, cx);
    }),
    ("chat-attachment-error", |shell, window, cx| {
        let path = std::env::temp_dir().join(format!("tour-attachment-{}.txt", std::process::id()));
        if let Err(error) = fs::write(&path, "A made-up local file for the attachment error view.") {
            eprintln!("tour attachment: {error}");
            cx.quit();
            return;
        }
        with_compose(shell, cx, |compose, cx| compose.attach_files(&[path], window, cx));
    }),
    ("chat-attachment-discarded", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.discard(window, cx));
        let path = std::env::temp_dir().join(format!("tour-attachment-{}.txt", std::process::id()));
        if let Err(error) = fs::remove_file(path) {
            eprintln!("tour attachment cleanup: {error}");
            cx.quit();
        }
        type_in_compose(shell, "Lunch at noon?", window, cx);
    }),
    // the box expanded with its format bar (Ctrl+Shift+X), holding formatted text: a heading, bold, a colour and a
    // highlight, a bulleted list
    ("chat-compose-expanded", |shell, window, cx| {
        expand_with_formats(shell, window, cx);
    }),
    // the font colour menu under its button
    ("chat-compose-colours", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| {
            compose.format_bar().update(cx, |bar, cx| {
                bar.toggle(teams_ui::editor::toolbar::Menu::FontColour, window, cx);
            });
        });
    }),
    // sent: drawn by the message renderer as the editor showed it, the box collapsed again
    ("chat-formatted-sent", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| {
            compose.format_bar().update(cx, |bar, cx| bar.close(window, cx));
            compose.send(window, cx);
        });
    }),
    // a picture on the clipboard pasted into the a–b chat's box below its text (the app's own clipboard, in the
    // sandbox), sent to the mock's media service at once, as Teams shows a pasted picture before Send
    ("compose-picture", |shell, window, cx| {
        paste_picture(shell, window, cx);
    }),
    ("compose-picture-dark", |shell, _, cx| {
        fixed_theme(shell, Some(teams_ui::theme::DARK_KEY), cx);
    }),
    // sent: the message with the picture in the chat
    ("chat-picture-sent-dark", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.send(window, cx));
    }),
    ("chat-picture-sent", |shell, _, cx| fixed_theme(shell, None, cx)),
    // German picked, the made-up tool ([`STAGED_CHAT_CLAUDE`]) not answering yet: the header's button spins and each
    // message waits in place, faded, "Translating…" beside its time
    ("chat-translating", |shell, _, cx| {
        staged_chat_tools(cx);
        translate_chat(shell, Some("de"), cx);
    }),
    // the newer half of the batch written (the tool answers the newest first): those messages, at the chat's end, show
    // their translation, the older ones still wait
    ("chat-translating-partial", |_, _, _| release_staged(1)),
    // the chat translated into German on this computer (a made-up tool, [`FAKE_CHAT_CLAUDE`]): the header's language
    // picker lit, each message translated with Teams' translation mark beside its time
    ("chat-translated", |shell, _, cx| {
        release_staged(2);
        chat_tools(FAKE_CHAT_CLAUDE, cx);
        translate_chat(shell, Some("de"), cx);
        translate_another_chat(shell, Some("de"), cx);
    }),
    // "See original message" on the last of someone else's messages
    ("chat-translated-original", |shell, _, cx| {
        let Some(chat) = shell.read(cx).chat().cloned() else {
            return;
        };
        let theirs = chat.read(cx).items().iter().rev().find_map(|item| match item {
            Item::Message(message) if !message.own && message.translation.is_some() => Some(message.id.clone()),
            _ => None,
        });
        if let Some(id) = theirs {
            let translation = chat.read(cx).translation().clone();
            translation.update(cx, |translation, cx| translation.toggle_original(&id, cx));
        }
    }),
    // a reply to someone else's translated message: the box shows its quote as the chat does
    ("chat-translated-reply", |shell, window, cx| {
        let Some(chat) = shell.read(cx).chat().cloned() else {
            return;
        };
        // the last of someone else's translated messages, shown translated again if "See original message" showed it
        // as written
        let theirs = chat.read(cx).items().iter().rev().find_map(|item| match item {
            Item::Message(message) if !message.own => match &message.translation {
                Some(teams_ui::chat_translate::Mark::Translated { .. }) => Some((message.id.clone(), false)),
                Some(teams_ui::chat_translate::Mark::Original { .. }) => Some((message.id.clone(), true)),
                _ => None,
            },
            _ => None,
        });
        if let Some((id, original)) = theirs {
            if original {
                let translation = chat.read(cx).translation().clone();
                translation.update(cx, |translation, cx| translation.toggle_original(&id, cx));
            }
            // once the chat drew it translated again
            window.defer(cx, move |window, cx| {
                chat.update(cx, |chat, cx| chat.act(MessageAction::Reply, &id, window, cx));
            });
        }
    }),
    // the header's language menu, a few letters typed into its search field
    ("chat-translate-menu", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.cancel(window, cx));
        language_menu(shell, Some("ger"), window, cx);
    }),
    // the same in Teams Dark, nothing typed
    ("chat-translate-menu-dark", |shell, window, cx| {
        language_menu(shell, None, window, cx);
        show_chat_in_theme(shell, teams_ui::theme::DARK_KEY, window, cx);
        language_menu(shell, Some(""), window, cx);
    }),
    // a message written in German, being translated into the chat's language before it goes
    ("chat-translated-sending", |shell, window, cx| {
        language_menu(shell, None, window, cx);
        show_chat_in_theme(shell, teams_ui::theme::LIGHT_KEY, window, cx);
        type_in_compose(shell, "Hallo! Passt dir Mittagessen um zwölf?", window, cx);
        with_compose(shell, cx, |compose, cx| compose.send(window, cx));
    }),
    // sent: shown as written
    ("chat-translated-sent", |_, _, _| {}),
    // the translated chat opened again in Teams Dark: its translations read back from the store at once
    ("chat-translated-dark", |shell, window, cx| {
        show_chat_in_theme(shell, teams_ui::theme::DARK_KEY, window, cx);
    }),
    // the tool signed out: the chat says why with Try again, and a send keeps its message with Try again and Send
    // without translating
    ("chat-translated-failed", |shell, window, cx| {
        show_chat_in_theme(shell, teams_ui::theme::LIGHT_KEY, window, cx);
        chat_tools(SIGNED_OUT_CLAUDE, cx);
        translate_chat(shell, Some("fr"), cx);
        type_in_compose(shell, "Bonjour à tous", window, cx);
        with_compose(shell, cx, |compose, cx| compose.send(window, cx));
    }),
    // read as written again
    ("chat-translated-off", |shell, window, cx| {
        translate_chat(shell, None, cx);
        translate_another_chat(shell, None, cx);
        cx.set_global(teams_ui::local_ai::Tools::default());
        clear_compose(shell, window, cx);
    }),
    // a draft being corrected by the AI: the line over the box, Correct lit in the toolbar
    ("compose-correcting", |shell, window, cx| {
        chat_tools(CORRECTING_CLAUDE, cx);
        type_in_compose(shell, "Bitte langsam lesen", window, cx);
        with_compose(shell, cx, |compose, cx| compose.correct_draft(window, cx));
    }),
    // the proposal: what it takes out struck through, what it puts in highlighted, and what to do with it
    ("compose-correction", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.end_correction(window, cx));
        type_in_compose(
            shell,
            "Ich habe gestern dem Bericht geschickt. Kannst du es bitte durchschauen, wenn du Zeit machst?",
            window,
            cx,
        );
        with_compose(shell, cx, |compose, cx| compose.correct_draft(window, cx));
    }),
    // the same, as the text would read
    ("compose-correction-result", |shell, _, cx| {
        with_compose(shell, cx, Compose::toggle_correction_view);
    }),
    // what changed again, in Teams Dark (the theme alone, as the draft stays)
    ("compose-correction-dark", |shell, _, cx| {
        with_compose(shell, cx, Compose::toggle_correction_view);
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // nothing to correct
    ("compose-correction-clean", |shell, window, cx| {
        show_chat_in_theme(shell, teams_ui::theme::LIGHT_KEY, window, cx);
        with_compose(shell, cx, |compose, cx| compose.end_correction(window, cx));
        type_in_compose(shell, "Alles in Ordnung", window, cx);
        with_compose(shell, cx, |compose, cx| compose.correct_draft(window, cx));
    }),
    // the tool signed out: why, with Try again, the draft as it was
    ("compose-correction-failed", |shell, window, cx| {
        chat_tools(SIGNED_OUT_CLAUDE, cx);
        with_compose(shell, cx, |compose, cx| compose.end_correction(window, cx));
        with_compose(shell, cx, |compose, cx| compose.correct_draft(window, cx));
    }),
    ("compose-correction-over", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.end_correction(window, cx));
        cx.set_global(teams_ui::local_ai::Tools::default());
        clear_compose(shell, window, cx);
    }),
    // text selected with the mouse from the second-to-last message into the last, on the selection colour, then its
    // right-click menu
    ("chat-selection", |shell, _, cx| select_across_last_two(shell, cx)),
    ("chat-selection-menu", |shell, window, cx| {
        if let Some(chat) = shell.read(cx).chat() {
            let size = window.viewport_size();
            let at = gpui::point(size.width * 0.5, size.height * 0.5);
            chat.read(cx).text_selection().open_menu(at);
            window.refresh();
        }
    }),
    // a team's "..." menu, then the "Create a channel" dialog through its states (the mock's team lets a make channels;
    // the mock does not implement team creation, so its failure is shown)
    ("channel-management-edit", |shell, window, cx| {
        if let Some(chat) = shell.read(cx).chat() {
            chat.read(cx).text_selection().close_menu();
        }
        let team = shell
            .read(cx)
            .teams()
            .read(cx)
            .teams()
            .and_then(|teams| teams.iter().find(|team| team.id == TEAM))
            .cloned();
        if let Some(team) = team
            && let Some(channel) = team.channels.first().cloned()
        {
            shell.update(cx, |shell, cx| {
                shell.channel_chosen(&team, &channel, teams_ui::channel_menu::ChannelChoice::Edit, window, cx);
            });
        }
    }),
    // the channel's "Channel notification settings", as Teams': the level's dropdown, replies, following
    ("channel-notifications", open_channel_notifications),
    ("channel-workflows", |shell, window, cx| {
        shell.update(cx, Shell::close_team_dialog);
        let team = shell
            .read(cx)
            .teams()
            .read(cx)
            .teams()
            .and_then(|teams| teams.iter().find(|team| team.id == TEAM))
            .cloned();
        if let Some(team) = team
            && let Some(channel) = team.channels.first()
        {
            shell.update(cx, |shell, cx| {
                shell.channel_chosen(
                    &team,
                    channel,
                    teams_ui::channel_menu::ChannelChoice::Workflows,
                    window,
                    cx,
                );
            });
        }
    }),
    ("team-menu", |shell, window, cx| {
        shell.update(cx, Shell::close_team_dialog);
        if let Some(chat) = shell.read(cx).chat() {
            let _ = chat.read(cx).text_selection().clear();
        }
        let sidebar = shell.read(cx).sidebar().clone();
        sidebar.update(cx, |sidebar, cx| sidebar.open_team_menu(TEAM, false, window, cx));
    }),
    ("channel-dialog", |shell, window, cx| {
        open_channel_dialog(shell, window, cx);
    }),
    // Tab from the name to the chosen layout: its card ringed
    ("channel-dialog-keyboard", |_, window, cx| {
        press(window, cx, "tab tab tab tab");
    }),
    ("channel-dialog-types", |shell, window, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| dialog.show_types(window, cx));
    }),
    ("channel-dialog-filled", |shell, window, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| fill_channel_dialog(dialog, window, cx));
    }),
    ("channel-dialog-problem", |shell, window, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| {
            dialog.set_name("Release #planning", window, cx);
        });
    }),
    ("channel-failed", |shell, window, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| {
            dialog.set_name("Release planning", window, cx);
            dialog.create(cx);
        });
    }),
    ("channel-people", |shell, _, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| {
            dialog.people_step(
                "OID:00000000-0000-4000-8000-0000000000bb@00000000-0000-4000-8000-0000000000cc".to_owned(),
                None,
                cx,
            );
        });
    }),
    ("channel-people-found", |shell, _, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| dialog.search_people("lab".to_owned(), cx));
    }),
    ("channel-closed", |shell, _, cx| {
        with_channel_dialog(shell, cx, teams_ui::channel_dialog::ChannelDialog::close_now);
    }),
    // "Your teams and channels", its Create team dropdown, a row's "..." menu, the dialogs and Manage team's tabs (the mock's
    // made-up team; the mock does not implement these writes)
    ("teams-page", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_teams_page(window, cx));
    }),
    ("sidebar-teams", |shell, _, cx| show_teams_section(shell, cx)),
    ("sidebar-team-menu", |shell, window, cx| {
        let sidebar = shell.read(cx).sidebar().clone();
        sidebar.update(cx, |sidebar, cx| sidebar.open_team_menu(TEAM, false, window, cx));
    }),
    // the same by keyboard: the team's line ringed, Shift+F10, the menu's first entry lit, Down to the next
    ("sidebar-team-menu-keyboard", |shell, window, cx| {
        let sidebar = shell.read(cx).sidebar().clone();
        sidebar.update(cx, |sidebar, cx| sidebar.close_team_menu(window, cx));
        sidebar.update(cx, |sidebar, cx| sidebar.focus_team(TEAM, window, cx));
        press(window, cx, "shift-f10 down");
    }),
    ("sidebar-team-menu-closed", |shell, window, cx| {
        let sidebar = shell.read(cx).sidebar().clone();
        sidebar.update(cx, |sidebar, cx| sidebar.close_team_menu(window, cx));
        show_chats_section(shell, cx);
    }),
    ("teams-page-create-menu", |shell, _, cx| {
        with_teams_page(shell, cx, teams_ui::teams_page::TeamsPage::open_create_menu);
    }),
    ("teams-page-row-menu", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.set_filter(teams_ui::teams_page::Filter::All, cx);
            page.open_menu(TEAM, false, window, cx);
        });
    }),
    // the rows by keyboard: the row ringed, its "..." shown
    ("teams-page-keyboard", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.close_menu(window, cx);
            page.focus_rows(window, cx);
        });
        press(window, cx, "down");
    }),
    ("teams-page-owned", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.close_menu(window, cx);
            page.set_filter(teams_ui::teams_page::Filter::Owned, cx);
        });
    }),
    ("teams-page-invites", |shell, _, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.set_filter(teams_ui::teams_page::Filter::All, cx);
            page.show_tab(teams_ui::teams_page::PageTab::PendingInvites, cx);
        });
    }),
    ("teams-page-analytics", |shell, _, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.show_tab(teams_ui::teams_page::PageTab::Analytics, cx);
        });
    }),
    ("create-team", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| {
            page.show_tab(teams_ui::teams_page::PageTab::Teams, cx);
        });
        shell.update(cx, |shell, cx| shell.open_create_team(window, cx));
    }),
    ("create-team-filled", |shell, window, cx| {
        with_team_dialog::<teams_ui::team_dialogs::CreateTeamDialog>(shell, cx, |dialog, cx| {
            dialog.fill(
                "Release planning",
                "Dates and owners for the next release",
                "Planning",
                window,
                cx,
            );
        });
    }),
    ("create-team-kind", |shell, _, cx| {
        with_team_dialog::<teams_ui::team_dialogs::CreateTeamDialog>(
            shell,
            cx,
            teams_ui::team_dialogs::CreateTeamDialog::show_kind,
        );
    }),
    ("create-team-failed", |shell, _, cx| {
        with_team_dialog::<teams_ui::team_dialogs::CreateTeamDialog>(shell, cx, |dialog, cx| {
            dialog.pick_kind(true, cx);
            dialog.create(cx);
        });
    }),
    ("add-members", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            let team = shell.teams().read(cx).teams().and_then(|teams| teams.first().cloned());
            if let Some(team) = team {
                shell.team_chosen(team, teams_ui::team_menu::TeamChoice::AddMember, window, cx);
            }
        });
    }),
    ("add-members-found", |shell, _, cx| {
        with_team_dialog::<teams_ui::team_dialogs::AddMembersDialog>(shell, cx, |dialog, cx| {
            dialog.search("lab".to_owned(), cx);
        });
    }),
    ("confirm-delete", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            let team = shell.teams().read(cx).teams().and_then(|teams| teams.first().cloned());
            if let Some(team) = team {
                shell.team_chosen(team, teams_ui::team_menu::TeamChoice::Delete, window, cx);
            }
        });
    }),
    ("confirm-archive", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            let team = shell.teams().read(cx).teams().and_then(|teams| teams.first().cloned());
            if let Some(team) = team {
                shell.team_chosen(team, teams_ui::team_menu::TeamChoice::Archive, window, cx);
            }
        });
    }),
    ("manage-channels", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_team_dialog(cx);
            shell.open_manage_team(TEAM.to_owned(), teams_ui::manage_team::ManageTab::Channels, window, cx);
        });
    }),
    ("manage-members", |shell, _, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Members, cx);
        });
    }),
    ("manage-settings", |shell, _, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Settings, cx);
        });
    }),
    ("manage-analytics", |shell, _, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Analytics, cx);
        });
    }),
    ("manage-tags", |shell, _, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Tags, cx);
        });
    }),
    // Analytics' "Select a channel", the Apps tab and an app's Remove asking, Get more apps, the Giphy rating, Change
    // picture (the mock's made-up apps; writes are not implemented by the mock)
    ("manage-analytics-channel", |shell, window, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Analytics, cx);
            page.toggle_channel_menu(window, cx);
        });
    }),
    ("manage-apps", |shell, window, cx| {
        with_manage_team(shell, cx, |page, cx| {
            page.toggle_channel_menu(window, cx);
            page.show_tab(teams_ui::manage_team::ManageTab::Apps, cx);
        });
    }),
    ("manage-apps-remove", |shell, _, cx| {
        with_manage_team(shell, cx, |page, cx| {
            if let Some(app) = page.apps_shown().into_iter().next() {
                page.ask_remove_app(app, cx);
            }
        });
    }),
    ("add-app", |shell, window, cx| {
        with_manage_team(shell, cx, teams_ui::manage_team::ManageTeam::keep_app);
        shell.update(cx, |shell, cx| {
            let team = shell.teams().read(cx).teams().and_then(|teams| teams.first().cloned());
            if let Some(team) = team {
                shell.open_add_app(team, window, cx);
            }
        });
    }),
    ("manage-settings-fun", |shell, window, cx| {
        shell.update(cx, Shell::close_team_dialog);
        with_manage_team(shell, cx, |page, cx| {
            page.show_tab(teams_ui::manage_team::ManageTab::Settings, cx);
            page.open_section("fun", cx);
            page.toggle_giphy_menu(window, cx);
        });
    }),
    ("team-picture", |shell, window, cx| {
        with_manage_team(shell, cx, |page, cx| page.toggle_giphy_menu(window, cx));
        shell.update(cx, |shell, cx| {
            let team = shell.teams().read(cx).teams().and_then(|teams| teams.first().cloned());
            if let Some(team) = team {
                shell.open_team_picture(team, window, cx);
            }
        });
    }),
    // "More create team options": the templates, one's details and its form, a copy of the team, no group
    ("create-more", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_team_dialog(cx);
            shell.open_create_team_more(window, cx);
        });
    }),
    ("create-more-template", |shell, _, cx| {
        with_create_more(shell, cx, |page, cx| {
            if let Some(card) = page.templates().first().cloned() {
                page.pick_template(card, cx);
            }
        });
    }),
    ("create-more-template-form", |shell, window, cx| {
        with_create_more(shell, cx, |page, cx| {
            page.use_template(window, cx);
            page.fill("Release planning", "", window, cx);
        });
    }),
    ("create-more-copy", |shell, window, cx| {
        with_create_more(shell, cx, |page, cx| {
            page.show(teams_ui::create_team_more::MoreSource::AnotherTeam, cx);
            let team = page.owned().first().cloned();
            if let Some(team) = team {
                page.pick_team(team, window, cx);
            }
        });
    }),
    ("create-more-group", |shell, _, cx| {
        with_create_more(shell, cx, |page, cx| {
            page.show(teams_ui::create_team_more::MoreSource::Group, cx);
        });
    }),
    ("join-team", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_join_team(window, cx));
    }),
    ("teams-page-back", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_teams_page(window, cx));
    }),
    // the title bar's search: its suggestions as "lab" is typed, its go-to mode, its results
    ("search-suggestions", |shell, window, cx| {
        if let Some(chat) = shell.read(cx).chat() {
            let _ = chat.read(cx).text_selection().clear();
        }
        shell.update(cx, |shell, cx| shell.focus_search(SearchMode::Search, window, cx));
        let search = shell.read(cx).search().clone();
        search.update(cx, |search, cx| search.set_query("lab", window, cx));
    }),
    ("search-goto", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.focus_search(SearchMode::GoTo, window, cx));
        let search = shell.read(cx).search().clone();
        search.update(cx, |search, cx| search.set_query("la", window, cx));
    }),
    ("search-results", |shell, window, cx| {
        let search = shell.read(cx).search().clone();
        search.update(cx, |search, cx| {
            search.escape(window, cx);
            search.set_query("lab", window, cx);
            search.show_results(Tab::All, cx);
        });
    }),
    ("account-menu", |shell, window, cx| {
        if let Some(chat) = shell.read(cx).chat() {
            let _ = chat.read(cx).text_selection().clear();
            window.refresh();
        }
        shell.update(cx, Shell::toggle_status_menu);
    }),
    ("settings-general", |shell, _, cx| {
        shell.update(cx, Shell::toggle_status_menu);
        show_page(shell, Page::General, cx);
    }),
    ("settings-appearance", |shell, _, cx| {
        show_page(shell, Page::Appearance, cx);
    }),
    ("settings-notifications", |shell, _, cx| {
        show_page(shell, Page::Notifications, cx);
    }),
    // the calls' sounds, scrolled to, each with its preview: a speaker that opens nothing is enough to offer them
    ("settings-sounds", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        teams_ui::settings::attach_sounds(
            &model,
            Arc::new(teams_ui::sounds::SoundBoard::new(Arc::new(NoDevices))),
            cx,
        );
        shell.read(cx).settings_view().read(cx).scroll_to_end();
    }),
    ("settings-privacy", |shell, _, cx| show_page(shell, Page::Privacy, cx)),
    ("settings-calls", |shell, _, cx| {
        // the mock's engine has no calls: made-up devices give the settings their Calls page, and calls shown with the
        // calling endpoint registered its "Make a test call"; made-up AI tools and models for its AI pickers
        made_up_ai_tools(cx);
        let model = shell.read(cx).settings().clone();
        teams_ui::settings::attach_devices(&model, Arc::new(FixedDirectory::new(made_up_devices())), cx);
        let models = shell.read(cx).models().clone();
        models
            .calls
            .update(cx, |calls, _| *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO))));
        models.apply(
            teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
            cx,
        );
        show_page(shell, Page::Calls, cx);
    }),
    // the Calls page's video effects, scrolled to: a camera to preview (the test pattern), one of the app's pictures
    // chosen, the camera off with Preview
    ("settings-video-effects", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        let camera: Arc<dyn teams_media::devices::Devices + Send + Sync> = match PictureCamera::sample() {
            Some(picture) => Arc::new(picture),
            None => Arc::new(teams_media::devices::file::FileDevices {
                input: teams_media::devices::file::AudioInput::Silence,
                recording: None,
                camera: teams_media::devices::file::PatternCamera::default(),
                pacing: teams_media::devices::file::Pacing::RealTime,
            }),
        };
        teams_ui::settings::attach_camera_preview(&model, teams_ui::CameraPreview::new(camera), cx);
        model.update(cx, |model, cx| {
            model.change(|settings| settings.video_effect = VideoEffect::Stock("office"), cx);
        });
        show_page(shell, Page::Calls, cx);
        shell.read(cx).settings_view().read(cx).scroll_to_end();
    }),
    // Blur chosen and the preview turned on (the user's Preview): the test pattern, blurred
    ("settings-video-effects-preview", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.video_effect = VideoEffect::Blur, cx);
        });
        let view = shell.read(cx).settings_view().clone();
        if let Some(picker) = view.read(cx).effects_picker().cloned() {
            picker.update(cx, teams_ui::EffectsPicker::start_preview);
        }
        view.read(cx).scroll_to_end();
    }),
    // the Calls page's local AI tools, scrolled to: captions translated by a model Claude Code lists, recaps
    // summarized by a model typed that Codex doesn't list (its name's field shown)
    ("settings-ai-tools", |shell, _, cx| show_ai_tools(shell, cx)),
    ("settings-diagnostics", |shell, _, cx| {
        // the calls shown for the Calls page go again: the mock's engine has none
        let calls = shell.read(cx).models().calls.clone();
        calls.update(cx, |calls, _| *calls = teams_ui::Calls::default());
        show_page(shell, Page::Diagnostics, cx);
    }),
    ("settings-appearance-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.follow_system = false;
                    settings.appearance.theme = teams_ui::theme::DARK_KEY;
                },
                cx,
            );
        });
        show_page(shell, Page::Appearance, cx);
    }),
    // the local AI tools in Teams Dark
    ("settings-ai-tools-dark", |shell, _, cx| show_ai_tools(shell, cx)),
    ("channel-dialog-dark", |shell, window, cx| {
        open_channel_dialog(shell, window, cx);
        with_channel_dialog(shell, cx, |dialog, cx| fill_channel_dialog(dialog, window, cx));
    }),
    ("channel-dialog-dark-types", |shell, window, cx| {
        with_channel_dialog(shell, cx, |dialog, cx| dialog.show_types(window, cx));
    }),
    ("channel-dialog-dark-closed", |shell, _, cx| {
        with_channel_dialog(shell, cx, teams_ui::channel_dialog::ChannelDialog::close_now);
    }),
    ("channel-notifications-dark", open_channel_notifications),
    ("teams-page-dark", |shell, window, cx| {
        shell.update(cx, Shell::close_team_dialog);
        shell.update(cx, |shell, cx| shell.open_teams_page(window, cx));
    }),
    ("teams-page-dark-row-menu", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| page.open_menu(TEAM, false, window, cx));
    }),
    ("manage-settings-dark", |shell, window, cx| {
        with_teams_page(shell, cx, |page, cx| page.close_menu(window, cx));
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(TEAM.to_owned(), teams_ui::manage_team::ManageTab::Settings, window, cx);
        });
    }),
    ("sidebar-teams-dark", |shell, _, cx| show_teams_section(shell, cx)),
    ("create-team-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_create_team(window, cx));
    }),
    ("create-team-dark-closed", |shell, _, cx| {
        with_team_dialog::<teams_ui::team_dialogs::CreateTeamDialog>(
            shell,
            cx,
            teams_ui::team_dialogs::CreateTeamDialog::cancel,
        );
    }),
    ("chat-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.show_chats(window, cx));
        select_last_message(shell, window, cx);
    }),
    ("update-available-dark", |_, _, cx| {
        update_shown(available(), cx);
    }),
    ("update-downloading-dark", |_, _, cx| update_shown(downloading(), cx)),
    ("update-failed-dark", |_, _, cx| update_shown(update_failed(), cx)),
    ("chat-dark-compose", |shell, window, cx| {
        update_shown(UpdateStatus::None, cx);
        type_in_compose(shell, "Lunch at noon?\nThe usual place, or the new one?", window, cx);
    }),
    ("chat-dark-compose-expanded", |shell, window, cx| {
        expand_with_formats(shell, window, cx);
    }),
    ("chat-dark-emoji", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.toggle_emoji(window, cx));
    }),
    // b's pasted picture with an emoticon in its text, b's wave alone, c's GIF from the GIF picker, b's link with its
    // preview card and b's phone screenshot alone in its square frame (`seed_pictures`), in Teams Dark
    ("chat-pictures", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(ABC_CHAT.to_owned(), window, cx));
    }),
    // the same once its pictures were read and decoded: the pasted picture, and c's GIF on one of its frames
    ("chat-pictures-read", |_, _, _| {}),
    // scrolled to its top: b's lines under the pasted picture, emoticons inside them at the text's size, on its line
    ("chat-pictures-emoticons", |shell, _, cx| show_first_message(shell, cx)),
    // the same in a larger text size: the emoticons grow with the text and stay on its baseline
    ("chat-pictures-emoticons-18", |shell, _, cx| {
        set_font_size(shell, 18, cx);
        show_first_message(shell, cx);
    }),
    // b's phone screenshot, the chat's third picture, opened in the picture viewer's window as a click on it opens it:
    // fitted to the window, b's avatar, name and the time in its header, "3 of 3", the zoom, Download and Close, and
    // Previous over the stage's left side, in Teams Dark (the text size back to the default)
    ("picture-viewer-dark", |shell, _, cx| {
        set_font_size(shell, teams_ui::theme::FONT_SIZE, cx);
        if let Some(chat) = shell.read(cx).chat().cloned() {
            chat.update(cx, |chat, cx| chat.open_nth_picture(2, cx));
        }
    }),
    // zoomed in three steps from the fit, past the window's height
    ("picture-viewer-zoomed-dark", |_, _, cx| {
        let (Some(viewer), Some(handle)) = (picture_viewer::shown(cx), picture_viewer::window(cx)) else {
            return;
        };
        let _ = handle.update(cx, |_, window, cx| {
            viewer.update(cx, |viewer, cx| {
                for _ in 0..3 {
                    viewer.zoom_step(true, window, cx);
                }
            });
        });
    }),
    // c's message with two videos and a document (`seed_files`): its lines, then the files' cards, the videos' with
    // their thumbnails as Graph and the media host (the mock) gave them, in Teams Dark
    ("chat-files-dark", |shell, window, cx| {
        picture_viewer::close(cx);
        shell.update(cx, |shell, cx| shell.open_chat(AC_CHAT.to_owned(), window, cx));
    }),
    // the first video played in its card, its bytes from the mock's download link (`seed_files`): loading, then
    // playing (its controls hidden while the pointer is elsewhere), then paused (or played to its end), its frame and
    // its controls with the seek bar part way
    ("chat-video-loading-dark", |_, _, cx| toggle_first_video(cx)),
    ("chat-video-playing-dark", |_, _, _| {}),
    ("chat-video-paused-dark", |_, _, cx| toggle_first_video(cx)),
    // x's message of text, two small pictures and a block of code (`seed_mixed`), in Teams Dark: the a–x chat opened
    // (at its newest, the forwards), then scrolled to its first message, each picture at its own size in the flow and
    // the code in Teams' code card
    ("chat-mixed-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AX_CHAT.to_owned(), window, cx));
    }),
    ("chat-mixed-dark-read", |shell, _, cx| show_first_message(shell, cx)),
    // See more in Teams Dark, as the Teams it is drawn after
    ("chats-see-more-dark", |shell, _, cx| reveal_see_more(shell, cx)),
    // New message (Ctrl+N), in Teams Dark: its row heading the chats, the To: line's suggestions with nothing typed
    // (the people of the latest 1:1 chats), "You're starting a new conversation" over its box
    ("new-chat-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_new_chat(window, cx));
        show_chats_section(shell, cx);
    }),
    // "lab" typed: the people it finds, then the group chats
    ("new-chat-suggestions-dark", |shell, window, cx| {
        with_new_chat(shell, cx, |view, cx| view.set_query("lab", window, cx));
    }),
    // b and c chosen, their chips on the line: the a–b–c chat they already have, under it with its own box
    ("new-chat-found-dark", |shell, window, cx| {
        with_new_chat(shell, cx, |view, cx| {
            view.set_query("", window, cx);
            for mri in [LAB_B, LAB_C] {
                let at = view
                    .suggestions()
                    .iter()
                    .position(|suggested| matches!(suggested, Suggested::Person { mri: person, .. } if person == mri));
                if let Some(at) = at {
                    view.choose(at, window, cx);
                }
            }
        });
    }),
    ("new-chat-found", |shell, _, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
    }),
    // a group name typed: "Group name:" over To:, and a new group chat again
    ("new-chat-named", |shell, window, cx| {
        with_new_chat(shell, cx, |view, cx| view.set_group_name("Lab trio", window, cx));
    }),
    ("new-chat-named-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
    }),
    // the a–b chat's people from its header (Teams Dark): "People", the account and b, "Start a group chat"
    ("chat-people-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
        clear_compose(shell, window, cx);
        shell.update(cx, |shell, cx| shell.toggle_people(window, cx));
    }),
    // its picker, "lab" typed: the people found, then the group chats
    ("chat-people-picker-dark", |shell, window, cx| {
        with_people(shell, cx, |people, cx| {
            people.open_picker(window, cx);
            people.set_query("lab", window, cx);
        });
    }),
    // c taken as a chip, Create lit (Teams Light)
    ("chat-people-picker", |shell, window, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
        with_people(shell, cx, |people, cx| {
            let at = people
                .suggestions()
                .iter()
                .position(|picked| matches!(picked, Picked::Person(person) if person.mri == LAB_C));
            if let Some(at) = at {
                people.take(at, window, cx);
            }
        });
    }),
    // no group chat of those people: a new one not made yet, its row and header named after them
    ("chat-people-draft", |shell, window, cx| {
        let named = |mri: &str, cx: &App| {
            shell
                .read(cx)
                .list()
                .read(cx)
                .all_rows()
                .find(|row| row.partner.as_deref() == Some(mri))
                .map_or_else(|| "Lab".into(), |row| row.name.clone())
        };
        let people = vec![
            Recipient {
                mri: OUTSIDER.to_owned(),
                name: "Synthetic Guest".into(),
                picture: None,
            },
            Recipient {
                mri: LAB_B.to_owned(),
                name: named(LAB_B, cx),
                picture: None,
            },
        ];
        shell.update(cx, |shell, cx| shell.open_group_draft(people, window, cx));
        show_chats_section(shell, cx);
    }),
    ("chat-people-draft-flyout", |shell, window, cx| {
        with_new_chat(shell, cx, |view, cx| view.toggle_people(window, cx));
    }),
    // the a–b–c chat's people, counted on the button, and its "Add" with the history choices
    ("chat-people-group", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(ABC_CHAT.to_owned(), window, cx);
            shell.toggle_people(window, cx);
        });
        with_people(shell, cx, |people, cx| {
            people.open_picker(window, cx);
            people.choose_history(HistoryChoice::Days, cx);
        });
    }),
    ("chat-people-group-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
    }),
    // the a–b chat in Teams Dark: the frame, the sheet, its own bubbles' tint and the reply's quote card
    ("chat-ab-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.show_chats(window, cx);
            shell.open_chat(AB_CHAT.to_owned(), window, cx);
        });
        clear_compose(shell, window, cx);
        show_chats_section(shell, cx);
    }),
    // a click on the reply's quote: the chat at the quoted message, ringed for a moment
    ("chat-quote-jump-dark", |shell, _, cx| go_to_quote(shell, cx)),
    // the account menu over it, in Teams Dark
    ("account-menu-dark", |shell, _, cx| {
        shell.update(cx, Shell::toggle_status_menu);
    }),
    // the settings' General and Notifications pages in Teams Dark
    ("settings-general-dark", |shell, _, cx| {
        shell.update(cx, Shell::toggle_status_menu);
        show_page(shell, Page::General, cx);
    }),
    ("settings-notifications-dark", |shell, _, cx| {
        show_page(shell, Page::Notifications, cx);
    }),
    // the a–b chat in each other dark theme: each its own frame and accent
    ("chat-nord", |shell, window, cx| {
        show_chat_in_theme(shell, "nord", window, cx);
    }),
    ("chat-dracula", |shell, window, cx| {
        show_chat_in_theme(shell, "dracula", window, cx);
    }),
    ("chat-solarized-dark", |shell, window, cx| {
        show_chat_in_theme(shell, "solarized-dark", window, cx);
    }),
    ("chat-high-contrast", |shell, window, cx| {
        show_chat_in_theme(shell, "high-contrast", window, cx);
    }),
    // the 12-hour clock chosen, back in Teams Light: the setting, then the a–b chat's times
    ("settings-time-12-hour", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.theme = teams_ui::theme::LIGHT_KEY;
                    settings.time_format = teams_ui::settings::TimeFormat::Twelve;
                },
                cx,
            );
        });
        show_page(shell, Page::General, cx);
    }),
    ("chat-12-hour", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.show_chats(window, cx);
            shell.open_chat(AB_CHAT.to_owned(), window, cx);
        });
    }),
    // a click on the reply's quote in Teams Light: the chat at the quoted message, ringed for a moment
    ("chat-quote-jump", |shell, _, cx| go_to_quote(shell, cx)),
    // the files' cards again, in Teams Light
    ("chat-files", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AC_CHAT.to_owned(), window, cx));
    }),
    // the a–b–c chat: its senders' presence as the view asked the engine and the mock's UPS answered (`seed_group`),
    // b's and c's badges on their avatars
    ("chat-group-presence", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(ABC_CHAT.to_owned(), window, cx));
    }),
    // `@` typed in the a–b–c chat: the mention picker over the box, the chat's members, then Everyone; above it b's
    // mention of everyone (`seed_group`), the account's own
    ("chat-mention-everyone", |shell, window, cx| {
        type_in_compose(shell, "@", window, cx);
    }),
    // the "Chats" section's first rows and See more under them, bold for the unread chat below it (`seed_many_chats`)
    ("chats-see-more", |shell, _, cx| reveal_see_more(shell, cx)),
    // See more clicked: the next rows where it was, the list not scrolled
    ("chats-seen-more", |shell, _, cx| {
        let list = shell.read(cx).list().clone();
        list.update(cx, teams_ui::ChatList::see_more);
    }),
    // See more again under them, no longer bold
    ("chats-see-more-again", |shell, _, cx| reveal_see_more(shell, cx)),
    // the oldest chat, opened from below the rows shown: listed after them, above See more, highlighted
    ("chats-opened-from-below", |shell, window, cx| {
        let oldest = shell.read(cx).list().read(cx).rows().last().map(|row| row.id.clone());
        if let Some(oldest) = oldest {
            shell.update(cx, |shell, cx| shell.open_chat(oldest, window, cx));
        }
    }),
    // the team's channel the mock's CSA lists as a's favourite, under "Favorites" with its team's mark, clicked there:
    // opened as a channel, its row highlighted: its posts, a post's replies summed up under it, "Message in <channel>"
    ("favorite-channel", |shell, _, cx| {
        let list = shell.read(cx).list().clone();
        list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
    }),
    // a post with replies under the pointer: Teams' bar, the quick reactions, react, reply in thread and more options
    ("channel-post-hover", |shell, _, cx| {
        if let Some(posts) = channel_posts(shell, cx) {
            posts.update(cx, |posts, cx| posts.hover(Some(CHANNEL_POST.to_owned()), cx));
        }
    }),
    // its "Reply in thread": the thread beside the posts, the post and its replies over "Reply in thread"
    ("channel-thread", |shell, window, cx| {
        if let Some(posts) = channel_posts(shell, cx) {
            posts.update(cx, |posts, cx| posts.hover(None, cx));
        }
        if let Some(channel) = shell.read(cx).channel().cloned() {
            channel.update(cx, |channel, cx| {
                channel.open_thread(CHANNEL_POST.to_owned(), window, cx);
            });
        }
    }),
    ("favorite-channel-menu", |shell, window, cx| {
        if let Some(channel) = shell.read(cx).channel().cloned() {
            channel.update(cx, teams_ui::ChannelView::close_thread);
        }
        let at = shell
            .read(cx)
            .list()
            .read(cx)
            .favorites()
            .iter()
            .position(|row| row.id == TEAM_CHANNEL);
        if let Some(at) = at {
            let sidebar = shell.read(cx).sidebar().clone();
            sidebar.update(cx, |sidebar, cx| {
                if !sidebar.is_open(teams_ui::sidebar::Section::Favorites) {
                    sidebar.toggle(teams_ui::sidebar::Section::Favorites, cx);
                }
                sidebar.focus_list(window, cx);
            });
            press(window, cx, "home");
            for _ in 0..=at {
                press(window, cx, "down");
            }
            press(window, cx, "shift-f10");
        }
    }),
    // calls registered (the mock's engine has none: shown as the engine would tell them): the channel's header has
    // Teams' Meet now, its caret beside it, since no meeting runs in it
    ("channel-meet-now", |shell, window, cx| {
        press(window, cx, "escape");
        let models = shell.read(cx).models().clone();
        models
            .calls
            .update(cx, |calls, _| *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO))));
        models.apply(
            teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
            cx,
        );
    }),
    // Meet now clicked: the new meeting's pre-join, titled after the channel, nothing sent
    ("channel-meet-now-prejoin", |shell, _, cx| {
        shell.update(cx, Shell::meet_now);
    }),
    // the Meet now menu's Schedule a meeting: the scheduling form in its own window, the channel's row ("<team> >
    // <channel>" and Send personal invites) in the Teams meeting switch's place
    ("channel-schedule-form", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.calls.update(cx, teams_ui::Calls::cancel_prejoin);
        shell.update(cx, Shell::schedule_channel_meeting);
    }),
    // a channel meeting's card's Edit (its organizer's): the scheduling form on the meeting read from the team's
    // calendar, the channel's row, Send update and Cancel meeting
    ("channel-meeting-edit-form", |shell, _, cx| {
        event_form::close(cx);
        let place = channel_meeting_place(TEAM);
        let calendar = shell.read(cx).calendar_view().clone();
        match channel_meeting(&place.channel.mailbox) {
            Some(full) => calendar.update(cx, |calendar, cx| calendar.edit_in_channel(full, place, cx)),
            None => say("the made-up channel meeting doesn't read"),
        }
    }),
    // the general channel, where the mock's CSA names a meeting running: its header's Join
    ("channel-join", |shell, window, cx| {
        event_form::close(cx);
        let models = shell.read(cx).models().clone();
        models.calls.update(cx, teams_ui::Calls::cancel_prejoin);
        shell.update(cx, |shell, cx| shell.open_channel_id(TEAM.to_owned(), window, cx));
    }),
    // the rail's Calendar: the work week of today, the mock's made-up week of meetings in it (recurring, cancelled,
    // overlapping, tentative, all day), the current time's line
    ("calendar", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.show_calendar(window, cx));
    }),
    // a meeting's card, opened by a click on its block: the split Join and Chat with participants
    ("calendar-card", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.calls.update(cx, |calls, _| {
            *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO)));
        });
        let calendar = shell.read(cx).calendar_view().clone();
        let meeting = calendar.read(cx).events().and_then(|events| {
            events
                .iter()
                .find(|shown| shown.event.online && !shown.event.cancelled)
                .cloned()
        });
        if let Some(meeting) = meeting {
            calendar.update(cx, |calendar, cx| calendar.open_card(meeting, cx));
        } else {
            say("no meeting in the calendar");
        }
    }),
    // Join clicked: the meeting's pre-join in its own window, its camera off, nothing sent
    ("calendar-prejoin", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        let shown = calendar.read(cx).card().cloned();
        let to_join = shown.and_then(|shown| {
            Some(teams_ui::calls::MeetingToJoin::Chat {
                chat: shown.event.meeting_chat.clone()?,
                meeting: shown.event.meeting.clone()?,
                title: shown.title(),
            })
        });
        if let Some(to_join) = to_join {
            calendar.update(cx, teams_ui::calendar_view::CalendarView::close_card);
            shell.update(cx, |shell, cx| shell.open_prejoin(to_join, cx));
        } else {
            say("the meeting's card names no meeting to join");
        }
    }),
    // and in Teams Dark, its microphone switched off
    ("calendar-prejoin-dark", |shell, _, cx| {
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
        let models = shell.read(cx).models().clone();
        models.calls.update(cx, |calls, cx| calls.set_prejoin_muted(true, cx));
    }),
    // Don't use audio picked: the computer's devices give way to its note
    ("calendar-prejoin-no-audio", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models
            .calls
            .update(cx, |calls, cx| calls.set_prejoin_no_audio(true, cx));
    }),
    ("calendar-month", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.calls.update(cx, teams_ui::Calls::cancel_prejoin);
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.close_card(cx);
            calendar.set_mode(teams_ui::calendar::Mode::Month, cx);
        });
    }),
    ("calendar-day", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| calendar.set_mode(teams_ui::calendar::Mode::Day, cx));
    }),
    // the work week in Teams Dark, as the user's screenshot shows it
    ("calendar-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.set_mode(teams_ui::calendar::Mode::WorkWeek, cx);
        });
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // the account's own meeting's card in Teams Dark: whom it invites and how each answered, Edit and Cancel occurrence
    ("calendar-card-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        let own = calendar.read(cx).events().and_then(|events| {
            events
                .iter()
                .find(|shown| shown.event.online && !shown.event.cancelled && shown.event.is_organizer)
                .cloned()
        });
        match own {
            Some(own) => calendar.update(cx, |calendar, cx| calendar.open_card(own, cx)),
            None => say("no meeting of the account's in the calendar"),
        }
    }),
    // two hours of tomorrow (or Monday) selected by a drag, as New would schedule them
    ("calendar-selection-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.close_card(cx);
            let span = calendar.span();
            let date = span.dates().nth(1).unwrap_or(span.first);
            let selection = teams_ui::calendar_view::Selection::at(date, 22).to(25);
            calendar.select(selection, cx);
        });
    }),
    // the Filter with series hidden: "Filter applied", only the single events left
    ("calendar-filtered-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.flip_shown(teams_ui::calendar_view::Shows::Recurring, cx);
        });
    }),
    // the toolbar's Join with an ID: its panel under the toolbar
    ("calendar-join-id", |shell, window, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.clear_filters(cx);
            calendar.open_join_id(window, cx);
        });
    }),
    // the toolbar's Meet now: "Start a meeting now" with the meeting's name
    ("calendar-meet-now-dark", |shell, window, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.close_join_id(cx);
            calendar.open_meet_now(window, cx);
        });
    }),
    // its Get a link to share: the meeting made (the mock's), its short link with Copy, Start meeting and Share via
    // email
    ("calendar-meet-now-link-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| calendar.meet(false, cx));
    }),
    // Add calendar: its dialog on Add from directory, the box's suggestions open over the disabled Add
    ("calendar-add-calendar", |shell, window, cx| {
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.close_meet_now(cx);
            calendar.open_add_calendar(window, cx);
            calendar.search_directory("avery@example.com", window, cx);
        });
    }),
    // a person picked: their chip in the box's place, Add enabled
    ("calendar-add-calendar-picked", |shell, window, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| calendar.pick_person(tour_person(), window, cx));
    }),
    // the person added: "People's calendars" in the pane, their busy times (the mock's) beside the account's events
    ("calendar-people", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, teams_ui::calendar_view::CalendarView::add_picked);
    }),
    // Split view over three days: the account's calendar and theirs in columns of their own
    ("calendar-split", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.set_days(3, cx);
            calendar.set_split(true, cx);
        });
    }),
    // a meeting's card opened by a click on it: beside the event, as Teams' peek opens, in Teams Dark
    ("calendar-peek-dark", |shell, _, cx| {
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.set_split(false, cx);
            calendar.set_mode(teams_ui::calendar::Mode::WorkWeek, cx);
            let meeting = calendar.events().and_then(|events| {
                events
                    .iter()
                    .find(|shown| shown.event.online && !shown.event.cancelled && !shown.all_day())
                    .cloned()
            });
            match meeting {
                Some(meeting) => {
                    calendar.open_card_at(meeting, gpui::point(gpui::px(520.0), gpui::px(380.0)), cx);
                }
                None => say("no meeting in the calendar"),
            }
        });
    }),
    // the calendar's New: the scheduling form in a window of its own ("New event - Calendar"), at the selection,
    // the day beside it with the new event's block, in Teams Dark as the user's screenshot has it
    ("event-form-dark", |shell, _, cx| {
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            calendar.close_card(cx);
            calendar.close_join_id(cx);
            let slot = calendar.next_slot();
            calendar.new_event(slot, false, cx);
        });
    }),
    // filled in: a title, b invited and c as optional, a Teams meeting, a line of text
    ("event-form-filled-dark", |_, _, cx| {
        let (Some(form), Some(handle)) = (event_form::shown(cx), event_form::window(cx)) else {
            say("no scheduling form");
            return;
        };
        let _ = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.fill_in(
                    "Planning",
                    &[
                        ("bot-b@lab.example.com", "Bot B", true),
                        ("bot-c@lab.example.com", "Bot C", false),
                    ],
                    "Agenda: the next milestone.",
                    window,
                    cx,
                );
            });
        });
    }),
    // Series: the event repeats weekly on its day, the repeat row under its time with the end and Teams' summary
    ("event-form-series-dark", |_, _, cx| {
        with_event_form(cx, |form, window, cx| form.choose_series(true, window, cx));
    }),
    // the repeat drop-down's Custom: the custom recurrence dialog over the form
    ("event-form-custom-repeat-dark", |_, _, cx| {
        with_event_form(cx, teams_ui::event_form::EventForm::open_custom);
    }),
    // the Scheduler in the day's place (b's and c's made-up busy times, the suggested times), and Format: Teams'
    // format bar over the text
    ("event-form-scheduler-dark", |_, _, cx| {
        with_event_form(cx, |form, window, cx| {
            form.close_custom(cx);
            form.show_side(event_form::Side::Scheduler, cx);
            form.toggle_formatting(window, cx);
        });
    }),
    // the globe's time zones: the start in Honolulu on its own date there, the end in London
    ("event-form-zones-dark", |_, _, cx| {
        with_event_form(cx, |form, _, cx| {
            form.show_side(event_form::Side::Day, cx);
            form.show_zones(true, cx);
            if let Ok(honolulu) = "Pacific/Honolulu".parse::<event_form::zones::Tz>() {
                form.set_zone(false, honolulu, cx);
            }
            if let Ok(london) = "Europe/London".parse::<event_form::zones::Tz>() {
                form.set_zone(true, london, cx);
            }
        });
    }),
    // two messages b sent in the a–b–c chat, forwarded to a by the outsider (`seed_forwards`): each in Teams' card
    // under the arrow, b's avatar and name and the original's time, the second's picture drawn in it
    ("chat-forward", |shell, window, cx| {
        event_form::close(cx);
        shell.update(cx, |shell, cx| shell.open_chat(AX_CHAT.to_owned(), window, cx));
    }),
    // the right-click menu of the forwarded note's link, as a right-click on it opens it: Copy link and Open link; in
    // Teams Light, then in Teams Dark
    ("chat-link-menu", |shell, window, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        teams_ui::selection::open_link_menu(FORWARDED_LINK, window, cx);
    }),
    ("chat-link-menu-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    ("chat-forward-dark", |shell, window, cx| {
        teams_ui::selection::close_link_menu(cx);
        window.refresh();
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // x's message of text, pictures and code again, in Teams Light, above the forwards in the same chat
    ("chat-mixed", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        show_first_message(shell, cx);
    }),
    // the Workflows bot's card in the "Lab alerts" chat (`seed_cards`), as Teams draws it: the app's tile beside it,
    // the card on its own surface (columns with an icon, an emphasis container, facts, a picture, a separated footer
    // with a link) and its buttons; first in Teams Dark, then in Teams Light
    ("chat-card-dark", |shell, window, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
        if let Some(chat) = CARD_CHAT.get().cloned() {
            shell.update(cx, |shell, cx| shell.open_chat(chat, window, cx));
        }
    }),
    ("chat-card", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
    }),
    // a person's profile card (`seed_profile_cards`), in Teams Dark then Light: the compact card beside c's face in
    // the a–b–c chat, the full card's Overview, Profile and Organization, and the compact card beside b's channel post
    ("profile-card-compact-dark", |shell, window, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
        compact_card_in_group(shell, window, cx);
    }),
    ("profile-card-full-dark", |shell, window, cx| {
        full_card(shell, CardTab::Overview, window, cx);
    }),
    ("profile-card-full-profile-dark", |shell, window, cx| {
        card_tab(shell, CardTab::Profile, window, cx);
    }),
    ("profile-card-full-organization-dark", |shell, window, cx| {
        card_tab(shell, CardTab::Organization, window, cx);
    }),
    ("profile-card-channel-dark", compact_card_in_channel),
    ("profile-card-compact", |shell, window, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
        compact_card_in_group(shell, window, cx);
    }),
    ("profile-card-full", |shell, window, cx| {
        full_card(shell, CardTab::Overview, window, cx);
    }),
    ("profile-card-full-profile", |shell, window, cx| {
        card_tab(shell, CardTab::Profile, window, cx);
    }),
    ("profile-card-full-organization", |shell, window, cx| {
        card_tab(shell, CardTab::Organization, window, cx);
    }),
    ("profile-card-channel", compact_card_in_channel),
    // a call b made and a answered, as the engine would tell it (`fake_call`: the mock's engine has no calls): its
    // own window over the main one (niri gives it the screen), b speaking on its video, the controls in its header
    ("call-window", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.close_card(window, cx));
        fake_call(shell, cx);
    }),
    // its participants' pane open from People
    ("call-window-people", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_roster(true, cx));
    }),
    // its video effects' pane open from More: the camera off with Preview, the backgrounds, Blur chosen
    ("call-window-effects", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_effects(true, cx));
    }),
    // its Call health pane from More → Settings: made-up measures (the mock's engine runs no call media)
    ("call-window-health", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_health_reading(tour_health(), cx));
    }),
    // and its Video page, from View more video data
    ("call-window-health-video", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_health_section(2, cx));
    }),
    // the main window beside it, its chats usable, its in-call strip the way back: the parent puts the two side by
    // side and shoots the screen
    ("call-side-by-side", |_, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.show_health(false, cx);
            stage.show_effects(false, cx);
            stage.show_roster(false, cx);
        });
    }),
    // live captions translated into German on this computer (a fake `claude` answering for Claude Haiku): each line
    // its translation alone (the one still spoken too, once its text settled), the pane naming the tool
    ("call-window-captions", |shell, _, cx| translated_captions(shell, cx)),
    // in Teams Dark, the tool picked gone: the captions as spoken under a notice saying why, with Try again
    ("call-window-captions-failed-dark", |shell, _, cx| {
        cx.set_global(teams_ui::local_ai::Tools::default());
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.theme = teams_ui::theme::DARK_KEY;
                    settings.caption_translator = teams_ui::translate::AiChoice::of(
                        teams_ui::local_ai::ToolChoice::Only(teams_ui::translate::Backend::Codex),
                    );
                },
                cx,
            );
        });
    }),
    // the call window in Teams Dark (the captions hidden again, translating off)
    ("call-window-dark", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::CallCaptions {
                call: teams_core::CallId::for_tests(FAKE_CALL),
                captions: teams_core::CallCaptions::default(),
            },
            cx,
        );
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.theme = teams_ui::theme::DARK_KEY;
                    settings.caption_translate_to = None;
                    settings.caption_translator = teams_ui::translate::AiChoice::default();
                },
                cx,
            );
        });
    }),
    // and its video effects' pane in Teams Dark
    ("call-window-effects-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_effects(true, cx));
    }),
    // and its Call health pane in Teams Dark
    ("call-window-health-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_health_reading(tour_health(), cx));
    }),
    // and its Audio page in Teams Dark
    ("call-window-health-audio-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_health_section(1, cx));
    }),
    // the 1:1 call on hold in Teams Dark: Resume in Hold's place, the camera and the microphone greyed out, "On hold"
    ("call-window-held-dark", |shell, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.show_health(false, cx);
            stage.show_effects(false, cx);
        });
        held(shell, true, cx);
    }),
    // the same call held by its far end (Teams' held party): the far end's avatar over "On hold", no duration, the
    // account's own controls as they were
    ("call-window-held-by-far-end-dark", |shell, _, cx| {
        held(shell, false, cx);
        held_by_far_end(shell, true, cx);
    }),
    // and in Teams Light
    ("call-window-held-by-far-end", |shell, _, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
    }),
    // the account sharing its screen in Teams Dark: the red Stop sharing in Share's place
    ("call-window-sharing-dark", |shell, _, cx| {
        held_by_far_end(shell, false, cx);
        theme(shell, teams_ui::theme::DARK_KEY, cx);
        share(shell, (true, false), cx);
    }),
    // b sharing its screen: the share takes the stage, b's tile and the account's own in a strip on its right
    ("call-window-viewing-dark", |shell, _, cx| {
        share(shell, (false, true), cx);
    }),
    // View's Focus on content: the share alone
    ("call-window-focus-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.set_layout(teams_ui::call_stage::Layout::Focus, cx);
        });
    }),
    // the same in Teams Light
    ("call-window-viewing", |shell, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.set_layout(teams_ui::call_stage::Layout::Gallery, cx);
        });
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
    }),
    ("call-window-sharing", |shell, _, cx| {
        share(shell, (true, false), cx);
    }),
    // Transfer in the 1:1 call: it holds the call, "Transfer the call" with someone typed and found
    ("call-window-transfer", |shell, _, cx| {
        share(shell, (false, false), cx);
        held_by_far_end(shell, false, cx);
        held(shell, true, cx);
        with_stage_window(cx, |stage, window, cx| {
            stage.open_transfer(window, cx);
            stage.transfer_found(vec![tour_person()], cx);
        });
    }),
    // someone picked: "Transfer to an audio call" and Transfer
    ("call-window-transfer-picked", |_, _, cx| {
        with_stage_window(cx, |stage, window, cx| stage.pick_transfer(tour_person(), window, cx));
    }),
    // and in Teams Dark
    ("call-window-transfer-picked-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
    }),
    // People in the 1:1 call, someone typed in "Invite someone" and found
    ("call-window-invite", |shell, _, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
        held(shell, false, cx);
        with_stage_window(cx, |stage, window, cx| {
            stage.close_transfer(cx);
            stage.show_roster(true, cx);
            stage.found_to_invite("lab", vec![tour_person()], window, cx);
        });
    }),
    // they were added: the call is a group call in its new chat, its toolbar a group call's, and they didn't answer
    ("call-window-invited", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        let call = teams_core::CallId::for_tests(FAKE_CALL);
        let person = tour_person();
        models.calls.update(cx, |calls, cx| {
            calls.invited(call, teams_proto::Mri::new(&person.mri), person.name.clone(), cx);
        });
        models.apply(
            teams_core::Event::CallGroupChat {
                call,
                chat: "19:00000000000000000000000000010001@thread.v2".to_owned(),
            },
            cx,
        );
        models.apply(
            teams_core::Event::CallAddFailed {
                call,
                who: teams_proto::Mri::new(&person.mri),
            },
            cx,
        );
        with_stage_window(cx, |stage, window, cx| {
            stage.found_to_invite("", Vec::new(), window, cx);
        });
    }),
    // and in Teams Dark
    ("call-window-invited-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
    }),
    // someone waiting in a meeting's lobby, as whoever may admit sees them: "Waiting in the lobby", Deny and Admit
    ("call-lobby-dark", |shell, _, cx| {
        share(shell, (false, false), cx);
        with_stage(cx, |stage, cx| stage.show_roster(false, cx));
        theme(shell, teams_ui::theme::DARK_KEY, cx);
        lobby(
            shell,
            vec![("8:orgid:00000000-0000-4000-8000-00000000000c", "Lab Sea")],
            cx,
        );
    }),
    // the same in Teams Light
    ("call-lobby", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
    }),
    // the account itself waiting in a meeting's lobby: only that someone should let it in
    ("call-in-lobby", |shell, _, cx| {
        lobby(shell, Vec::new(), cx);
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::Call {
                call: teams_core::CallId::for_tests(FAKE_CALL),
                status: teams_core::CallStatus::Lobby,
            },
            cx,
        );
    }),
    // a group call in the a-b-c chat, answered: Chat, People with how many are in it, Raise and React; Lab Bee
    // speaking on its video, Lab Sea muted with its hand up, reactions floating up (its window opens as the call is
    // told: its panes open in the steps after)
    ("call-group", |shell, _, cx| fake_group_call(shell, cx)),
    // its View's Speaker: Lab Bee large over a strip of the others
    ("call-group-speaker", |_, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.show_roster(false, cx);
            stage.set_layout(teams_ui::call_stage::Layout::Speaker, cx);
        });
    }),
    // the call's chat beside the stage, back in the gallery
    ("call-group-chat", |_, _, cx| {
        with_stage_window(cx, |stage, window, cx| {
            stage.set_layout(teams_ui::call_stage::Layout::Gallery, cx);
            stage.show_chat(true, window, cx);
        });
    }),
    // in Teams Dark, the account's own hand up, the People pane open
    ("call-group-dark", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::CallOwn {
                call: teams_core::CallId::for_tests(GROUP_CALL),
                hand_raised: true,
                organizer: false,
            },
            cx,
        );
        react(shell, LAB_C, "applause", cx);
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
        with_stage(cx, |stage, cx| stage.show_roster(true, cx));
    }),
    // and its chat in Teams Dark
    ("call-group-chat-dark", |_, _, cx| {
        with_stage_window(cx, |stage, window, cx| stage.show_chat(true, window, cx));
    }),
    // back in Teams Light, the People pane with the hands up in their line, and End meeting's confirmation over it
    ("call-end-meeting-confirm", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        with_stage(cx, |stage, cx| {
            stage.show_roster(true, cx);
            stage.confirm_end_meeting(true, cx);
        });
    }),
    // the meeting's timer running over the stage, someone's recording's notice and the red dot by the duration
    ("call-group-timer", |shell, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.confirm_end_meeting(false, cx);
            stage.show_roster(false, cx);
        });
        meeting_state(shell, false, cx);
    }),
    // the timer's editor open from its time
    ("call-group-timer-edit", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.edit_timer(true, cx));
    }),
    // in Teams Dark, the meeting locked: its notice, and Lock the meeting's question over the window
    ("call-group-lock-dark", |shell, _, cx| {
        with_stage(cx, |stage, cx| stage.edit_timer(false, cx));
        meeting_state(shell, true, cx);
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
        with_stage(cx, |stage, cx| stage.confirm_locking(true, cx));
    }),
    // More's Meeting info in Teams Dark: how to join, as the invitation says
    ("call-group-meeting-info-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| {
            stage.confirm_locking(false, cx);
            stage.show_meeting_info(true, cx);
        });
    }),
    // and in Teams Light
    ("call-group-meeting-info", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
    }),
    // More → Record and transcribe → Start recording: "Start recording and transcription" asks the spoken language
    ("call-group-record", |shell, _, cx| {
        recording_state(shell, None, cx);
        with_stage(cx, |stage, cx| {
            stage.show_meeting_info(false, cx);
            stage.open_record_dialog(cx);
        });
    }),
    // and in Teams Dark
    ("call-group-record-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // the account's own recording: "Started by you", its privacy policy, and the red dot
    ("call-group-recording-by-you-dark", |shell, _, cx| {
        with_stage(cx, CallStage::close_record_dialog);
        recording_state(shell, Some(LAB_A), cx);
    }),
    // Stop recording asks first
    ("call-group-record-stop-dark", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.confirm_stop_recording(true, cx));
    }),
    // in Teams Light, it stopped, and a start the policy refused
    ("call-group-recording-stopped", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        with_stage(cx, |stage, cx| stage.confirm_stop_recording(false, cx));
        recording_state(shell, None, cx);
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::CallRecording {
                call: teams_core::CallId::for_tests(GROUP_CALL),
                recording: teams_core::CallRecording {
                    phase: teams_core::RecordingPhase::Idle,
                    problem: Some(teams_core::RecordingProblem {
                        heading: "Couldn't start recording",
                        detail: "Recording not started. Your IT admin has disabled Recording.",
                    }),
                },
            },
            cx,
        );
    }),
    // More → Record and transcribe → Start transcription: "What language is everyone speaking?"
    ("call-group-transcribe", |shell, _, cx| {
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::CallRecording {
                call: teams_core::CallId::for_tests(GROUP_CALL),
                recording: teams_core::CallRecording::default(),
            },
            cx,
        );
        with_stage(cx, CallStage::open_transcribe_dialog);
    }),
    // the account's transcription: its notice, the transcript's mark by the duration, and the Transcript pane open
    ("call-group-transcribing", |shell, _, cx| {
        with_stage(cx, CallStage::close_record_dialog);
        transcription_state(shell, false, cx);
    }),
    // and in Teams Dark
    ("call-group-transcribing-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // in Teams Light, Start recording while it transcribes asks what to record: Audio only picked
    ("call-group-record-audio-only", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        with_stage(cx, |stage, cx| {
            stage.open_record_dialog(cx);
            stage.choose_audio_only(true, cx);
        });
    }),
    // the audio alone records beside the transcription: "Audio recording and transcription" and the microphone's mark
    ("call-group-recording-audio", |shell, _, cx| {
        with_stage(cx, CallStage::close_record_dialog);
        transcription_state(shell, true, cx);
    }),
    // the account organizes the group call: People names it Organizer, with Mute all while the others are unmuted
    ("call-group-people-mute-all", |shell, _, cx| {
        group_controls(shell, (true, true), teams_core::MeetingState::default(), cx);
        with_stage(cx, |stage, cx| stage.show_roster(true, cx));
    }),
    // Mute all asks first: "Mute everyone?"
    ("call-group-mute-all-ask", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.confirm_mute_all(true, cx));
    }),
    // Lab Sea left: "People added to the chat (1)", its Request to join shown as the pointer is on the row
    ("call-group-people-added", |shell, _, cx| {
        with_stage(cx, |stage, cx| stage.confirm_mute_all(false, cx));
        group_controls(shell, (true, false), teams_core::MeetingState::default(), cx);
        with_stage(cx, |stage, cx| stage.hover_added(Some(0), cx));
    }),
    // and in Teams Dark
    ("call-group-people-added-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
    }),
    // in Teams Light, the organizer's Controls: Lock meeting, Enable lobby, Allow all participants to …
    ("call-group-controls", |shell, _, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
        group_controls(shell, (true, true), teams_core::MeetingState::default(), cx);
        with_stage(cx, |stage, cx| {
            stage.hover_added(None, cx);
            stage.show_roster(false, cx);
            stage.open_controls(true, cx);
        });
    }),
    // and in Teams Dark, with only the organizer presenting: the attendees' restrictions taken off greyness
    ("call-group-controls-dark", |shell, _, cx| {
        theme(shell, teams_ui::theme::DARK_KEY, cx);
        let presenting = teams_core::MeetingState {
            options: teams_proto::MeetingOptions {
                present: Some(false),
                chat: Some(false),
                unmute: Some(false),
                ..teams_proto::MeetingOptions::default()
            },
            ..teams_core::MeetingState::default()
        };
        group_controls(shell, (true, true), presenting, cx);
    }),
    // someone else organizes: the account, an attendee muted by them, kept from unmuting, its camera and presenting —
    // "You've been muted" over the stage, Mic, Camera and Share greyed (Teams Dark)
    ("call-group-attendee-dark", |shell, _, cx| {
        with_stage(cx, |stage, cx| stage.open_controls(false, cx));
        group_controls(shell, (false, true), attendee(), cx);
    }),
    // in Teams Light, muted anew by the organizer while attendees may unmute: the card's Unmute, the controls back
    ("call-group-muted", |shell, _, cx| {
        theme(shell, teams_ui::theme::LIGHT_KEY, cx);
        let muted = teams_core::MeetingState {
            muted_by_organizer: Some(6),
            ..teams_core::MeetingState::default()
        };
        group_controls(shell, (false, true), muted, cx);
    }),
];

/// The a–b–c chat with the compact card of the last of b's or c's messages drawn with a face open beside that face, as
/// a rest of the pointer on it opens it.
fn compact_card_in_group(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        shell.close_card(window, cx);
        shell.open_chat(ABC_CHAT.to_owned(), window, cx);
    });
    card_beside_face(shell.clone(), window, 10, |shell, cx| {
        let chat = shell.read(cx).chat()?.clone();
        let chat = chat.read(cx);
        chat.items().iter().rev().find_map(|item| match item {
            Item::Message(message) if message.header && !message.own => {
                let mri = message
                    .sender
                    .clone()
                    .filter(|mri| [LAB_B, LAB_C].contains(&mri.as_str()))?;
                let face = chat.face_bounds(&message.id)?;
                Some((CardOf::new(mri, Some(message.sender_name.clone())), face))
            }
            _ => None,
        })
    });
}

/// Whose card and which face, once the view drew it.
type FindFace = fn(&Entity<Shell>, &App) -> Option<(CardOf, gpui::Bounds<gpui::Pixels>)>;

/// Opens the compact card `find` names beside the face it names, once it was drawn: tried again on the next frames,
/// `tries` times, while the view hasn't drawn it yet.
fn card_beside_face(shell: Entity<Shell>, window: &mut Window, tries: usize, find: FindFace) {
    after_frames(window, 2, move |window, cx| match find(&shell, cx) {
        Some((of, face)) => shell.update(cx, |shell, cx| {
            shell.open_card(of, (Form::Compact, CardTab::Overview), face, window, cx);
        }),
        None if tries > 0 => card_beside_face(shell, window, tries - 1, find),
        None => {}
    });
}

/// c's full card on `tab`.
fn full_card(shell: &Entity<Shell>, tab: CardTab, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        let of = CardOf::new(LAB_C, None);
        shell.open_card(of, (Form::Full, tab), gpui::Bounds::default(), window, cx);
    });
}

/// The card open on `tab`.
fn card_tab(shell: &Entity<Shell>, tab: CardTab, window: &mut Window, cx: &mut App) {
    if let Some(card) = shell.read(cx).profile_card().cloned() {
        card.update(cx, |card, cx| card.show_tab(tab, window, cx));
    }
}

/// The team's channel with b's compact card open beside the face of b's post.
fn compact_card_in_channel(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| shell.close_card(window, cx));
    let list = shell.read(cx).list().clone();
    list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
    card_beside_face(shell.clone(), window, 10, |shell, cx| {
        let face = channel_posts(shell, cx)?.read(cx).face_bounds(CHANNEL_POST)?;
        Some((CardOf::new(LAB_B, None), face))
    });
}

/// The app in Teams Dark (`Some` of its key), or back to following the system's light or dark (`None`).
fn fixed_theme(shell: &Entity<Shell>, key: Option<&'static str>, cx: &mut App) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = key.is_none();
                settings.appearance.theme = key.unwrap_or(teams_ui::theme::LIGHT_KEY);
            },
            cx,
        );
    });
}

/// Puts a made-up picture in the open chat's box after a line of text, as a paste of it does (the paste itself, the
/// sandbox's clipboard read by GPUI, is `ui-sandbox`'s clipboard test and the compose tests'; the sandbox's niri puts
/// its own screenshots on its clipboard between steps, so a paste here would show one of those).
fn paste_picture(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    let mut png = Vec::new();
    if made_up_dashboard()
        .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return;
    }
    let Some(picture) = teams_ui::editor::pictures::Picture::of(png, window.scale_factor()) else {
        return;
    };
    with_compose(shell, cx, |compose, cx| {
        compose.focus(window, cx);
        compose.editor().update(cx, |editor, cx| {
            editor.paste_plain("Here is the dashboard", cx);
            editor.enter(true, false, window, cx);
            editor.insert_picture(picture, cx);
        });
    });
}

/// The made-up group call transcribing for the account, recording the audio alone (`audio`) or not, as the roster
/// tells it.
fn transcription_state(shell: &Entity<Shell>, audio: bool, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallMeeting {
            call: teams_core::CallId::for_tests(GROUP_CALL),
            state: teams_core::MeetingState {
                recording: audio.then(|| teams_core::Recording {
                    by: Some(Mri::new(LAB_A)),
                    audio_only: true,
                }),
                transcribing: Some(teams_core::Transcription {
                    by: Some(Mri::new(LAB_A)),
                }),
                ..teams_core::MeetingState::default()
            },
        },
        cx,
    );
}

/// What binds an attendee the organizer muted and kept from unmuting, its camera and presenting.
fn attendee() -> teams_core::MeetingState {
    teams_core::MeetingState {
        attendee: true,
        muted_by_organizer: Some(4),
        options: teams_proto::MeetingOptions {
            present: Some(false),
            unmute: Some(false),
            camera: Some(false),
            ..teams_proto::MeetingOptions::default()
        },
        ..teams_core::MeetingState::default()
    }
}

/// The made-up group call as its organizer's controls see it: the account organizing it or not, Lab Sea in it or
/// gone (`organizer`, `with_c`), Lab Bee and Lab Sea unmuted, and the meeting `state`; no recording's problem shown.
fn group_controls(
    shell: &Entity<Shell>,
    (organizer, with_c): (bool, bool),
    state: teams_core::MeetingState,
    cx: &mut App,
) {
    let models = shell.read(cx).models().clone();
    let call = teams_core::CallId::for_tests(GROUP_CALL);
    let participant = |mri: &str, name: &str, line: usize| teams_core::CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: false,
        line: Some(line),
        speaking: false,
        muted: false,
        hand_raised: false,
        organizer: false,
        joined: None,
        sharing: false,
    };
    let mut participants = vec![participant(LAB_B, "Lab Bee", 1)];
    if with_c {
        participants.push(participant(LAB_C, "Lab Sea", 2));
    }
    let events = [
        teams_core::Event::CallRecording {
            call,
            recording: teams_core::CallRecording::default(),
        },
        teams_core::Event::CallOwn {
            call,
            hand_raised: false,
            organizer,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants,
                speaking: false,
                hearing: false,
            },
        },
        teams_core::Event::CallMeeting { call, state },
    ];
    for event in events {
        models.apply(event, cx);
    }
}

/// The made-up group call recording, started by `by`, or not recording (`None`), as the roster tells it.
fn recording_state(shell: &Entity<Shell>, by: Option<&str>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallMeeting {
            call: teams_core::CallId::for_tests(GROUP_CALL),
            state: teams_core::MeetingState {
                recording: by.map(|by| teams_core::Recording {
                    by: Some(Mri::new(by)),
                    audio_only: false,
                }),
                ..teams_core::MeetingState::default()
            },
        },
        cx,
    );
}

/// The made-up group call's meeting state, as the engine tells it: a timer running for a minute already, Lab Bee's
/// recording, how to join, and the meeting `locked` or not.
fn meeting_state(shell: &Entity<Shell>, locked: bool, cx: &mut App) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| i64::try_from(now.as_millis()).unwrap_or(0));
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallMeeting {
            call: teams_core::CallId::for_tests(GROUP_CALL),
            state: teams_core::MeetingState {
                timer: Some(teams_proto::MeetingTimer {
                    state: teams_proto::TimerState::InProgress,
                    remaining_ms: 300_000,
                    original_ms: 300_000,
                    edited_ms: Some(now - 60_000),
                    previous: None,
                }),
                locked,
                join: Some(teams_proto::MeetingJoinInfo {
                    join_link: Some("https://teams.microsoft.com/meet/1112223334445?p=MadeUpKey".to_owned()),
                    meeting_id: Some("111 222 333 444 5".to_owned()),
                    passcode: Some("MadeUp1".to_owned()),
                    help_link: Some("https://aka.ms/JoinTeamsMeeting?omkt=en-US".to_owned()),
                    reference_link: Some("https://teams.microsoft.com/l/meetup-join/made-up".to_owned()),
                }),
                recording: Some(teams_core::Recording {
                    by: Some(Mri::new(LAB_B)),
                    audio_only: false,
                }),
                transcribing: None,
                ..teams_core::MeetingState::default()
            },
        },
        cx,
    );
}

/// The made-up 1:1 call on hold (`held`) or off it, as the engine tells it.
fn held(shell: &Entity<Shell>, held: bool, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallHeld {
            call: teams_core::CallId::for_tests(FAKE_CALL),
            held,
        },
        cx,
    );
}

/// The made-up 1:1 call held by its far end (`held`) or not, as the engine tells it.
fn held_by_far_end(shell: &Entity<Shell>, held: bool, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallHeldByFarEnd {
            call: teams_core::CallId::for_tests(FAKE_CALL),
            held,
        },
        cx,
    );
}

/// The app in the theme `key`.
fn with_activity(
    shell: &Entity<Shell>,
    cx: &mut App,
    change: impl FnOnce(&mut teams_ui::activity_view::ActivityView, &mut Context<teams_ui::activity_view::ActivityView>),
) {
    let view = shell.read(cx).activity_view().clone();
    view.update(cx, change);
}

/// The tour's feed items, oldest first, each read or not: `mine` is a's message in the 1:1 chat, `theirs` c's in the
/// group chat.
fn activity_items(mine_number: u64, theirs_number: u64) -> [(serde_json::Value, bool); 8] {
    let item = |kind: &str, sub: &str, (from, name): (&str, &str), (thread, message): (&str, u64), preview: &str| {
        serde_json::json!({
            "activityType": kind, "activitySubtype": sub, "activityId": 100_000 + message,
            "sourceThreadId": thread, "sourceMessageId": message, "sourceUserId": from,
            "sourceUserImDisplayName": name, "messagePreview": preview,
            "sourceThreadType": if thread.ends_with("tacv2") { "space" } else { "chat" },
        })
    };
    [
        (
            item("teamMembershipChange", "addedToTeam", (LAB_B, "Lab B"), (TEAM, 1), ""),
            true,
        ),
        (
            serde_json::json!({
                "activityType": "msGraph", "activitySubtype": "customNotification", "activityId": 7,
                "sourceThreadId": TEAM, "sourceMessageId": CHANNEL_POST, "sourceUserImDisplayName": "Workflows",
                "messagePreview": "Made-up reminder to follow up", "sourceThreadType": "space",
                "activityContext": {"templateParameters": "{\"customMessage\":\"Follow up on a message\"}"},
            }),
            true,
        ),
        (
            item(
                "mentionInChat",
                "everyone",
                (LAB_B, "Lab B"),
                (ABC_CHAT, theirs_number),
                "Everyone the made-up demo moved to Friday",
            ),
            true,
        ),
        (item("call", "missedCall", (LAB_B, "Lab B"), ("", 1), ""), false),
        (
            serde_json::json!({
                "activityType": "msGraph", "activitySubtype": "privateMeetingUpdated", "activityId": 8,
                "sourceThreadId": "", "sourceMessageId": 1, "sourceUserId": "28:app:made-up-calendar",
                "sourceUserImDisplayName": "Lab C", "messagePreview": "Made-up weekly sync",
                "activityContext": {
                    "attributedToActorId": LAB_C,
                    "location": "{\"Custom\":\"<DateTimeRange><StartDateTime>2025-10-10T14:30:00Z</StartDateTime><EndDateTime>2025-10-10T15:00:00Z</EndDateTime></DateTimeRange>\"}",
                },
            }),
            false,
        ),
        (
            item(
                "mentionInChat",
                "person",
                (LAB_C, "Lab C"),
                (ABC_CHAT, theirs_number),
                "Lab A can you check the made-up numbers before noon?",
            ),
            false,
        ),
        (
            item(
                "reactionInChat",
                "like",
                (LAB_C, "Lab C"),
                (AB_CHAT, mine_number),
                "The made-up report is ready for a look",
            ),
            true,
        ),
        (
            item(
                "reactionInChat",
                "heart",
                (LAB_B, "Lab B"),
                (AB_CHAT, mine_number),
                "The made-up report is ready for a look",
            ),
            false,
        ),
    ]
}

/// a's activity feed, made up for the tour: reactions of b and c to one of a's messages (folded into one item),
/// mentions in the group chat, a missed call from b, a meeting c updated, an app's notification and a team a was
/// added to; some read, some not.
fn seed_activity(mock: &ChatServiceMock) -> SandboxResult {
    let mine = mock
        .post_as(LAB_A, AB_CHAT, "<p>The made-up report is ready for a look</p>")
        .ok_or("a's post refused")?;
    let theirs = mock
        .post_as(
            LAB_C,
            ABC_CHAT,
            "<p>Lab A can you check the made-up numbers before noon?</p>",
        )
        .ok_or("c's post refused")?;
    let mine_number: u64 = mine.parse()?;
    let theirs_number: u64 = theirs.parse()?;
    for (activity, read) in activity_items(mine_number, theirs_number) {
        mock.add_activity(LAB_A, &activity, read).ok_or("no feed")?;
    }
    Ok(())
}

fn theme(shell: &Entity<Shell>, key: &'static str, cx: &mut App) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(|settings| settings.appearance.theme = key, cx);
    });
}

/// The made-up 1:1 call's shares as the engine tells them: the account's own (`sharing`), b's (`shown`).
fn share(shell: &Entity<Shell>, (sharing, shown): (bool, bool), cx: &mut App) {
    let models = shell.read(cx).models().clone();
    let call = teams_core::CallId::for_tests(FAKE_CALL);
    models.apply(teams_core::Event::CallSharing { call, on: sharing }, cx);
    models.apply(teams_core::Event::CallRemoteShare { call, on: shown }, cx);
}

/// The made-up group call's number.
const GROUP_CALL: u64 = 2;

/// The made-up call ended, and a group call in the a-b-c chat from b answered in its place, told to a's models as the
/// engine would tell it: b speaking on its camera (line 1), c muted with its hand up, and live reactions from c and
/// the account itself; b's picture a test pattern, fed while the tour runs.
fn fake_group_call(shell: &Entity<Shell>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
    });
    let call = teams_core::CallId::for_tests(GROUP_CALL);
    let participant = |mri: &str, name: &str, video: bool, speaking: bool, quiet: bool| teams_core::CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: video,
        line: Some(if video { 1 } else { 2 }),
        speaking,
        muted: quiet,
        hand_raised: quiet,
        organizer: false,
        joined: None,
        sharing: false,
    };
    let events = [
        teams_core::Event::Call {
            call: teams_core::CallId::for_tests(FAKE_CALL),
            status: teams_core::CallStatus::Ended(teams_core::End::HungUp),
        },
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some("Lab Bee".to_owned()),
            group_chat: Some(ABC_CHAT.to_owned()),
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants: vec![
                    participant(LAB_B, "Lab Bee", true, true, false),
                    participant(LAB_C, "Lab Sea", false, false, true),
                ],
                speaking: false,
                hearing: false,
            },
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
    react(shell, LAB_C, "heart", cx);
    react(shell, LAB_A, "like", cx);
    let Some(feed) = models.calls.read(cx).video().cloned() else {
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let pattern = TestPattern::new(640, 360);
        let output: &dyn VideoOutput = &feed;
        let mut sink = output.remote(1);
        for frame in 0.. {
            if sink.show(&pattern.frame(frame)).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    })
    .detach();
}

/// A live reaction `reaction` from `from` in the made-up group call, told as the engine would tell it.
fn react(shell: &Entity<Shell>, from: &str, reaction: &str, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallReaction {
            call: teams_core::CallId::for_tests(GROUP_CALL),
            from: Mri::new(from),
            name: None,
            reaction: reaction.to_owned(),
        },
        cx,
    );
}

/// Runs `change` on the call window's stage with its window, when it is open.
fn with_stage_window(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut Window, &mut Context<CallStage>)) {
    let found = cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        let stage = kit.read(cx).ok()?.view().clone().downcast::<CallStage>().ok()?;
        Some((kit, stage))
    });
    if let Some((kit, stage)) = found {
        let _ = kit.update(cx, |_, window, cx| {
            stage.update(cx, |stage, cx| change(stage, window, cx));
        });
    }
}

/// A fake Claude Code for the tour's message correction (made-up German): one draft answered slowly, one left as it is,
/// and one corrected.
const CORRECTING_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  case "$line" in
    *langsam*) sleep 30; ans='<unchanged/>';;
    *Ordnung*) ans='<unchanged/>';;
    *) ans='<corrected><x0>Ich habe gestern den Bericht geschickt. Kannst du ihn bitte durchsehen, wenn du Zeit hast?</x0></corrected>';;
  esac
  printf '{"type":"result","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

/// A fake Claude Code for the tour: it answers the made-up captions with their German, anything else unchanged.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  case "$line" in
    *"release plan"*) text="Guten Morgen zusammen. Gehen wir den Release-Plan durch.";;
    *"still want to"*) text="Vorher möchte ich noch";;
    *"passed overnight"*) text="Der Acme-Build ist über Nacht durchgelaufen, also können wir am Donnerstag ausliefern.";;
    *) text=$(printf '%s' "$line" | sed -n 's/.*<caption[^>]*>\(.*\)<\/caption>.*/\1/p');;
  esac
  printf '{"type":"result","is_error":false,"result":"%s"}\n' "$text"
done
"#;

/// Made-up local AI tools (both installed, at paths nothing runs) and the models they list.
fn made_up_ai_tools(cx: &mut App) {
    use teams_ui::local_ai::{Backend, Catalog, Listed, Tools};
    let listed = |id: &str, name: &str, latest: bool| Listed {
        id: id.to_owned(),
        name: name.to_owned(),
        latest,
    };
    cx.set_global(Tools::with(vec![
        (Backend::Claude, "/made-up/claude".into()),
        (Backend::Codex, "/made-up/codex".into()),
    ]));
    cx.set_global(Catalog::with(
        vec![
            listed("opus", "Opus", true),
            listed("sonnet", "Sonnet", true),
            listed("haiku", "Haiku", true),
            listed("claude-opus-5-5", "Opus 5.5", false),
            listed("claude-sonnet-5-5", "Sonnet 5.5", false),
        ],
        vec![
            listed("gpt-6.1-sol", "GPT-6.1-Sol", false),
            listed("gpt-6.1-terra", "GPT-6.1 Terra", false),
            listed("gpt-6-luna", "GPT-6-Luna", false),
        ],
    ));
}

/// The Calls page scrolled to its AI pickers: captions by Claude Code's Opus, recaps by a model typed for Codex.
fn show_ai_tools(shell: &Entity<Shell>, cx: &mut App) {
    use teams_ui::local_ai::{AiChoice, Backend, Model, ToolChoice};
    made_up_ai_tools(cx);
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.caption_translator = AiChoice {
                    tool: ToolChoice::Only(Backend::Claude),
                    model: Model::Named("opus".to_owned()),
                };
                settings.recap_summarizer = AiChoice {
                    tool: ToolChoice::Only(Backend::Codex),
                    model: Model::Named("my-team-model".to_owned()),
                };
            },
            cx,
        );
    });
    show_page(shell, Page::Calls, cx);
    shell.read(cx).settings_view().read(cx).scroll_to_end();
}

/// A made-up Claude Code translating a chat: each message of a batch (a reply's too, its `re` read past) with
/// "Übersetzt:" before its words, kept in its language (English); an outgoing message, after a moment, with
/// "(in English)" after it.
const FAKE_CHAT_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  echo '{"type":"system","subtype":"init"}'
  body=""
  case "$line" in
    *'<translate>'*)
      body=$(printf '%s' "$line" | sed 's/.*<translate>\\n//; s/\\n<\/translate>.*//')
      body=$(printf '%s' "$body" | sed 's/\(<m id=\\"[0-9]*\\"\) from=\\"[^\\]*\\"\( re=\\"[^\\]*\\"\)\{0,1\}>\(<x[0-9]*>\)\{0,1\}/\1 lang=\\"en\\">\3Übersetzt: /g')
      ;;
    *'<outgoing'*)
      sleep 2
      text=$(printf '%s' "$line" | sed 's/.*<outgoing to=\\"[a-z]*\\"\( re=\\"[^\\]*\\"\)\{0,1\}>\(.*\)<\/outgoing>.*/\2/')
      body="<out lang=\\\"en\\\">$text (in English)</out>"
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$body"
done
"#;

/// A made-up Claude Code translating a chat as [`FAKE_CHAT_CLAUDE`] does, but streaming its answer a message at a time
/// and in stages, the newest first as it is asked: nothing until the gate's first file exists, then the newer half of
/// the batch's answers, then the rest once its second file exists (each wait a minute at most). `__GATE__` is the
/// gate's path ([`staged_chat_tools`]).
const STAGED_CHAT_CLAUDE: &str = r#"#!/bin/bash
gate="__GATE__"
delta() { printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s\\n"}}}\n' "$1"; }
hold() { for _ in $(seq 1200); do [ -e "$gate-$1" ] && return; sleep 0.05; done; }
while IFS= read -r line; do
  echo '{"type":"system","subtype":"init"}'
  body=""
  case "$line" in
    *'<translate>'*)
      body=$(printf '%s' "$line" | sed 's/.*<translate>\\n//; s/\\n<\/translate>.*//')
      body=$(printf '%s' "$body" | sed 's/\(<m id=\\"[0-9]*\\"\) from=\\"[^\\]*\\"\( re=\\"[^\\]*\\"\)\{0,1\}>\(<x[0-9]*>\)\{0,1\}/\1 lang=\\"en\\">\3Übersetzt: /g')
      mapfile -t answers < <(printf '%s\n' "$body" | sed 's/\\n/\n/g' | tac)
      half=$(( (${#answers[@]} + 1) / 2 ))
      echo '{"type":"stream_event","event":{"type":"message_start"}}'
      hold 1
      for answer in "${answers[@]:0:$half}"; do delta "$answer"; done
      hold 2
      for answer in "${answers[@]:$half}"; do delta "$answer"; done
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$body"
done
"#;

/// The gate [`STAGED_CHAT_CLAUDE`] waits on, in the temporary directory.
fn staged_gate() -> PathBuf {
    std::env::temp_dir().join(format!("tour-chat-gate-{}", std::process::id()))
}

/// [`STAGED_CHAT_CLAUDE`] as the AI tool the app finds, its gate closed.
fn staged_chat_tools(cx: &mut App) {
    let gate = staged_gate();
    for stage in [1, 2] {
        let _ = fs::remove_file(format!("{}-{stage}", gate.display()));
    }
    chat_tools(&STAGED_CHAT_CLAUDE.replace("__GATE__", &gate.display().to_string()), cx);
}

/// Lets [`STAGED_CHAT_CLAUDE`] go on to its next `stage`.
fn release_staged(stage: u8) {
    let _ = fs::write(format!("{}-{stage}", staged_gate().display()), "");
}

/// A made-up Claude Code that isn't signed in.
const SIGNED_OUT_CLAUDE: &str = "#!/bin/bash\necho 'Invalid API key. Please run /login' >&2\nexit 1\n";

/// `script` as the AI tool the app finds (Claude Code), written to the temporary directory: never the real one.
fn chat_tools(script: &str, cx: &mut App) {
    use std::os::unix::fs::PermissionsExt as _;
    let program = std::env::temp_dir().join(format!("tour-chat-claude-{}-{}", std::process::id(), script.len()));
    let written =
        fs::write(&program, script).and_then(|()| fs::set_permissions(&program, fs::Permissions::from_mode(0o755)));
    if let Err(error) = written {
        eprintln!("tour chat translator: {error}");
        cx.quit();
        return;
    }
    cx.set_global(teams_ui::local_ai::Tools::with(vec![(
        teams_ui::local_ai::Backend::Claude,
        program,
    )]));
}

/// The open chat read in `language` (`None`: as written), as its header's picker sets it.
fn translate_chat(shell: &Entity<Shell>, language: Option<&'static str>, cx: &mut App) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        let translation = chat.read(cx).translation().clone();
        translation.update(cx, |translation, cx| translation.set_language(language, cx));
    }
}

/// The second chat of the list read in `language` (`None`: as written) without opening it: its row's preview is
/// translated outside its view.
fn translate_another_chat(shell: &Entity<Shell>, language: Option<&'static str>, cx: &mut App) {
    let open = shell
        .read(cx)
        .chat()
        .map(|chat| chat.read(cx).conversation().to_owned());
    let me = shell.read(cx).account().to_owned();
    // one whose preview is someone else's text message
    let other = shell
        .read(cx)
        .list()
        .read(cx)
        .rows()
        .iter()
        .filter(|row| Some(&row.id) != open.as_ref())
        .find(|row| {
            row.previewed.as_ref().is_some_and(|message| {
                message.sender.as_deref() != Some(me.as_str()) && !message.content.contains("<img")
            })
        })
        .map(|row| row.id.clone());
    if let Some(other) = other {
        let settings = shell.read(cx).settings().clone();
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.set_chat_language(&me, &other, language), cx);
        });
    }
}

/// Opens the open chat's language menu with `query` typed in its search field, or closes it (`None`).
fn language_menu(shell: &Entity<Shell>, query: Option<&str>, window: &mut Window, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let model = chat.read(cx).translation().clone();
    let picker = teams_ui::chat_translate::picker::picker_of(&model, window, cx);
    picker.update(cx, |picker, cx| {
        picker.set_open(query.is_some(), window, cx);
        if let Some(query) = query {
            picker.search(query, window, cx);
        }
    });
}

/// The made-up call's captions shown and translated into German by [`FAKE_CLAUDE`]: two lines said, one still spoken.
fn translated_captions(shell: &Entity<Shell>, cx: &mut App) {
    use std::os::unix::fs::PermissionsExt as _;
    let program = std::env::temp_dir().join(format!("tour-claude-{}", std::process::id()));
    let written = fs::write(&program, FAKE_CLAUDE)
        .and_then(|()| fs::set_permissions(&program, fs::Permissions::from_mode(0o755)));
    if let Err(error) = written {
        eprintln!("tour translator: {error}");
        cx.quit();
        return;
    }
    cx.set_global(teams_ui::local_ai::Tools::with(vec![(
        teams_ui::translate::Backend::Claude,
        program,
    )]));
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.caption_translate_to = Some("de");
                // the recommended model, whatever the settings' tour picked
                settings.caption_translator = teams_ui::translate::AiChoice::default();
            },
            cx,
        );
    });
    let line = |stamp: u64, who: (&str, &str), text: &str, final_result: bool| teams_core::Caption {
        id: format!("made-up/{stamp}"),
        speaker: Some(who.0.to_owned()),
        display_name: Some(who.1.to_owned()),
        text: text.to_owned(),
        final_result,
        spoken_language: "en-us".to_owned(),
        track_language: "en-us".to_owned(),
        audio_timestamp: stamp,
        duration: 10,
    };
    let bee = (LAB_B, "Lab Bee");
    let lines = vec![
        line(
            1,
            bee,
            "Good morning, everyone. Let's go through the release plan.",
            true,
        ),
        line(
            2,
            bee,
            "The Acme build passed overnight, so we can ship on Thursday.",
            true,
        ),
        // still spoken: sent once its text settles
        line(3, bee, "Before that I still want to", false),
    ];
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallCaptions {
            call: teams_core::CallId::for_tests(FAKE_CALL),
            captions: teams_core::CallCaptions {
                phase: teams_core::CaptionPhase::On,
                spoken_language: "en-us".to_owned(),
                error: None,
                lines,
            },
        },
        cx,
    );
}

/// Who waits in the made-up call's lobby, told to a's models as the engine would tell it.
fn lobby(shell: &Entity<Shell>, waiting: Vec<(&str, &str)>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::Lobby {
            call: teams_core::CallId::for_tests(FAKE_CALL),
            waiting: waiting
                .into_iter()
                .map(|(mri, name)| teams_core::LobbyPerson {
                    mri: Mri::new(mri),
                    name: Some(name.to_owned()),
                })
                .collect(),
        },
        cx,
    );
}

/// The made-up call's number.
const FAKE_CALL: u64 = 1;

/// A call from b answered, told to a's models as the engine would tell it: the invitation, its answer, its media
/// connected, the roster naming b speaking with its camera on line 0; b's picture a test pattern, fed while the tour
/// runs.
fn fake_call(shell: &Entity<Shell>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    let feed = VideoFeed::new();
    models.calls.update(cx, |calls, _| {
        *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO))).with_video(feed.clone());
    });
    let call = teams_core::CallId::for_tests(FAKE_CALL);
    let events = [
        teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some("Lab Bee".to_owned()),
            group_chat: None,
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants: vec![teams_core::CallParticipant {
                    mri: LAB_B.to_owned(),
                    name: Some("Lab Bee".to_owned()),
                    video_on: true,
                    line: Some(0),
                    speaking: true,
                    muted: false,
                    hand_raised: false,
                    organizer: false,
                    joined: None,
                    sharing: false,
                }],
                speaking: false,
                hearing: false,
            },
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let (pattern, screen) = (TestPattern::new(640, 360), TestPattern::new(1280, 720));
        let output: &dyn VideoOutput = &feed;
        let mut sink = output.remote(0);
        // b's shared screen on its sharing line (shown while the call tells b shares)
        let mut shared = output.remote(teams_media::session::SHARING_LINE);
        for frame in 0.. {
            if sink.show(&pattern.frame(frame)).is_err() || shared.show(&screen.frame(frame)).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    })
    .detach();
}

/// Runs `change` on the call window's stage, when it is open.
fn with_stage(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut Context<CallStage>)) {
    let stage = cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        kit.read(cx).ok()?.view().clone().downcast::<CallStage>().ok()
    });
    if let Some(stage) = stage {
        stage.update(cx, change);
    }
}

/// A 1:1 video call's health as the Call health pane shows it (made up): audio and video both ways, nothing shared.
fn tour_health() -> teams_core::CallHealth {
    use teams_core::calling::{MediaHealth, NetworkHealth, StreamHealth};
    let stream = |kbps, packets, video: Option<(u32, u32, f64)>, codec: &str| StreamHealth {
        bitrate_kbps: Some(kbps),
        packets,
        loss_percent: Some(0.4),
        jitter_ms: Some(3.0),
        round_trip_ms: Some(38.0),
        frame_rate: video.map(|(_, _, fps)| fps),
        resolution: video.map(|(width, height, _)| (width, height)),
        codec: Some(codec.to_owned()),
        hardware: video.map(|_| false),
    };
    teams_core::CallHealth {
        network: NetworkHealth {
            round_trip_ms: Some(38.0),
            received_loss_percent: Some(0.4),
            received_jitter_ms: Some(3.0),
            sent_loss_percent: Some(0.0),
            available_send_kbps: Some(2_480.0),
            available_receive_kbps: None,
        },
        audio: MediaHealth {
            sent: Some(stream(33.0, 6_212, None, "opus")),
            received: Some(stream(31.0, 6_190, None, "opus")),
        },
        video: MediaHealth {
            sent: Some(stream(1_180.0, 48_120, Some((1280, 720, 30.0)), "H264")),
            received: Some(stream(860.0, 39_005, Some((960, 540, 24.0)), "H264")),
        },
        sharing: MediaHealth::default(),
    }
}

/// Runs `change` on the scheduling form, in its window, when it is open.
/// The person the tour adds a calendar of (made up).
fn tour_person() -> teams_core::Invitable {
    teams_core::Invitable {
        mri: String::new(),
        name: "Avery Example".to_owned(),
        address: "avery@example.com".to_owned(),
    }
}

fn with_event_form(
    cx: &mut App,
    change: impl FnOnce(&mut event_form::EventForm, &mut Window, &mut Context<event_form::EventForm>),
) {
    let (Some(form), Some(handle)) = (event_form::shown(cx), event_form::window(cx)) else {
        say("no scheduling form");
        return;
    };
    let _ = handle.update(cx, |_, window, cx| form.update(cx, |form, cx| change(form, window, cx)));
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("window") => child_window(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ui tour: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox and the screenshots ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams ui tour", &options)? else {
        return Ok(());
    };
    tour(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    say("ngteams ui tour: ok");
    Ok(())
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The step whose screenshot is the screen, the main window and the call window side by side.
const SIDE_BY_SIDE: &str = "call-side-by-side";

/// The main window and the call window put side by side, each half the screen, and the screen shot into `file`; the
/// call window keeps the focus.
fn side_by_side(sandbox: &Sandbox, file: &str) -> SandboxResult<PathBuf> {
    for action in [
        &["action", "set-column-width", "50%"][..],
        &["action", "focus-column-left"],
        &["action", "set-column-width", "50%"],
        &["action", "focus-column-right"],
    ] {
        sandbox.niri_msg(action)?;
        // each move done before the next is reckoned from where the columns are
        thread::sleep(Duration::from_millis(600));
    }
    // niri's columns move into place
    thread::sleep(SETTLE);
    sandbox.screenshot(file, false)
}

fn tour(sandbox: &Sandbox) -> SandboxResult {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let child = sandbox.spawn_with_fds(
        &std::env::current_exe()?,
        &[],
        &[(CHILD_ENV, "window"), (TRACE_FD_ENV, &fd)],
        &[writer.as_fd()],
    )?;
    // the child holds the only writer from here, so the trace ends when it exits
    drop(writer);
    let mut running = Running::new(child)?;
    // the trace is read only to keep its pipe from filling
    thread::spawn(move || for _ in BufReader::new(reader).lines() {});
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    for (name, _) in STEPS {
        running
            .line_where(WAIT, |line| line == format!("shot {name}"))
            .map_err(|error| running.fail(&format!("no shot {name}: {error:?}")))?;
        let file = format!("tour-{name}.png");
        let png = if name == SIDE_BY_SIDE {
            side_by_side(sandbox, &file)?
        } else {
            sandbox.screenshot(&file, true)?
        };
        fs::copy(&png, shots_dir()?.join(&file))?;
        fs::write(sandbox.dir().join(NEXT), "")?;
    }
    running
        .line_where(WAIT, |line| line == "toured")
        .map_err(|error| running.fail(&format!("the tour didn't end: {error:?}")))?;
    drop(running);
    Ok(())
}

// --- the child: the app on the mock, walked through its views ---

/// The shell the window shows.
fn shown(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// The chat shown and the id of its last message that offers actions (a deleted one offers none), once it has one.
fn last_message(shell: &Entity<Shell>, cx: &App) -> Option<(Entity<teams_ui::chat_view::ChatView>, String)> {
    let chat = shell.read(cx).chat()?.clone();
    let message = chat.read(cx).items().iter().rev().find_map(|item| match item {
        Item::Message(message) if !message.actions().is_empty() => Some(message.id.clone()),
        _ => None,
    })?;
    Some((chat, message))
}

/// Goes to the message the chat's first reply quotes, as a click on its quote does.
fn go_to_quote(shell: &Entity<Shell>, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let quoted = chat.read(cx).message_items().find_map(|message| match &message.body {
        teams_ui::chat_view::Body::Text(text) => text.0.blocks.iter().find_map(|block| match block {
            teams_ui::rich_text::Block::Quote(quote) => quote.message_id.clone().map(|id| (id, quote.time)),
            _ => None,
        }),
        _ => None,
    });
    if let Some((id, time)) = quoted {
        chat.update(cx, |chat, cx| chat.go_to_quoted(id, time, cx));
    }
}

/// Selects text from the fourth character of the chat's second-to-last text message to the eighth of its last, as a
/// drag with the mouse would.
fn select_across_last_two(shell: &Entity<Shell>, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let view = chat.read(cx);
    let texts: Vec<(teams_ui::selection::MessageKey, Vec<usize>, usize)> = view
        .message_items()
        .filter_map(|message| match &message.body {
            teams_ui::chat_view::Body::Text(text) => teams_ui::rich_text::units(&text.0)
                .first()
                .map(|unit| (teams_ui::chat_view::message_key(message), unit.path.clone(), unit.len())),
            _ => None,
        })
        .collect();
    let [.., from, to] = texts.as_slice() else {
        return;
    };
    let spot = |(message, path, len): &(teams_ui::selection::MessageKey, Vec<usize>, usize), offset: usize| {
        teams_ui::selection::Spot {
            message: message.clone(),
            path: path.clone(),
            offset: offset.min(*len),
        }
    };
    view.text_selection().select(spot(from, 3), spot(to, 8));
    chat.update(cx, |_, cx| cx.notify());
}

/// Plays the first video card drawn, or pauses it while it plays.
fn toggle_first_video(cx: &mut App) {
    if let Some(player) = teams_ui::video_player::players(cx).first() {
        player.update(cx, teams_ui::video_player::VideoPlayer::toggle);
    }
}

/// Shows the hover bar of the last message of b's (`own` false) or a's that offers actions, as the pointer over it
/// would, nothing selected.
fn hover_last(shell: &Entity<Shell>, own: bool, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let message = chat.read(cx).items().iter().rev().find_map(|item| match item {
        Item::Message(message) if message.own == own && !message.actions().is_empty() => Some(message.id.clone()),
        _ => None,
    });
    chat.update(cx, |chat, cx| {
        chat.select(None, cx);
        chat.hover(message, cx);
    });
}

/// The posts' view of the channel the shell shows.
fn channel_posts(shell: &Entity<Shell>, cx: &App) -> Option<Entity<teams_ui::ChatView>> {
    let channel = shell.read(cx).channel()?;
    Some(channel.read(cx).posts().clone())
}

/// Folds the favourites and the chats, so the teams' section shows.
fn show_teams_section(shell: &Entity<Shell>, cx: &mut App) {
    let sidebar = shell.read(cx).sidebar().clone();
    sidebar.update(cx, |sidebar, cx| {
        for section in [teams_ui::sidebar::Section::Favorites, teams_ui::sidebar::Section::Chats] {
            if sidebar.is_open(section) {
                sidebar.toggle(section, cx);
            }
        }
    });
}

/// Unfolds them again.
fn show_chats_section(shell: &Entity<Shell>, cx: &mut App) {
    let sidebar = shell.read(cx).sidebar().clone();
    sidebar.update(cx, |sidebar, cx| {
        for section in [teams_ui::sidebar::Section::Favorites, teams_ui::sidebar::Section::Chats] {
            if !sidebar.is_open(section) {
                sidebar.toggle(section, cx);
            }
        }
    });
}

/// `act` on "Your teams and channels", once it was opened.
fn with_teams_page(
    shell: &Entity<Shell>,
    cx: &mut App,
    act: impl FnOnce(&mut teams_ui::teams_page::TeamsPage, &mut gpui::Context<teams_ui::teams_page::TeamsPage>),
) {
    if let Some(page) = shell.read(cx).teams_page().cloned() {
        page.update(cx, act);
    }
}

/// `act` on Manage team, while it is shown.
fn with_manage_team(
    shell: &Entity<Shell>,
    cx: &mut App,
    act: impl FnOnce(&mut teams_ui::manage_team::ManageTeam, &mut gpui::Context<teams_ui::manage_team::ManageTeam>),
) {
    if let Some(page) = shell.read(cx).manage_team().cloned() {
        page.update(cx, act);
    }
}

/// `act` on "More create team options", while it is shown.
fn with_create_more(
    shell: &Entity<Shell>,
    cx: &mut App,
    act: impl FnOnce(
        &mut teams_ui::create_team_more::CreateTeamMore,
        &mut gpui::Context<teams_ui::create_team_more::CreateTeamMore>,
    ),
) {
    if let Some(page) = shell.read(cx).create_team_more().cloned() {
        page.update(cx, act);
    }
}

/// `act` on the team page's dialog `D`, while it is the one open.
fn with_team_dialog<D: gpui::Render>(
    shell: &Entity<Shell>,
    cx: &mut App,
    act: impl FnOnce(&mut D, &mut gpui::Context<D>),
) {
    let dialog = shell
        .read(cx)
        .team_dialog()
        .cloned()
        .and_then(|dialog| dialog.downcast::<D>().ok());
    if let Some(dialog) = dialog {
        dialog.update(cx, act);
    }
}

/// Opens the mock's team's "Create a channel" dialog.
fn open_channel_dialog(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    let sidebar = shell.read(cx).sidebar().clone();
    sidebar.update(cx, |sidebar, cx| sidebar.close_team_menu(window, cx));
    let teams = shell.read(cx).teams().clone();
    teams.update(cx, |teams, cx| teams.add_channel(TEAM, cx));
}

/// Opens the mock's team's General channel's "Channel notification settings" in place of any team dialog.
fn open_channel_notifications(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx, Shell::close_team_dialog);
    let team = shell
        .read(cx)
        .teams()
        .read(cx)
        .teams()
        .and_then(|teams| teams.iter().find(|team| team.id == TEAM))
        .cloned();
    if let Some(team) = team
        && let Some(channel) = team.channels.first().cloned()
    {
        shell.update(cx, |shell, cx| {
            shell.channel_chosen(
                &team,
                &channel,
                teams_ui::channel_menu::ChannelChoice::Notifications,
                window,
                cx,
            );
        });
    }
}

/// Fills the dialog as a person would: a private channel, laid out as posts.
fn fill_channel_dialog(
    dialog: &mut teams_ui::channel_dialog::ChannelDialog,
    window: &mut Window,
    cx: &mut gpui::Context<teams_ui::channel_dialog::ChannelDialog>,
) {
    dialog.pick_kind(teams_proto::NewChannelKind::Private, cx);
    dialog.close_types(window, cx);
    dialog.pick_layout(teams_proto::ChannelLayout::Posts, cx);
    dialog.set_name("Release planning", window, cx);
    dialog.set_description("Dates and owners for the next release", window, cx);
}

/// `act` on the "Create a channel" dialog, while it is open.
fn with_channel_dialog(
    shell: &Entity<Shell>,
    cx: &mut App,
    act: impl FnOnce(
        &mut teams_ui::channel_dialog::ChannelDialog,
        &mut gpui::Context<teams_ui::channel_dialog::ChannelDialog>,
    ),
) {
    if let Some(dialog) = shell.read(cx).channel_dialog().cloned() {
        dialog.update(cx, act);
    }
}

/// Scrolls the chat shown to its top: its first message (the a–x chat's mixed one, above the forwards).
fn show_first_message(shell: &Entity<Shell>, cx: &mut App) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        chat.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        });
        cx.refresh_windows();
    }
}

/// Sets the app's text size to `size` px.
fn set_font_size(shell: &Entity<Shell>, size: u16, cx: &mut App) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(|settings| settings.appearance.font_size = size, cx);
    });
}

fn hover_none(shell: &Entity<Shell>, cx: &mut App) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        chat.update(cx, |chat, cx| {
            chat.hover(None, cx);
            chat.show_reactors(None, cx);
        });
    }
}

fn focus_reaction(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let reacted = chat
        .read(cx)
        .message_items()
        .filter(|message| !message.reactions.is_empty())
        .max_by_key(|message| {
            message
                .reactions
                .iter()
                .map(teams_ui::chat_view::ReactionCount::count)
                .sum::<usize>()
        })
        .map(|message| message.id.clone());
    chat.update(cx, |chat, cx| {
        chat.show_reactors(None, cx);
        chat.select(reacted, cx);
    });
    let focus = chat.read(cx).focus_handle().clone();
    focus.focus(window, cx);
    window.dispatch_action(Box::new(teams_ui::chat_view::reactors::NextReaction), cx);
}

fn show_reactors(shell: &Entity<Shell>, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let most = chat
        .read(cx)
        .message_items()
        .flat_map(|message| message.reactions.iter().map(move |reaction| (message, reaction)))
        .max_by_key(|(_, reaction)| reaction.count())
        .map(|(message, reaction)| (message.id.clone(), reaction.key.clone()));
    chat.update(cx, |chat, cx| {
        chat.hover(None, cx);
        chat.show_reactors(most.as_ref().map(|(message, key)| (message.as_str(), key.as_str())), cx);
    });
}

/// Runs `change` on the compose box of the chat shown.
/// Changes the chat header's people flyout, while it is open.
fn with_people(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut ChatPeople, &mut Context<ChatPeople>)) {
    if let Some(view) = shell.read(cx).people().cloned() {
        view.update(cx, change);
    }
}

/// Changes New message, while it is open.
fn with_new_chat(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut NewChat, &mut Context<NewChat>)) {
    if let Some(view) = shell.read(cx).new_chat().cloned() {
        view.update(cx, change);
    }
}

fn with_compose(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut Compose, &mut Context<Compose>)) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        let compose = chat.read(cx).compose().clone();
        compose.update(cx, change);
    }
}

/// Expands the compose box with its format bar and fills it with formatted text (made up): a heading, a paragraph of
/// bold, coloured and highlighted runs, and a bulleted list, the cursor at its end.
fn expand_with_formats(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    use teams_proto::chat::{Marks, Rgb, SpanStyle};
    use teams_ui::editor::doc::{Attrs, Block, Doc, Kind, Run};
    let bold = Attrs {
        marks: Marks::BOLD,
        ..Attrs::default()
    };
    let coloured = Attrs {
        span: SpanStyle {
            color: Some(Rgb(0x1E_53A3)),
            highlight: Some(Rgb(0xEB_D3E1)),
            size: None,
        },
        ..Attrs::default()
    };
    let doc = Doc::new(vec![
        Block::new(Kind::Heading(2), 0, vec![Run::new("Friday's plan", Attrs::default())]),
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                Run::new("Lunch is ", Attrs::default()),
                Run::new("at noon", bold),
                Run::new(", then the ", Attrs::default()),
                Run::new("review", coloured),
                Run::new(":", Attrs::default()),
            ],
        ),
        Block::new(Kind::Bullet, 0, vec![Run::new("slides from Lab Bee", Attrs::default())]),
        Block::new(Kind::Bullet, 0, vec![Run::new("the demo", Attrs::default())]),
    ]);
    with_compose(shell, cx, |compose, cx| {
        if compose.emoji_picker().is_some() {
            compose.toggle_emoji(window, cx);
        }
        if !compose.is_expanded() {
            compose.toggle_expanded(window, cx);
        }
        compose.editor().update(cx, |editor, cx| editor.set_doc(doc, cx));
        compose.focus(window, cx);
    });
}

/// Closes the emoji picker if it is open, gives the compose box the focus and types `text` into it, in place of what
/// an earlier step left there.
fn type_in_compose(shell: &Entity<Shell>, text: &str, window: &mut Window, cx: &mut App) {
    with_compose(shell, cx, |compose, cx| {
        if compose.emoji_picker().is_some() {
            compose.toggle_emoji(window, cx);
        }
        compose.focus(window, cx);
        compose.editor().update(cx, |editor, cx| {
            editor.set_doc(teams_ui::editor::doc::Doc::default(), cx);
            editor.insert(text, cx);
        });
    });
}

fn select_last_message(shell: &Entity<Shell>, _: &mut Window, cx: &mut App) {
    if let Some((chat, message)) = last_message(shell, cx) {
        chat.update(cx, |chat, cx| chat.select(Some(message), cx));
    }
}

/// A microphone, a headset, speakers and a camera, the system's default marked.
fn made_up_devices() -> DeviceList {
    let device = |kind, name: &str, description: &str| DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    };
    DeviceList::new(vec![
        device(DeviceKind::Microphone, "built-in-mic", "Built-in microphone"),
        device(DeviceKind::Microphone, "usb-headset-mic", "USB headset"),
        device(DeviceKind::Speaker, "built-in-speakers", "Speakers"),
        device(DeviceKind::Camera, "webcam", "Integrated webcam"),
    ])
    .with_default(DeviceKind::Microphone, Some("built-in-mic"))
}

fn show_page(shell: &Entity<Shell>, page: Page, cx: &mut App) {
    shell.update(cx, Shell::show_settings);
    let view = shell.read(cx).settings_view().clone();
    view.update(cx, |view, cx| view.show_page(page, cx));
}

/// The made-up release the update steps show.
const TOUR_RELEASE: &str = "0.1.0.13";

fn update_shown(status: UpdateStatus, cx: &mut App) {
    teams_ui::update::set_status(status, cx);
}

fn available() -> UpdateStatus {
    UpdateStatus::Available(TOUR_RELEASE.into())
}

fn downloading() -> UpdateStatus {
    UpdateStatus::Downloading {
        version: TOUR_RELEASE.into(),
        percent: 42,
    }
}

fn update_failed() -> UpdateStatus {
    UpdateStatus::Failed {
        version: TOUR_RELEASE.into(),
        reason: "There isn't enough free space in /home/you/.local/opt for the update (it needs about 255 MB). \
                 Nothing was changed."
            .into(),
    }
}

/// The a–b chat shown in the catalog's theme `key`, whatever the system says.
fn show_chat_in_theme(shell: &Entity<Shell>, key: &'static str, window: &mut Window, cx: &mut App) {
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            },
            cx,
        );
    });
    shell.update(cx, |shell, cx| {
        shell.show_chats(window, cx);
        shell.open_chat(AB_CHAT.to_owned(), window, cx);
    });
    clear_compose(shell, window, cx);
}

/// The open chat's compose box back to empty and one line: the emoji picker closed, the draft and its formats gone.
fn clear_compose(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    with_compose(shell, cx, |compose, cx| {
        if compose.emoji_picker().is_some() {
            compose.toggle_emoji(window, cx);
        }
        if compose.is_expanded() {
            compose.toggle_expanded(window, cx);
        }
        compose.editor().update(cx, |editor, cx| {
            editor.set_doc(teams_ui::editor::doc::Doc::default(), cx);
        });
    });
}

/// Scrolls the sidebar to the "Chats" section's See more.
fn reveal_see_more(shell: &Entity<Shell>, cx: &mut App) {
    let sidebar = shell.read(cx).sidebar().clone();
    sidebar.update(cx, Sidebar::reveal_see_more);
}

/// Presses `keys` in the window, as a person does: the keyboard's rings show from then on. Once the step's update is
/// over, as the keys reach the kit's root, which the step holds.
fn press(window: &mut Window, cx: &mut App, keys: &'static str) {
    window.defer(cx, move |window, cx| {
        for spec in keys.split(' ') {
            if let Ok(keystroke) = gpui::Keystroke::parse(spec) {
                window.dispatch_keystroke(keystroke, cx);
            }
        }
    });
}

/// Walks [`STEPS`], each one's screenshot taken by the parent before the next.
fn walk(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let next = PathBuf::from(dir).join(NEXT);
    cx.spawn(async move |cx: &mut AsyncApp| {
        // the catch-up lists the chats
        cx.background_executor().timer(SETTLE).await;
        for (name, step) in STEPS {
            if let Some(shell) = shown(window, cx) {
                let _ = window.update(cx, |_, window, cx| step(&shell, window, cx));
            }
            cx.background_executor().timer(SETTLE).await;
            say(&format!("shot {name}"));
            while fs::remove_file(&next).is_err() {
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
        }
        say("toured");
    })
    .detach();
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock and a's engine on it, as `teams_core::account::start` builds one but for the mock origins, a test roster and
/// a test log; its push channel on the Trouter mock.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    _mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        seed_many_chats(&mock)?;
        seed_group(&mock, &runtime)?;
        seed_pictures(&mock, &runtime)?;
        // a picture pasted into the box is sent to the mock's media service (`paste_picture`)
        let mut png = Vec::new();
        made_up_dashboard().write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        runtime.block_on(mock.serve_picture_sends(PASTED_OBJECT, png));
        seed_files(&mock, &runtime)?;
        seed_mixed(&mock, &runtime)?;
        seed_own(&mock)?;
        seed_mentions(&mock)?;
        seed_activity(&mock)?;
        seed_forwards(&mock, &runtime)?;
        seed_cards(&mock)?;
        seed_profile_cards(&mock, &runtime);
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });

        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        // the outbox sends the calendar's writes (Meet now's meeting) to the middle tier
        // and a pasted picture to AMS
        let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?.with_ams(AMS_BASE)?);

        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?
                    .with_loki(),
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?;
        let lab = Self {
            _runtime: runtime,
            _mock: mock,
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }
}

/// b's and c's profile cards on the mock's Loki (`ChatServiceMock::serve_loki`): made-up names and addresses, each
/// working with the other, the fixture's schedule; their presence is `seed_group`'s.
fn seed_profile_cards(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) {
    runtime.block_on(mock.serve_loki(&[
        LokiCard {
            mri: LAB_B,
            name: "Lab Bee",
            email: "bot-b@lab.example.test",
        },
        LokiCard {
            mri: LAB_C,
            name: "Lab Sea",
            email: "bot-c@lab.example.test",
        },
    ]));
}

/// c's message in the a–b–c chat, then b's mention of everyone, before b's other messages; the mock's UPS answering b as away (as the "chat" step tells it) and c
/// as busy, so the chat's senders show two badges once the view asked about them.
fn seed_group(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    runtime.block_on(mock.serve_presence(&[(LAB_B, "Away"), (LAB_C, "Busy")]));
    mock.post_as(
        LAB_C,
        ABC_CHAT,
        "<p>Morning all, the release notes are ready for a look</p>",
    )
    .ok_or("c's post refused")?;
    // b's mention of everyone, as Teams sends one (`mention-everyone.json`): the chat's id as its MRI
    let html = "<p><span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Everyone</span> \
                standup moves to 10:30 tomorrow</p>";
    let id = mock.post_as(LAB_B, ABC_CHAT, html).ok_or("b's post refused")?;
    let mentions = serde_json::json!([{
        "@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": ABC_CHAT, "mentionType": "everyone",
        "displayName": "Everyone",
    }])
    .to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    if !mock.change_message(ABC_CHAT, &client_message_id, |message| {
        message["properties"]["mentions"] = serde_json::json!(mentions);
    }) {
        return Err("b's post isn't in the mock".into());
    }
    Ok(())
}

/// More chats than the "Chats" section shows at first, older than every other chat, each with a's message but one with
/// b's, unread: it is below the first rows and among the ones See more brings.
fn seed_many_chats(mock: &ChatServiceMock) -> SandboxResult {
    for number in (1..=50).rev() {
        let topic = format!("Project {number}");
        let chat = mock.add_group(&topic, &[LAB_A, LAB_B, LAB_C]).ok_or("no chat made")?;
        let (from, text) = if number == 30 {
            (LAB_B, "<p>Could you look at this one?</p>")
        } else {
            (LAB_A, "<p>Thanks, all done here</p>")
        };
        mock.post_as(from, &chat, text).ok_or("the post refused")?;
    }
    Ok(())
}

/// A pasted picture's made-up object, and the view of it b's message names (on the media service's host, read from
/// the account's AMS base).
const PICTURE_OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";

/// The media service's object a picture pasted into the box is sent into (made up).
const PASTED_OBJECT: &str = "0-eus-d9-00000000000000000000000000000ab9";

/// An emoticon's `<img>` as Teams sends it, in its span (`docs/protocol/chat.md`, "Pictures in messages").
fn emoticon(id: &str, alt: &str, side: u32) -> String {
    format!(
        "<span title=\"{id}\" type=\"({id})\" class=\"animated-emoticon-{side}-{id}\" itemscope=\"\"><img itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Emoji\" itemid=\"{id}\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/{id}/default/\
         20_f.png\" alt=\"{alt}\" style=\"width:{side}px; height:{side}px\"></span>"
    )
}

/// b's two messages in the a–b–c chat as the ones Teams shows: a status line, a pasted picture of a dashboard (made up,
/// served by the mock as AMS would) and three lines, the second ending in an emoticon and the third with two inside it;
/// then a wave alone.
fn seed_pictures(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut png = Vec::new();
    made_up_dashboard().write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{PICTURE_OBJECT}/views/imgo"), "image/png", png));
    let html = format!(
        "<p>Made-up board summary for the lab week:</p>\r\n\
         <p><img src=\"https://us-api.asm.skype.com/v1/objects/{PICTURE_OBJECT}/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"884\" height=\"228\" alt=\"image\" \
         id=\"x_{PICTURE_OBJECT}\" itemid=\"{PICTURE_OBJECT}\"></p>\r\n\
         <p>12 made-up cards moved</p>\r\n<p>7 made-up cards added{}</p>\r\n\
         <p>Thanks!{} That is the whole made-up list {}</p>",
        emoticon("speechless", "\u{1f610}", 20),
        emoticon("laugh", "\u{1f606}", 20),
        emoticon("wink", "\u{1f609}", 20)
    );
    mock.post_as(LAB_B, ABC_CHAT, &html).ok_or("b's picture refused")?;
    let wave = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    mock.post_as(LAB_B, ABC_CHAT, &wave).ok_or("b's wave refused")?;
    let mut gif = Vec::new();
    write_made_up_gif(&mut gif)?;
    runtime.block_on(mock.serve_bytes(&format!("/media/{GIF_ID}/giphy.gif"), "image/gif", gif));
    // as the web client sends one picked from its GIF picker: between two blank paragraphs, on a shard of Giphy's host
    let picked = format!(
        "<p>&nbsp;</p>\n<img src=\"https://media2.giphy.com/media/v1.Y2lkPW1hZGUtdXA/{GIF_ID}/giphy.gif\" width=\"250\" \
         height=\"200\" alt=\"Made Up News GIF (GIF Image)\" style=\"height:auto; margin-top:4px; max-width:100%\">\n\
         <p>&nbsp;</p>"
    );
    mock.post_as(LAB_C, ABC_CHAT, &picked).ok_or("c's GIF refused")?;
    seed_link_and_screenshot(mock, runtime)
}

/// A made-up phone screenshot's object, and the size its message gives it (a phone's, twice as high as wide and more).
const SCREENSHOT_OBJECT: &str = "0-eus-d9-00000000000000000000000000000ab3";

/// The made-up page picture b's link preview names, as the async gateway's URL preview thumbnail of it.
const PREVIEW_PICTURE: &str = "https%3a%2f%2fcdn.example.test%2fmade-up%2fcard-800x418.png";

/// b's link with its preview (`link-preview.json`'s shape: `properties.links`, its picture through the async gateway's
/// URL preview service, served by the mock), then b's phone screenshot alone, as the message Teams draws as a thumbnail
/// in a square frame with no bubble.
fn seed_link_and_screenshot(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut png = Vec::new();
    made_up_panel(640, 334).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes("/urlp/v1/url/image/Thumbnail", "image/png", png));
    let html = "<p>Is this the right <a href=\"http://made-up.example.test\" target=\"_blank\" \
                rel=\"noreferrer noopener\">made-up.example.test</a> account </p>";
    let id = mock.post_as(LAB_B, ABC_CHAT, html).ok_or("b's link refused")?;
    let links = serde_json::json!([{
        "itemid": "0",
        "preview": {
            "description": "A made-up page's description, long enough to run over two lines of the card: every word \
                            of it is invented for the tour and names nothing real.",
            "previewMetadata": {"height": 167.0, "width": 320.0},
            "previewurl": format!(
                "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url={PREVIEW_PICTURE}"
            ),
            "title": "Made-up Site, your made-up page for the tour",
        },
        "previewenabled": true,
        "@type": "http://schema.skype.com/HyperLink",
        "url": "http://made-up.example.test",
    }])
    .to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    if !mock.change_message(ABC_CHAT, &client_message_id, |message| {
        message["properties"]["links"] = serde_json::json!(links);
    }) {
        return Err("b's link isn't in the mock".into());
    }
    let mut png = Vec::new();
    made_up_panel(540, 1260).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{SCREENSHOT_OBJECT}/views/imgo"), "image/png", png));
    let screenshot = format!(
        "<p><img alt=\"Media\" itemscope=\"\" itemtype=\"http://schema.skype.com/AMSImage\" \
         src=\"https://us-api.asm.skype.com/v1/objects/{SCREENSHOT_OBJECT}/views/imgo\" width=\"1080\" \
         height=\"2520\"></p>"
    );
    mock.post_as(LAB_B, ABC_CHAT, &screenshot)
        .ok_or("b's screenshot refused")?;
    Ok(())
}

/// The made-up pictures x's mixed message shows: their objects, and the sizes the message gives them (half their
/// pixels', as a picture pasted from a screen of twice the density is given).
const MIXED_PICTURES: [(&str, (u32, u32)); 2] = [
    ("0-eus-d9-00000000000000000000000000000ab1", (198, 53)),
    ("0-eus-d9-00000000000000000000000000000ab2", (535, 83)),
];

/// x's message in the a–x chat in the shape of one Teams shows (made up): a line, a small picture, a line, a wider
/// picture, a line, a block of code as Teams' code block editor sends one (its own paragraph, `pre` with its language,
/// lines split by `<br>`, highlight.js spans), a last line.
fn seed_mixed(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut pictures = Vec::new();
    for (object, (width, height)) in MIXED_PICTURES {
        let mut png = Vec::new();
        made_up_panel(width * 2, height * 2).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), "image/png", png));
        pictures.push(format!(
            "<p><img src=\"https://us-api.asm.skype.com/v1/objects/{object}/views/imgo\" \
             itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"{width}\" height=\"{height}\" \
             alt=\"image\" id=\"x_{object}\" itemid=\"{object}\"></p>"
        ));
    }
    let key = |name: &str| format!("<span class=\"hljs-function\">&quot;{name}&quot;</span>");
    let html = format!(
        "<p>The service never sends a proper error, it comes over the socket like this</p>\n{}\n\
         <p>Which is odd already. The socket's tab shows it like this</p>\n{}\n\
         <p>and through the client library it arrives as this object:</p>\n<p></p>\n\
         <p itemtype=\"http://schema.skype.com/CodeBlockEditor\" id=\"x_codeBlockEditor-made-up\">\n&nbsp;</p>\n\
         <pre class=\"language-json language-was-manually-selected\" itemid=\"codeBlockEditor-made-up\"><code>{{<br> \
         &nbsp;&nbsp; {}:&nbsp;<span class=\"hljs-string\">&quot;error&quot;</span>,<br> &nbsp;&nbsp; {}:&nbsp;\
         <span class=\"hljs-keyword\">null</span>,<br> &nbsp;&nbsp; {}:&nbsp;<span class=\"hljs-string\">\
         &quot;Made&nbsp;up&nbsp;limit&nbsp;reached.&quot;</span><br>}}</code></pre>\n<p></p>\n\
         <p>without any request id at all</p>\n<p>&nbsp;</p>",
        pictures[0],
        pictures[1],
        key("type"),
        key("detail"),
        key("error"),
    );
    mock.post_as(OUTSIDER, AX_CHAT, &html).ok_or("x's message refused")?;
    Ok(())
}

/// A made-up panel, `width` by `height`: a dark strip over a blue band with a light bar in it.
fn made_up_panel(width: u32, height: u32) -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(width, height, image::Rgba([30, 30, 30, 255]));
    for y in height / 3..height {
        for x in 0..width {
            let colour = if y < height * 2 / 3 && x > width / 10 && x < width / 2 {
                [235, 235, 235, 255]
            } else {
                [20, 80, 160, 255]
            };
            picture.put_pixel(x, y, image::Rgba(colour));
        }
    }
    picture
}

/// The made-up GIF c's message names on Giphy (served by the mock as Giphy's media host would).
const GIF_ID: &str = "MadeUpGif000";

/// A made-up animated GIF, 250 by 200: a red banner on dark blue, a light bar sweeping across it in six frames.
fn write_made_up_gif(out: &mut Vec<u8>) -> SandboxResult {
    let mut encoder = image::codecs::gif::GifEncoder::new(out);
    encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
    for at in 0u32..6 {
        let mut frame = image::RgbaImage::from_pixel(250, 200, image::Rgba([20, 30, 70, 255]));
        for y in 120..170 {
            for x in 0..250 {
                frame.put_pixel(x, y, image::Rgba([200, 30, 40, 255]));
            }
        }
        for y in 135..155 {
            for x in (at * 40)..(at * 40 + 60).min(250) {
                frame.put_pixel(x, y, image::Rgba([245, 245, 245, 255]));
            }
        }
        let delay = image::Delay::from_numer_denom_ms(150, 1);
        encoder.encode_frame(image::Frame::from_parts(frame, 0, 0, delay))?;
    }
    Ok(())
}

/// A made-up dashboard, 884 by 228: four cards on black, each with a coloured figure.
fn made_up_dashboard() -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(884, 228, image::Rgba([12, 12, 12, 255]));
    let mut fill = |x0: u32, y0: u32, width: u32, height: u32, colour: [u8; 3]| {
        for y in y0..(y0 + height).min(228) {
            for x in x0..(x0 + width).min(884) {
                picture.put_pixel(x, y, image::Rgba([colour[0], colour[1], colour[2], 255]));
            }
        }
    };
    // the title and the subtitle
    fill(16, 14, 250, 16, [235, 235, 235]);
    fill(16, 40, 520, 8, [120, 120, 120]);
    let figures = [[74, 222, 128], [59, 130, 246], [245, 245, 245], [245, 245, 245]];
    for (at, figure) in (0u32..).zip(figures) {
        let x = 16 + at * 214;
        fill(x, 72, 200, 140, [44, 44, 44]);
        fill(x + 2, 74, 196, 136, [22, 22, 22]);
        fill(x + 16, 92, 80, 6, [150, 150, 150]);
        fill(x + 16, 112, 60, 28, figure);
        fill(x + 16, 156, 150, 6, [110, 110, 110]);
        fill(x + 16, 170, 110, 6, [110, 110, 110]);
    }
    picture
}

/// A made-up file c shares in the a–c chat: its name, its extension, its sharing link's kind (`v` a video, `b` a
/// document) and id, and for a video the colours its made-up thumbnail is drawn in.
struct MadeUpFile {
    name: &'static str,
    extension: &'static str,
    kind: char,
    share: &'static str,
    frame: Option<([u8; 3], [u8; 3])>,
}

const MADE_UP_FILES: [MadeUpFile; 3] = [
    MadeUpFile {
        name: "06-made-up-rehearsal-multicam-edit-v2.mp4",
        extension: "mp4",
        kind: 'v',
        share: "MadeUpShareLinkMulticam0001",
        frame: Some(([24, 120, 40], [170, 230, 120])),
    },
    MadeUpFile {
        name: "04-made-up-rehearsal-three-cameras.mp4",
        extension: "mp4",
        kind: 'v',
        share: "MadeUpShareLinkCameras0002",
        frame: Some(([14, 18, 32], [60, 90, 200])),
    },
    MadeUpFile {
        name: "made-up-stage-plan.pdf",
        extension: "pdf",
        kind: 'b',
        share: "MadeUpShareLinkStagePlan0003",
        frame: None,
    },
];

/// A file's entry in `properties.files`, in the shape `tests/fixtures/chatsvc/files.json` has (made up).
fn file_entry(file: &MadeUpFile, at: usize) -> serde_json::Value {
    let site = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
    let object = format!("{site}Documents/Microsoft Teams Chat Files/{}", file.name);
    serde_json::json!({
        "itemid": format!("made-up-item-{at}"),
        "fileName": file.name,
        "fileType": file.extension,
        "fileInfo": {
            "fileUrl": object,
            "siteUrl": site,
            "shareUrl": share_url(file),
        },
        "@type": "http://schema.skype.com/File",
        "version": 2,
        "objectUrl": object,
        "type": file.extension,
        "title": file.name,
        "state": "active",
        "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
    })
}

fn share_url(file: &MadeUpFile) -> String {
    format!(
        "https://contoso-my.sharepoint.com/:{}:/g/personal/made_up_user_contoso_test/{}",
        file.kind, file.share
    )
}

/// c's message in the a–c chat as the one Teams shows with two videos: lines broken by `<br>` and a line feed each,
/// then two videos and a document. Graph's thumbnail list and each video's made-up picture are served by the mock (the
/// picture's link on the media host, as Graph hands one out).
fn seed_files(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let html = "<p>Made-up lab note: a first line long enough to wrap across the chat view a couple of times over, \
                (with a made-up aside)<br>\n<br>\nThe list below:<br>\n1. The first made-up item, which runs on a \
                little.<br>\n2. The second made-up item.<br>\n3. The third made-up item.\
                <br>\n<br>\nThe stage plan is attached too</p>";
    let id = mock.post_as(LAB_C, AC_CHAT, html).ok_or("c's post refused")?;
    let entries: Vec<serde_json::Value> = (0..)
        .zip(&MADE_UP_FILES)
        .map(|(at, file)| file_entry(file, at))
        .collect();
    let files = serde_json::Value::Array(entries).to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AC_CHAT, &client_message_id, |message| {
        message["properties"]["files"] = serde_json::json!(files);
    });
    if !changed {
        return Err("c's post isn't in the mock".into());
    }
    for (at, file) in MADE_UP_FILES.iter().enumerate() {
        let Some((dark, light)) = file.frame else { continue };
        let read = teams_proto::FileThumbnails::of(&share_url(file)).ok_or("a made-up link no read names")?;
        let token = read.share();
        let picture = format!("/transform/thumbnail/made-up-{at}");
        let list = serde_json::json!({"value": [{"id": "0", "large": {
            "height": 450, "width": 800,
            "url": format!("https://westus1-mediap.svc.ms{picture}?provider=spo&docid=made-up"),
        }}]});
        runtime.block_on(mock.serve_bytes(
            &format!("/v1.0/shares/{token}/driveItem/thumbnails"),
            "application/json",
            list.to_string().into_bytes(),
        ));
        let mut png = Vec::new();
        made_up_frame(dark, light).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        runtime.block_on(mock.serve_bytes(&picture, "image/png", png));
        // the file's item with its download link on the sharer's site, and the file there, read by ranges
        let download = "/personal/made_up_user_contoso_test/_layouts/15/download.aspx";
        let item = serde_json::json!({
            "id": format!("made-up-item-{at}"),
            "name": file.name,
            "size": MADE_UP_VIDEO.len(),
            "@microsoft.graph.downloadUrl": format!(
                "https://contoso-my.sharepoint.com{download}?UniqueId=made-up-{at}&tempauth=made-up"
            ),
        });
        runtime.block_on(mock.serve_bytes(
            &format!("/v1.0/shares/{token}/driveItem"),
            "application/json",
            item.to_string().into_bytes(),
        ));
        runtime.block_on(mock.serve_ranged(download, "video/mp4", MADE_UP_VIDEO.to_vec()));
    }
    Ok(())
}

/// The made-up video every video card plays: 2 s of `FFmpeg`'s test pattern and a tone (teams-media's test input).
const MADE_UP_VIDEO: &[u8] = include_bytes!("../../../../teams-media/tests/fixtures/media/made-up-test-pattern.mp4");

/// A made-up video frame, 800 by 450: a gradient from `dark` to `light` with a few bars, a figure and a caption strip.
fn made_up_frame(dark: [u8; 3], light: [u8; 3]) -> image::RgbaImage {
    let (width, height) = (800_u32, 450_u32);
    image::RgbaImage::from_fn(width, height, |x, y| {
        let mix = |from: u8, to: u8| {
            let t = (x + y) * 255 / (width + height);
            u8::try_from((u32::from(from) * (255 - t) + u32::from(to) * t) / 255).unwrap_or(to)
        };
        let figure = (300..380).contains(&x) && y > 120 && y < 420;
        let caption = y > 410 && x < 260;
        let bars = y > 330 && x % 90 < 30;
        if caption {
            image::Rgba([20, 20, 20, 255])
        } else if figure {
            image::Rgba([10, 10, 10, 255])
        } else if bars {
            image::Rgba([mix(dark[0], 40), mix(dark[1], 40), mix(dark[2], 40), 255])
        } else {
            image::Rgba([
                mix(dark[0], light[0]),
                mix(dark[1], light[1]),
                mix(dark[2], light[2]),
                255,
            ])
        }
    })
}

/// The made-up picture b sent and the outsider forwarded, its object on AMS.
const FORWARDED_OBJECT: &str = "0-eus-d9-0000000000000000000000000000f0d0";

/// The made-up link in the note b sent and the outsider forwarded.
const FORWARDED_LINK: &str = "https://example.com/made-up-assets";

/// Two of b's messages in the a–b–c chat, forwarded to a by the outsider in their 1:1 chat as Teams forwards them
/// (`docs/protocol/chat.md`, "Forwarded messages"): a note with a link, and a picture (a made-up phone screenshot,
/// served by the mock as AMS would) over a line.
fn seed_forwards(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut png = Vec::new();
    made_up_phone().write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{FORWARDED_OBJECT}/views/imgo"), "image/png", png));
    let link = FORWARDED_LINK;
    let note = format!(
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p>Hey, here is the made-up link. It was mentioned \
         in the made-up review, as a stand-in for a forwarded note that runs over a line</p>\r\n<p><a href=\
         \"{link}\" title=\"{link}\">{link}</a></p>\r\n</blockquote>"
    );
    let picture = format!(
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p><img src=\"https://us-api.asm.skype.com/v1/\
         objects/{FORWARDED_OBJECT}/views/imgo\" itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" \
         width=\"127.40112994350282\" height=\"250\" alt=\"image\" id=\"x_x_{FORWARDED_OBJECT}\" \
         itemid=\"{FORWARDED_OBJECT}\"></p>\r\n\r\n\r\n<p>The made-up picture is here, forwarded with a note that runs a \
         little long too</p>\r\n</blockquote>"
    );
    for (html, sent, original) in [
        (note, "2026-01-01T02:49:12.345Z", 1_767_235_752_345_u64),
        (picture, "2026-01-01T17:36:47.526Z", 1_767_289_007_526),
    ] {
        let id = mock
            .post_as(OUTSIDER, AX_CHAT, &html)
            .ok_or("the outsider's forward refused")?;
        let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
        let changed = mock.change_message(AX_CHAT, &client_message_id, |message| {
            message["properties"]["forwardTemplateId"] = serde_json::json!("basic_forward_message_template");
            message["properties"]["originalMessageContext"] = serde_json::json!({
                "originalSender": LAB_B, "originalSentTime": sent, "messageId": original,
                "originalThreadId": ABC_CHAT, "threadType": "chat", "messageType": "RichText/Html",
            });
        });
        if !changed {
            return Err("the outsider's forward isn't in the mock".into());
        }
    }
    Ok(())
}

/// A made-up phone screenshot, 254 by 500: a dark frame round a white screen with a title bar, two round badges and a
/// band of sea at the bottom.
fn made_up_phone() -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(254, 500, image::Rgba([250, 250, 250, 255]));
    let mut fill = |x0: u32, y0: u32, width: u32, height: u32, colour: [u8; 3]| {
        for y in y0..(y0 + height).min(500) {
            for x in x0..(x0 + width).min(254) {
                picture.put_pixel(x, y, image::Rgba([colour[0], colour[1], colour[2], 255]));
            }
        }
    };
    let frame = [24, 24, 28];
    fill(0, 0, 254, 14, frame);
    fill(0, 486, 254, 14, frame);
    fill(0, 0, 14, 500, frame);
    fill(240, 0, 14, 500, frame);
    fill(40, 70, 174, 26, [30, 30, 30]);
    fill(118, 64, 18, 38, [240, 140, 30]);
    fill(44, 170, 70, 70, [230, 120, 150]);
    fill(140, 170, 70, 70, [120, 160, 230]);
    fill(94, 300, 66, 80, [200, 200, 210]);
    fill(14, 400, 226, 50, [90, 200, 210]);
    picture
}

/// The Workflows bot, as it posts its cards.
const WORKFLOWS_BOT: &str = "28:358f0194-6b0e-4dd3-af35-c24fe8a9ec87";

/// The "Lab alerts" chat `seed_cards` made, where the Workflows bot's card is.
static CARD_CHAT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// A `RichText/Media_Card` content carrying the Adaptive Card `card`, as the chat service gives a bot's card.
fn card_content(card: &serde_json::Value) -> SandboxResult<String> {
    use base64::Engine as _;
    let activity = serde_json::json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}],
    });
    let b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&activity)?);
    Ok(format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift>\
         <Description></Description></URIObject>"
    ))
}

/// The Workflows bot's cards (made up): the webhook template's post in the team's channel (a bold title, its text,
/// the separated footer naming who used the template with its "Get template" link), and a task card in a new "Lab
/// alerts" chat of a and b (an icon beside a subtle line, an emphasis container framed and rounded, facts, a
/// picture, a time function, a "View item" button with Teams' Open icon and a shown card's button).
fn seed_cards(mock: &ChatServiceMock) -> SandboxResult {
    let footer = serde_json::json!({"type": "Container", "separator": true, "items": [{
        "type": "TextBlock", "size": "small", "weight": "lighter", "wrap": true,
        "text": "Lab Aye used a Workflow template to send this card. [Get template](https://made-up.example.test/t)"
    }]});
    let post = serde_json::json!({
        "type": "AdaptiveCard", "version": "1.2",
        "body": [
            {"type": "TextBlock", "text": "Test: made-up webhook check", "weight": "bolder", "wrap": true},
            {"type": "TextBlock", "wrap": true,
             "text": "This is a made-up test message. Cards moved to Needs a Person on the lab board will be announced here."},
            footer,
        ]
    });
    mock.add_message(
        TEAM_CHANNEL,
        (WORKFLOWS_BOT, "Workflows"),
        "RichText/Media_Card",
        &card_content(&post)?,
    )
    .ok_or("the channel's card refused")?;
    let chat = mock.add_group("Lab alerts", &[LAB_A, LAB_B]).ok_or("no chat made")?;
    let task = serde_json::json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "ColumnSet", "columns": [
                {"type": "Column", "width": "auto", "items": [
                    {"type": "Image", "url": "https://made-up.example.test/icon.png",
                     "width": "16px", "height": "16px"}
                ]},
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "TextBlock", "text": "Task created from message", "isSubtle": true, "wrap": true}
                ]}
            ]},
            {"type": "Container", "style": "emphasis", "showBorder": true, "roundedCorners": true, "spacing": "small",
             "items": [
                {"type": "TextBlock", "text": "Lab Bee {{TIME(2026-01-01T12:00:00Z)}}", "wrap": true},
                {"type": "TextBlock", "text": "Lab build reminder", "maxLines": 2, "wrap": true, "spacing": "small"}
             ]},
            {"type": "FactSet", "facts": [
                {"title": "Task title", "value": "Lab build reminder"},
                {"title": "Plan", "value": "Made-up lab plan"}
            ]},
            {"type": "TextBlock", "size": "medium", "weight": "bolder", "text": "Workflow Created", "wrap": true},
            {"type": "TextBlock", "wrap": true,
             "text": "1. Go to the message you want to run the workflow on.\r\n2. Select **More options** (**...**)."}
        ],
        "actions": [
            {"type": "Action.OpenUrl", "title": "View item", "iconUrl": "icon:Open", "url": "https://made-up.example.test/item"},
            {"type": "Action.ShowCard", "title": "Details", "card": {"type": "AdaptiveCard", "body": [
                {"type": "TextBlock", "text": "Made-up details", "wrap": true}
            ]}}
        ]
    });
    mock.add_message(
        &chat,
        (WORKFLOWS_BOT, "Workflows"),
        "RichText/Media_Card",
        &card_content(&task)?,
    )
    .ok_or("the chat's card refused")?;
    CARD_CHAT.set(chat).map_err(|_| "the card chat was set twice")?;
    Ok(())
}

/// a's own message in the a–b chat, before b's, with a's heart on it: a's hover bar, and a's most used reactions
/// (the heart, and the eyes on b's) first in every hover bar.
fn seed_own(mock: &ChatServiceMock) -> SandboxResult {
    let id = mock
        .post_as(LAB_A, AB_CHAT, "<p>Sure, I can look at it this afternoon</p>")
        .ok_or("a's post refused")?;
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["emotions"] = serde_json::json!([{
            "key": "heart",
            "users": [{"mri": LAB_A, "time": 1_767_268_860_000_i64, "value": "1767268860000"}],
        }]);
    });
    if !changed {
        return Err("a's post isn't in the mock".into());
    }
    Ok(())
}

/// b's message to a in the a–b chat as the one Teams shows: a mention of a and one of c in its text, and a's own
/// reaction on it.
fn seed_mentions(mock: &ChatServiceMock) -> SandboxResult {
    let mention = |id: u32, name: &str| {
        format!("<span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"{id}\">{name}</span>")
    };
    let html = format!(
        "<p>{} can you check the made-up report with {}? It is a long message written only for this tour, so that it \
         wraps onto a second line and a third. Nothing in it is about anyone, it just fills the bubble with enough \
         words to show how a long message reads with two mentions at the start of it, ending right about here</p>",
        mention(0, "Lab A"),
        mention(1, "Lab C")
    );
    let id = mock.post_as(LAB_B, AB_CHAT, &html).ok_or("b's post refused")?;
    let entry = |id: u32, mri: &str, name: &str| {
        serde_json::json!({
            "@type": "http://schema.skype.com/Mention", "itemid": id, "mri": mri, "mentionType": "person",
            "displayName": name,
        })
    };
    let mentions = serde_json::json!([entry(0, LAB_A, "Lab A"), entry(1, LAB_C, "Lab C")]).to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["mentions"] = serde_json::json!(mentions);
        message["properties"]["emotions"] = serde_json::json!([{
            "key": "1f440_eyes",
            "users": [
                {"mri": LAB_B, "time": 1_767_268_790_000_i64, "value": "1767268790000"},
                {"mri": LAB_C, "time": 1_767_268_795_000_i64, "value": "1767268795000"},
                {"mri": LAB_A, "time": 1_767_268_800_000_i64, "value": "1767268800000"},
            ],
        }]);
    });
    if !changed {
        return Err("b's post isn't in the mock".into());
    }
    Ok(())
}

fn child_window() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let (lab, engine) = MockLab::start()?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                // the sandbox's private bus, with its fake notification server and tray watcher
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                walk(window, cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(lab);
    Ok(())
}
