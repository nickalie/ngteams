//! QA of what a bot hears when people come and go: a private team's join request (asked, approved, declined), a
//! team's code, a private channel's people, several people added to a group chat at once, and a bot endpoint that is
//! down at the change and comes back later. Names are synthetic.

use std::time::Duration;

use ngteams_server::bots::{SENT, deliveries};
use serde_json::Value;
use teams_core::ChannelDraft;
use teams_core::engine::{Action, ChatHistory};
use teams_proto::{MemberUpdate, Mri, NewChannelKind, TeamWrite};
use teams_store::OutboxState;

use super::installs::{BOT, Shop, add_to_team, create_chat, told_of_team};
use super::member_updates::{chat_app, person, quiet, team_update, told};
use crate::support::backends;
use crate::teams::{Seat, new_channel};

/// The ids of the people in `field` of `activity`, sorted.
fn ids(activity: &Value, field: &str) -> Vec<String> {
    let mut ids: Vec<String> = activity[field]
        .as_array()
        .map(|people| {
            people
                .iter()
                .map(|one| one["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

async fn write_ok(seat: &Seat, write: TeamWrite) {
    let written = seat.engine.team_write(write, None).await;
    assert!(written.is_ok(), "{written:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn qa_people_let_into_a_private_team_reach_its_bot_but_its_private_channels_people_do_not() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let cora = Seat::new(server, "cora@own.test", "Cora Lovelace").await;
        let dan = Seat::new(server, "dan@own.test", "Dan Lovelace").await;
        let eve = Seat::new(server, "eve@own.test", "Eve Lovelace").await;
        let (team, group) = shop.ada.make_team("Polling crew", false).await;
        add_to_team(&shop.ada, &team).await;
        told_of_team(&shop.bot, &team, &group).await;
        assert!(quiet(&shop.bot).await, "the bot heard of its own seat twice");

        // a request puts nobody on the team: only the owner's answer does
        let join = TeamWrite::Join { group: group.clone() };
        write_ok(&cora, join.clone()).await;
        write_ok(&dan, join).await;
        assert!(quiet(&shop.bot).await, "a join request told the bot");
        let answer = |seat: &Seat, accept| TeamWrite::AnswerJoinRequest {
            group: group.clone(),
            member: Mri::new(&seat.mri),
            accept,
        };
        write_ok(&shop.ada, answer(&cora, true)).await;
        let got = told(&shop.bot, &team, &shop.ada, ("membersAdded", person(&cora))).await;
        assert_eq!(got["channelData"]["eventType"], "teamMemberAdded", "{got}");
        write_ok(&shop.ada, answer(&dan, false)).await;
        assert!(quiet(&shop.bot).await, "a declined join request told the bot");

        // the person who joins by the team's code is the one who adds them
        write_ok(&shop.ada, TeamWrite::ResetCode { team: team.clone() }).await;
        let code = shop.ada.engine.team_code(team.as_str()).await.unwrap().unwrap();
        write_ok(&eve, TeamWrite::JoinByCode { code }).await;
        told(&shop.bot, &team, &eve, ("membersAdded", person(&eve))).await;

        let draft = ChannelDraft {
            channel: new_channel(&team, &group, "Secret", NewChannelKind::Private),
            show_by_default: false,
        };
        let made = shop.ada.engine.create_channel(draft).await.unwrap();
        let channel_group = made.group.unwrap();
        let failed = shop
            .ada
            .engine
            .add_channel_members(group.as_str(), channel_group.as_str(), vec![cora.mri.clone()])
            .await
            .unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        assert!(quiet(&shop.bot).await, "a private channel's people told the bot");

        // a leave also takes Cora out of the private channel: the bot hears one removal
        write_ok(&cora, team_update(&team, &group, &cora, MemberUpdate::Left)).await;
        let got = told(&shop.bot, &team, &cora, ("membersRemoved", person(&cora))).await;
        assert_eq!(got["channelData"]["eventType"], "teamMemberRemoved", "{got}");
        assert!(quiet(&shop.bot).await, "one leave told the bot more than once");
        for seat in [cora, dan, eve] {
            seat.stop();
        }
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn qa_a_bot_that_is_down_when_people_are_added_hears_of_them_all_when_it_is_back() {
    for backend in backends() {
        let shop = Shop::open_with(backend, |config| config.bots.retry_delays = vec![2, 2]).await;
        let server = &shop.registry.server;
        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let cora = Seat::new(server, "cora@own.test", "Cora Lovelace").await;
        let dan = Seat::new(server, "dan@own.test", "Dan Lovelace").await;
        let eve = Seat::new(server, "eve@own.test", "Eve Lovelace").await;
        let chat = create_chat(&shop.ada, &[&bea, &eve], Some("Polls room")).await;
        chat_app(&shop.ada, &chat, true).await;
        assert_eq!(shop.bot.next_of("installationUpdate").await.activity["action"], "add");
        let own = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(own["membersAdded"][0]["id"], format!("28:{BOT}"), "{own}");
        assert!(quiet(&shop.bot).await, "the bot heard of its own seat twice");

        shop.bot.answer(503);
        let row = shop
            .ada
            .engine
            .write(Action::AddMembers {
                conversation: chat.clone(),
                members: vec![Mri::new(&cora.mri), Mri::new(&dan.mri)],
                history: ChatHistory::None,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let refused = shop.bot.next_of("conversationUpdate").await.activity;
        shop.bot.answer(200);
        let got = shop
            .bot
            .next_of_within("conversationUpdate", Duration::from_secs(15))
            .await
            .activity;
        assert_eq!(got, refused, "the retry is the same activity");
        let mut both = vec![format!("29:{}", cora.id), format!("29:{}", dan.id)];
        both.sort();
        assert_eq!(ids(&got, "membersAdded"), both, "{got}");
        assert!(got["membersRemoved"].is_null(), "{got}");
        assert_eq!(got["from"]["id"], format!("29:{}", shop.ada.id), "{got}");
        assert_eq!(got["conversation"]["id"], chat, "{got}");
        assert!(
            quiet(&shop.bot).await,
            "one add of two people told the bot more than once"
        );
        let rows = deliveries(&server.app.db, BOT).await.unwrap();
        let last = rows.last().unwrap();
        assert_eq!((last.state.as_str(), last.attempts), (SENT, 2), "{rows:?}");
        for seat in [bea, cora, dan, eve] {
            seat.stop();
        }
        shop.stop();
    }
}
