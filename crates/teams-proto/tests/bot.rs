//! Bot invokes (`teams_proto::bot`): each kind's body against its synthetic fixture (`tests/fixtures/bots/`), and
//! each answer read from its fixture.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use serde_json::{Map, Value, json};
use teams_proto::Mri;
use teams_proto::bot::{
    BotInvoke, CardInvoke, CommandContext, DialogSize, InvokeAnswer, InvokeKind, MessagePayload, PayloadSender,
};
use teams_proto::chat::card::{ActionKind, InputKind, Kind, Via, adaptive_card};

const INVOKES: &str = include_str!("../../../tests/fixtures/bots/invokes.json");
const ANSWERS: &str = include_str!("../../../tests/fixtures/bots/answers.json");
const BOT: &str = "28:00000000-0000-4000-8000-0000000000b0";
const APP: &str = "00000000-0000-4000-8000-0000000000a0";
const AB_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const GROUP: &str = "19:00000000000000000000000000000abc@thread.v2";

/// The fixture `name` of `source`, its `<guid:…>` placeholders filled with this test's made-up ids.
fn fixture(source: &str, name: &str) -> Value {
    let mut text = source.to_owned();
    for (placeholder, id) in [
        ("<guid:app>", APP),
        ("<guid:bot>", &BOT[3..]),
        ("<guid:a>", "00000000-0000-4000-8000-00000000000a"),
        ("<guid:b>", "00000000-0000-4000-8000-00000000000b"),
        ("<guid:flow>", "00000000-0000-4000-8000-0000000000f0"),
    ] {
        text = text.replace(placeholder, id);
    }
    let all: Value = serde_json::from_str(&text).unwrap();
    all.get(name).cloned().unwrap_or_else(|| panic!("no fixture {name}"))
}

fn invoke(name: &str, kind: InvokeKind, conversation: &str) -> Value {
    BotInvoke::new(Mri::new(BOT), APP, name, kind)
        .unwrap()
        .in_conversation(conversation)
        .body_without_token()
}

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("not an object")
    };
    map
}

#[test]
fn seen_message_extension_invokes_match_their_fixtures() {
    let fetch_commands = invoke(
        "Lab A",
        InvokeKind::FetchCommands {
            context: CommandContext::Message,
        },
        "19:channel0001@thread.tacv2",
    );
    assert_eq!(fetch_commands, fixture(INVOKES, "fetchCommands"));

    let post = MessagePayload {
        id: "1700000000001".to_owned(),
        reply_to: Some("1700000000000".to_owned()),
        created: "2026-01-02T03:04:05.006Z".to_owned(),
        from: PayloadSender::App {
            id: "00000000-0000-4000-8000-0000000000b0".to_owned(),
            name: "Synthetic Bot".to_owned(),
        },
        html: "<p>Synthetic post</p>".to_owned(),
        link: teams_proto::MessageRun::link_to("19:channel0001@thread.tacv2", "1700000000001", Some("1700000000000")),
    };
    let fetch_task = invoke(
        "Lab A",
        InvokeKind::FetchTask {
            command: "synthetic-command-1".to_owned(),
            context: CommandContext::Message,
            message: Some(post),
        },
        "19:channel0001@thread.tacv2;messageid=1700000000000",
    );
    assert_eq!(fetch_task, fixture(INVOKES, "fetchTask"));

    let message = MessagePayload {
        id: "1700000000002".to_owned(),
        reply_to: None,
        created: "2026-01-02T03:04:05.006Z".to_owned(),
        from: PayloadSender::User {
            id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            name: "Lab B".to_owned(),
        },
        html: "<p>Synthetic message</p>".to_owned(),
        link: teams_proto::MessageRun::link_to(AB_CHAT, "1700000000002", None),
    };
    let data = object(json!({
        "hostEvent": "RunFlow",
        "flowDetails": {"data": {"flowId": "00000000-0000-4000-8000-0000000000f0"}},
        "remindDate": "2026-01-03",
        "remindTime": "08:00",
    }));
    let submit_action = invoke(
        "Lab A",
        InvokeKind::SubmitAction {
            command: "synthetic-command-1".to_owned(),
            context: CommandContext::Message,
            data,
            message: Some(message),
        },
        AB_CHAT,
    );
    assert_eq!(submit_action, fixture(INVOKES, "submitAction"));
}

#[test]
fn documented_invokes_match_their_fixtures() {
    let query = InvokeKind::Query {
        command: "synthetic-search".to_owned(),
        parameters: vec![("searchText".to_owned(), "lab".to_owned())],
        skip: 0,
        count: 25,
    };
    assert_eq!(invoke("Lab A", query, GROUP), fixture(INVOKES, "query"));
    let select = InvokeKind::SelectItem {
        value: json!({"itemId": "synthetic-item-3"}),
    };
    assert_eq!(invoke("Lab A", select, GROUP), fixture(INVOKES, "selectItem"));

    let execute = InvokeKind::Execute(CardInvoke {
        message: Some("1700000000003".to_owned()),
        id: Some("approve".to_owned()),
        verb: Some("approve".to_owned()),
        data: json!({"request": "synthetic-request-1", "comment": "Looks fine"}),
        automatic: false,
        text: None,
    });
    assert_eq!(invoke("Lab B", execute, AB_CHAT), fixture(INVOKES, "execute"));

    let task_fetch = InvokeKind::TaskFetch(CardInvoke {
        message: Some("1700000000004".to_owned()),
        data: json!({"msteams": {"type": "task/fetch"}, "form": "synthetic"}),
        ..CardInvoke::default()
    });
    assert_eq!(invoke("Lab A", task_fetch, GROUP), fixture(INVOKES, "taskFetch"));
    let task_submit = InvokeKind::TaskSubmit(CardInvoke {
        data: json!({"form": "synthetic", "title": "A made-up title"}),
        ..CardInvoke::default()
    });
    assert_eq!(invoke("Lab A", task_submit, GROUP), fixture(INVOKES, "taskSubmit"));

    let bot_chat = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000b0@unq.gbl.spaces";
    let submit = InvokeKind::Submit(CardInvoke {
        message: Some("1700000000005".to_owned()),
        data: json!({"choice": "yes", "note": "A made-up note"}),
        ..CardInvoke::default()
    });
    assert_eq!(invoke("Lab A", submit, bot_chat), fixture(INVOKES, "submit"));
    let verify = InvokeKind::VerifyState {
        code: "402917".to_owned(),
    };
    assert_eq!(invoke("Lab A", verify, bot_chat), fixture(INVOKES, "verifyState"));
}

fn answer(name: &str) -> InvokeAnswer {
    InvokeAnswer::read(200, fixture(ANSWERS, name).to_string().as_bytes())
}

#[test]
fn seen_answers_read() {
    let InvokeAnswer::Commands(commands) = answer("commands") else {
        panic!("commands")
    };
    assert_eq!(commands.len(), 2);
    assert!(commands[0].action && commands[0].fetch_task);
    assert_eq!(commands[0].width, Some(DialogSize::Pixels(600)));
    assert_eq!(
        commands[0].icon.as_deref(),
        Some("https://example.invalid/icons/follow-up.png")
    );
    assert!(!commands[1].action);
    assert_eq!(answer("noCommands"), InvokeAnswer::Commands(Vec::new()));
    assert_eq!(InvokeAnswer::read(200, b""), InvokeAnswer::Done);

    let InvokeAnswer::Dialog(dialog) = answer("dialog") else {
        panic!("a dialog")
    };
    assert_eq!(dialog.width, Some(DialogSize::Pixels(600)));
    let card = adaptive_card(dialog.card.as_ref().unwrap());
    let inputs: Vec<_> = card
        .body
        .iter()
        .filter_map(|element| match &element.kind {
            Kind::Input(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(inputs.len(), 3);
    assert!(matches!(inputs[0].kind, InputKind::Date { .. }) && inputs[0].required);
    assert_eq!(inputs[0].error_message.as_deref(), Some("Please pick a date"));
    assert!(matches!(
        inputs[2].kind,
        InputKind::Text {
            multiline: true,
            max_length: Some(500),
            ..
        }
    ));
}

/// An app not yet in the chat asks to be added first; its Continue sends the data back as the card gives it.
#[test]
fn the_just_in_time_install_dialog_keeps_its_data() {
    let InvokeAnswer::Dialog(install) = answer("installDialog") else {
        panic!("the install dialog")
    };
    let card = adaptive_card(install.card.as_ref().unwrap());
    let ActionKind::Submit(continue_button) = &card.actions[0].kind else {
        panic!("its Continue")
    };
    assert_eq!(continue_button.via, Via::Submit);
    assert_eq!(
        continue_button.data.as_ref().unwrap()["msteams"]["justInTimeInstall"],
        true,
        "the data goes back as the card gives it"
    );
}

#[test]
fn documented_answers_read() {
    let InvokeAnswer::Results { grid, attachments } = answer("results") else {
        panic!("results")
    };
    assert!(!grid);
    assert_eq!(attachments.len(), 1);
    assert_eq!(answer("message"), InvokeAnswer::Message("Nothing found.".to_owned()));
    assert_eq!(
        answer("taskMessage"),
        InvokeAnswer::Message("Thanks, saved.".to_owned())
    );
    let InvokeAnswer::Card(card) = answer("card") else {
        panic!("a card")
    };
    assert_eq!(adaptive_card(&card).body.len(), 1);
    assert_eq!(
        answer("executeMessage"),
        InvokeAnswer::Message("Your answer was saved.".to_owned())
    );
    assert_eq!(
        answer("error"),
        InvokeAnswer::Error {
            code: Some("BadRequest".to_owned()),
            message: "This request was already answered.".to_owned()
        }
    );
    assert!(
        matches!(answer("signIn"), InvokeAnswer::SignIn(value) if value["connectionName"] == "synthetic-connection")
    );
}

mod transport {
    use std::future::{self, Future};
    use std::sync::{Arc, Mutex, PoisonError};

    use teams_auth::secrets::Secret;
    use teams_auth::token::{SPACES_RESOURCE, TokenError};
    use teams_mock::{ABC_CHAT, BotReply, CHAT_BASE, ChatServiceMock, LAB_A, bot_answer};
    use teams_proto::bot::{BotInvoke, CommandContext, InvokeAnswer, InvokeKind};
    use teams_proto::{Cloud, Credentials, HttpError, HttpTransport, Mri, Transport};

    use super::{APP, BOT};

    /// The mock's chat token, and a made-up Teams service token for that audience; renewals asked are kept.
    struct Tokens {
        chat: String,
        rejected: Mutex<Vec<String>>,
    }

    impl Credentials for Tokens {
        fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            let token = if audience == SPACES_RESOURCE {
                "synthetic-spaces-token".to_owned()
            } else {
                self.chat.clone()
            };
            future::ready(Ok(Secret::new(token)))
        }

        fn bearer_rejected(
            &self,
            audience: &str,
            _: &Secret,
        ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            self.rejected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(audience.to_owned());
            self.bearer(audience)
        }
    }

    fn transport(mock: &ChatServiceMock, cloud: &Cloud) -> (HttpTransport<Tokens>, Arc<Tokens>) {
        let tokens = Arc::new(Tokens {
            chat: mock.token(LAB_A).unwrap(),
            rejected: Mutex::new(Vec::new()),
        });
        let base = cloud.service_url(CHAT_BASE, "/api/chatsvc/amer").unwrap();
        let transport = HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&tokens),
            base.as_str(),
            cloud.clone(),
            &mock.origin(),
        )
        .unwrap()
        .for_account(Mri::new(LAB_A));
        (transport, tokens)
    }

    fn fetch() -> BotInvoke {
        BotInvoke::new(
            Mri::new(BOT),
            APP,
            "Lab A",
            InvokeKind::FetchCommands {
                context: CommandContext::Message,
            },
        )
        .unwrap()
        .in_conversation(ABC_CHAT)
    }

    #[tokio::test]
    async fn a_microsoft_body_carries_the_teams_token_and_an_own_body_none() {
        let mock = ChatServiceMock::start().await.unwrap();
        mock.script_bot(
            BOT,
            "Synthetic Bot",
            &[(
                "composeExtension/fetchCommands",
                BotReply::answer(bot_answer("commands").unwrap()),
            )],
        );
        let (http, _) = transport(&mock, &Cloud::Microsoft);
        let answer = http.deliver_bot_invoke(fetch()).await.unwrap().unwrap();
        assert!(matches!(
            InvokeAnswer::read(answer.status(), answer.body()),
            InvokeAnswer::Commands(_)
        ));
        assert_eq!(mock.bot_invokes()[0].body["userAadToken"], "synthetic-spaces-token");

        let cloud = Cloud::own("https://chat.example:8443").unwrap();
        let own = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
        own.script_bot(
            BOT,
            "Synthetic Bot",
            &[("composeExtension/fetchCommands", BotReply::done())],
        );
        let (http, _) = transport(&own, &cloud);
        let answer = http.deliver_bot_invoke(fetch()).await.unwrap().unwrap();
        assert_eq!(InvokeAnswer::read(answer.status(), answer.body()), InvokeAnswer::Done);
        assert!(own.bot_invokes()[0].body.get("userAadToken").is_none());
    }

    #[tokio::test]
    async fn read_only_refuses_and_a_401_renews_the_chat_token_without_sending_again() {
        let mock = ChatServiceMock::start().await.unwrap();
        let (http, _) = transport(&mock, &Cloud::Microsoft);
        let refused = http.read_only().deliver_bot_invoke(fetch()).await.unwrap();
        assert!(matches!(refused, Err(HttpError::Refused(_))));
        assert!(mock.bot_invokes().is_empty());

        let (http, tokens) = transport(&mock, &Cloud::Microsoft);
        mock.refuse_next("POST", "/invoke", 401).await;
        let answer = http.deliver_bot_invoke(fetch()).await.unwrap().unwrap();
        assert_eq!(answer.status(), 401);
        assert_eq!(
            *tokens.rejected.lock().unwrap(),
            vec![teams_proto::IC3_AUDIENCE.to_owned()]
        );
        let sent = mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.url.path().ends_with("/invoke"))
            .count();
        assert_eq!(sent, 1);
    }
}
