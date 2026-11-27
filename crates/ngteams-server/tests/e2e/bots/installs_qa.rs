//! QA of task 231 on each backend, as own-server people and an administrator see it: a team app hears the standard
//! channels made before its add and never a private channel; a bot's answer in a group chat shows to the people in
//! it, and an app added again is heard again; the store shows an app only to the people who may use it, and only
//! while it is on. Apps, bots and names are synthetic.

use reqwest::Method;
use serde_json::json;
use teams_proto::TeamWrite;

use super::endpoint::{Connector, encode};
use super::installs::{
    APP, BOT, Shop, add_to_team, create_chat, mention, polls, reaches, say, told_of_removal,
};
use crate::support::backends;
use crate::teams::{Seat, new_channel};

/// `seat` makes the private channel `name` on `team`; its thread.
async fn make_private(seat: &Seat, team: &str, group: &str, name: &str) -> String {
    let draft = teams_core::ChannelDraft {
        channel: new_channel(team, group, name, teams_proto::NewChannelKind::Private),
        show_by_default: false,
    };
    seat.engine.create_channel(draft).await.unwrap().thread.unwrap()
}

/// The ids of the channels the bot reads on `team` through its connector.
async fn listed_channels(connector: &Connector, team: &str) -> Vec<String> {
    let (status, listed) = connector
        .call(Method::GET, &format!("v3/teams/{}/conversations", encode(team)), None)
        .await;
    assert_eq!(status, 200, "{listed}");
    listed["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The ids of the apps in `seat`'s store.
async fn store_ids(seat: &Seat) -> Vec<String> {
    seat.engine.app_listing().await.unwrap().sections.ids()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_team_app_hears_the_standard_channels_made_before_it_and_never_a_private_one() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let bea = Seat::new(&shop.registry.server, "bea@own.test", "Bea Lovelace").await;
        let (team, group) = shop.ada.make_team("Polling crew", false).await;
        shop.ada.add(&team, &group, &[&bea]).await;
        let plans = shop.ada.make_channel(&team, &group, "Plans", true).await;
        let secret = make_private(&shop.ada, &team, &group, "Secret").await;
        add_to_team(&shop.ada, &team).await;
        assert_eq!(
            shop.bot.next_of("installationUpdate").await.activity["action"],
            "add"
        );
        let later_secret = make_private(&shop.ada, &team, &group, "Later secret").await;

        // a standard channel made before the add
        let post = mention(&bea, &plans, "plans?").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(
            message["conversation"]["id"],
            format!("{plans};messageid={post}"),
            "{message}"
        );
        assert_eq!(message["channelData"]["channel"]["id"], plans);
        assert_eq!(message["channelData"]["team"]["id"], team);
        let connector = shop.connector(message["serviceUrl"].as_str().unwrap()).await;
        let conversation = message["conversation"]["id"].as_str().unwrap().to_owned();
        let (status, answer) = connector.send(&conversation, Connector::text("planned")).await;
        assert_eq!(status, 200, "{answer}");
        let answered = answer["id"].as_str().unwrap().to_owned();
        bea.engine.sync_conversation(plans.clone()).await.unwrap();
        let roots = bea.engine.reply_roots(plans.clone()).await.unwrap();
        assert!(roots.contains(&(answered, post.clone())), "{roots:?}");

        // private channels, made before or after the add, are not the bot's
        mention(&shop.ada, &secret, "secret plans?").await;
        mention(&shop.ada, &later_secret, "later secret?").await;
        assert!(!reaches(&shop.bot).await, "a mention in a private channel reached the bot");
        let (status, _) = connector.send(&secret, Connector::text("peek")).await;
        assert_eq!(status, 403);
        let (status, _) = connector.send(&later_secret, Connector::text("peek")).await;
        assert_eq!(status, 403);
        let channels = listed_channels(&connector, &team).await;
        assert!(channels.contains(&team) && channels.contains(&plans), "{channels:?}");
        assert!(
            !channels.contains(&secret) && !channels.contains(&later_secret),
            "{channels:?}"
        );

        // the removal takes it out of the earlier channel too, and out of the team's routes
        let removed = shop
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
        assert!(removed.is_ok(), "{removed:?}");
        told_of_removal(&shop.bot, &team, Some("teamMemberRemoved")).await;
        mention(&bea, &plans, "plans now?").await;
        assert!(!reaches(&shop.bot).await, "a removed bot got a mention in an earlier channel");
        let (status, _) = connector.send(&conversation, Connector::text("late")).await;
        assert_eq!(status, 403);
        let (status, _) = connector.send(&plans, Connector::text("late post")).await;
        assert_eq!(status, 403);
        let (status, _) = connector
            .call(Method::GET, &format!("v3/teams/{}", encode(&team)), None)
            .await;
        assert_eq!(status, 403);
        bea.stop();
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bots_answer_in_a_group_chat_shows_to_its_people_and_an_app_added_again_is_heard_again() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let bea = Seat::new(&shop.registry.server, "bea@own.test", "Bea Lovelace").await;
        let cora = Seat::new(&shop.registry.server, "cora@own.test", "Cora Lovelace").await;
        let chat = create_chat(&shop.ada, &[&bea, &cora], Some("Polls room")).await;
        let add = |definition| TeamWrite::AddChatApp {
            chat: chat.clone(),
            app: definition,
        };
        let written = shop.ada.engine.team_write(add(polls(&shop.ada).await), None).await;
        assert!(written.is_ok(), "{written:?}");
        assert_eq!(
            shop.bot.next_of("installationUpdate").await.activity["action"],
            "add"
        );

        let sent = mention(&bea, &chat, "start a poll").await;
        let message = shop.bot.next_of("message").await.activity;
        assert_eq!(message["id"], sent);
        let connector = shop.connector(message["serviceUrl"].as_str().unwrap()).await;
        let (status, answer) = connector.reply(&chat, &sent, Connector::text("poll started")).await;
        assert_eq!(status, 200, "{answer}");
        let answered = answer["id"].as_str().unwrap().to_owned();
        // each person in the chat reads the bot's answer as the bot's
        for seat in [&shop.ada, &cora] {
            seat.engine.sync_conversation(chat.clone()).await.unwrap();
            let read = seat
                .engine
                .message(chat.clone(), answered.clone())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(read.sender.as_deref(), Some(format!("28:{BOT}").as_str()));
            assert!(read.content.contains("poll started"), "{read:?}");
        }

        let removed = cora
            .engine
            .team_write(
                TeamWrite::RemoveChatApp {
                    chat: chat.clone(),
                    app: APP.into(),
                },
                None,
            )
            .await;
        assert!(removed.is_ok(), "{removed:?}");
        told_of_removal(&shop.bot, &chat, None).await;
        mention(&bea, &chat, "gone?").await;
        assert!(!reaches(&shop.bot).await, "a removed bot got a mention");

        // Bea adds it again: the bot is told, hears mentions and posts again
        let written = bea.engine.team_write(add(polls(&bea).await), None).await;
        assert!(written.is_ok(), "{written:?}");
        let got = shop.bot.next_of("installationUpdate").await.activity;
        assert_eq!((got["action"].as_str(), got["conversation"]["id"].as_str()), (Some("add"), Some(chat.as_str())));
        let members = shop.bot.next_of("conversationUpdate").await.activity;
        assert_eq!(members["membersAdded"][0]["id"], format!("28:{BOT}"), "{members}");
        let names = cora.engine.chat_apps(chat.clone()).await.unwrap();
        assert_eq!(names.iter().map(|app| app.app.id.as_str()).collect::<Vec<_>>(), [APP]);
        say(&cora, &chat, "no mention here").await;
        assert!(!reaches(&shop.bot).await, "a message without a mention reached the bot");
        mention(&cora, &chat, "back?").await;
        assert_eq!(
            shop.bot.next_of("message").await.activity["text"],
            "<at>Polls (synthetic)</at> back?"
        );
        let (status, _) = connector.send(&chat, Connector::text("back")).await;
        assert_eq!(status, 200);
        for seat in [bea, cora] {
            seat.stop();
        }
        shop.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_store_shows_an_app_only_to_its_people_and_only_while_it_is_on() {
    for backend in backends() {
        let shop = Shop::open(backend).await;
        let server = &shop.registry.server;
        let bea = Seat::new(server, "bea@own.test", "Bea Lovelace").await;
        let few = shop
            .registry
            .register("Few (synthetic)", "https://few.example.test/api")
            .await;
        let few = few["id"].as_str().unwrap().to_owned();
        let path = super::app_path(&few);
        let (status, _) = shop
            .registry
            .call(Method::PATCH, &path, Some(json!({"everyone": false, "users": [bea.id]})))
            .await;
        assert_eq!(status, 200);

        // only the listed person sees it, reads it and can add it
        let listed = store_ids(&bea).await;
        assert!(listed.contains(&few) && listed.contains(&APP.to_owned()), "{listed:?}");
        let listed = store_ids(&shop.ada).await;
        assert!(!listed.contains(&few) && listed.contains(&APP.to_owned()), "{listed:?}");
        assert!(shop.ada.engine.app_definition(few.clone()).await.unwrap().is_none());
        let definition = bea.engine.app_definition(few.clone()).await.unwrap().unwrap();
        let written = bea
            .engine
            .team_write(TeamWrite::AddPersonalApp { app: definition.clone() }, None)
            .await;
        assert!(written.is_ok(), "{written:?}");
        let mine = bea.engine.user_apps().await.unwrap();
        assert!(mine.iter().any(|app| app.app.id == few), "{mine:?}");

        // the administrator turns it off: it leaves the store, and an add is refused
        let (status, _) = shop
            .registry
            .call(Method::PATCH, &path, Some(json!({"enabled": false})))
            .await;
        assert_eq!(status, 200);
        assert!(!store_ids(&bea).await.contains(&few));
        // the engine keeps a definition for its run, so a new run of Bea's reads the server again
        let again = Seat::of(server, "bea@own.test", bea.id.clone()).await;
        assert!(again.engine.app_definition(few.clone()).await.unwrap().is_none());
        assert!(!store_ids(&again).await.contains(&few));
        again.stop();
        let refused = bea
            .engine
            .team_write(TeamWrite::AddPersonalApp { app: definition }, None)
            .await;
        assert!(
            crate::teams::refusal(refused).is_some_and(|code| code.contains("NotFound")),
            "an add of an app that is off was not refused as NotFound"
        );

        // on again, it is back for the listed person only
        let (status, _) = shop
            .registry
            .call(Method::PATCH, &path, Some(json!({"enabled": true})))
            .await;
        assert_eq!(status, 200);
        assert!(store_ids(&bea).await.contains(&few));
        assert!(!store_ids(&shop.ada).await.contains(&few));
        bea.stop();
        shop.stop();
    }
}
