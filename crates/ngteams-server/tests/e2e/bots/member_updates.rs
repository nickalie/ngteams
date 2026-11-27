//! A bot hears the people of its chats and teams change: someone added to or taken out of a group chat or a team
//! where its app is gives it `conversationUpdate` (`membersAdded`, `membersRemoved`), as the `msteams` channel sends
//! it; a chat or team without the bot, or a bot whose app was taken off, hears nothing. Names are synthetic.

use std::time::Duration;

use serde_json::{Value, json};
use teams_core::engine::{Action, ChatHistory};
use teams_proto::{MemberUpdate, Mri, TeamWrite};
use teams_store::OutboxState;

use super::endpoint::TestBot;
use super::installs::{APP, BOT, Shop, add_to_team, create_chat, polls, told_of_removal, told_of_team};
use crate::chatsvc::members::RemoveMember;
use crate::support::backends;
use crate::teams::Seat;

/// A person as the bot reads them in `membersAdded` or `membersRemoved`.
fn person(seat: &Seat) -> Value {
    json!([{"id": format!("29:{}", seat.id), "aadObjectId": seat.id}])
}

/// The next `conversationUpdate` the bot gets: `listed` added (or removed) by `by` in `conversation`.
async fn told(bot: &TestBot, conversation: &str, by: &Seat, (field, listed): (&str, Value)) -> Value {
    let got = bot.next_of("conversationUpdate").await.activity;
    assert_eq!(got[field], listed, "{got}");
    let other = if field == "membersAdded" { "membersRemoved" } else { "membersAdded" };
    assert!(got[other].is_null(), "{got}");
    assert_eq!(got["conversation"]["id"], conversation, "{got}");
    assert_eq!(got["from"]["id"], format!("29:{}", by.id), "{got}");
    assert_eq!(got["recipient"]["id"], format!("28:{BOT}"), "{got}");
    got
}

async fn quiet(bot: &TestBot) -> bool {
    !bot.gets_within("conversationUpdate", Duration::from_secs(3)).await
}

async fn add_to_chat(seat: &Seat, chat: &str, who: &Seat) {
    let row = seat
        .engine
        .write(Action::AddMembers {
            conversation: chat.into(),
            members: vec![Mri::new(&who.mri)],
            history: ChatHistory::None,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

async fn out_of_chat(seat: &Seat, chat: &str, who: &Seat) {
    let gone = seat
        .client
        .write(RemoveMember {
            chat: chat.to_owned(),
            member: who.mri.clone(),
        })
        .await
        .unwrap();
    assert_eq!(gone.status(), 200);
}

async fn chat_app(seat: &Seat, chat: &str, add: bool) {
    let write = if add {
        TeamWrite::AddChatApp {
            chat: chat.to_owned(),
            app: polls(seat).await,
        }
    } else {
        TeamWrite::RemoveChatApp {
            chat: chat.to_owned(),
            app: APP.into(),
        }
    };
    let written = seat.engine.team_write(write, None).await;
    assert!(written.is_ok(), "{written:?}");
}

fn team_update(team: &str, group: &str, member: &Seat, update: MemberUpdate) -> TeamWrite {
    TeamWrite::UpdateMember {
        team: team.into(),
        group: group.into(),
        member: Mri::new(&member.mri),
        update,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn people_added_to_and_taken_out_of_a_group_chat_reach_its_bot_until_its_app_is_removed() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let cora = Seat::new(server, "cora@own.test", "Cora Lovelace").await;
        let dan = Seat::new(server, "dan@own.test", "Dan Lovelace").await;
        let eve = Seat::new(server, "eve@own.test", "Eve Lovelace").await;
        // two others: a chat of two is 1:1, where no app goes
        let chat = create_chat(&shop.ada, &[&bea, &eve], Some("Polls room")).await;
        chat_app(&shop.ada, &chat, true).await;
        assert_eq!(shop.bot.next_of("installationUpdate").await.activity["action"], "add");
        let own = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(own["membersAdded"][0]["id"], format!("28:{BOT}"), "{own}");

        add_to_chat(&shop.ada, &chat, &cora).await;
        let got = told(&shop.bot, &chat, &shop.ada, ("membersAdded", person(&cora))).await;
        assert_eq!(got["conversation"]["conversationType"], "groupChat");
        assert_eq!(got["conversation"]["isGroup"], true);
        assert!(got["channelData"]["eventType"].is_null(), "{got}");
        out_of_chat(&bea, &chat, &cora).await;
        told(&shop.bot, &chat, &bea, ("membersRemoved", person(&cora))).await;
        // someone who leaves is the one who takes them out
        out_of_chat(&bea, &chat, &bea).await;
        told(&shop.bot, &chat, &bea, ("membersRemoved", person(&bea))).await;

        // a chat without the bot tells it nothing
        let other = create_chat(&shop.ada, &[&bea, &cora], None).await;
        add_to_chat(&shop.ada, &other, &dan).await;
        out_of_chat(&shop.ada, &other, &dan).await;
        assert!(quiet(&shop.bot).await, "a chat without the bot told it of its people");

        chat_app(&shop.ada, &chat, false).await;
        told_of_removal(&shop.bot, &chat, None).await;
        add_to_chat(&shop.ada, &chat, &dan).await;
        out_of_chat(&shop.ada, &chat, &dan).await;
        assert!(quiet(&shop.bot).await, "a removed bot heard of the chat's people");
        for seat in [bea, cora, dan, eve] {
            seat.stop();
        }
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_person_who_leaves_a_group_chat_still_reaches_its_bot() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let eve = Seat::new(server, "eve@own.test", "Eve Lovelace").await;
        let chat = create_chat(&shop.ada, &[&bea, &eve], Some("Polls room")).await;
        chat_app(&shop.ada, &chat, true).await;
        assert_eq!(shop.bot.next_of("installationUpdate").await.activity["action"], "add");
        shop.bot.next_of("conversationUpdate").await;

        for seat in [&eve, &bea, &shop.ada] {
            out_of_chat(seat, &chat, seat).await;
            told(&shop.bot, &chat, seat, ("membersRemoved", person(seat))).await;
        }
        bea.stop();
        eve.stop();
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn people_added_to_joining_and_leaving_a_team_reach_its_bot_in_the_teams_conversation() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let cora = Seat::new(server, "cora@own.test", "Cora Lovelace").await;
        let (team, group) = shop.ada.make_team("Polling crew", true).await;
        add_to_team(&shop.ada, &team).await;
        told_of_team(&shop.bot, &team, &group).await;

        shop.ada.add(&team, &group, &[&bea]).await;
        let got = told(&shop.bot, &team, &shop.ada, ("membersAdded", person(&bea))).await;
        assert_eq!(got["conversation"]["conversationType"], "channel");
        assert_eq!(got["channelData"]["eventType"], "teamMemberAdded", "{got}");
        assert_eq!(got["channelData"]["team"]["id"], team);
        assert_eq!(got["channelData"]["team"]["aadGroupId"], group);
        assert_eq!(got["channelData"]["channel"]["id"], team);
        // a role change is no member change
        let promoted = shop
            .ada
            .engine
            .team_write(team_update(&team, &group, &bea, MemberUpdate::Promoted), None)
            .await;
        assert!(promoted.is_ok(), "{promoted:?}");
        let joined = cora.engine.team_write(TeamWrite::Join { group: group.clone() }, None).await;
        assert!(joined.is_ok(), "{joined:?}");
        told(&shop.bot, &team, &cora, ("membersAdded", person(&cora))).await;

        let removed = bea
            .engine
            .team_write(team_update(&team, &group, &cora, MemberUpdate::Removed), None)
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        let got = told(&shop.bot, &team, &bea, ("membersRemoved", person(&cora))).await;
        assert_eq!(got["channelData"]["eventType"], "teamMemberRemoved", "{got}");
        let left = bea
            .engine
            .team_write(team_update(&team, &group, &bea, MemberUpdate::DemotedAndLeft), None)
            .await;
        assert!(left.is_ok(), "{left:?}");
        told(&shop.bot, &team, &bea, ("membersRemoved", person(&bea))).await;

        // a team without the bot tells it nothing
        let (bare, bare_group) = shop.ada.make_team("No bots here", false).await;
        shop.ada.add(&bare, &bare_group, &[&bea]).await;
        assert!(quiet(&shop.bot).await, "a team without the bot told it of its people");

        let off = shop
            .ada
            .engine
            .team_write(
                TeamWrite::RemoveApp {
                    team: team.clone(),
                    app: APP.into(),
                },
                None,
            )
            .await;
        assert!(off.is_ok(), "{off:?}");
        told_of_removal(&shop.bot, &team, Some("teamMemberRemoved")).await;
        shop.ada.add(&team, &group, &[&bea]).await;
        assert!(quiet(&shop.bot).await, "a removed bot heard of the team's people");
        bea.stop();
        cora.stop();
        shop.stop();
    }
}
