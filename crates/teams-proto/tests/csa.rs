//! CSA reads against synthetic mocks: captured team responses, configured service bases, queries and channel message reads.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::chat::{ConversationKind, Message};
use teams_proto::{
    BuildError, CSA_AUDIENCE, Client, Credentials, CsaAnswerError, CsaTeams, Endpoint, HttpError, HttpTransport, Read,
    Refusal, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const CSA_BASE: &str = "https://teams.microsoft.com/api/csa/amer";
const TEAMS_PATH: &str = "/api/csa/amer/api/v3/teams/users/me";
const FIXTURE: &str = include_str!("../../../tests/fixtures/csa/teams.json");
const FOLDERS: &str = include_str!("../../../tests/fixtures/csa/folders.json");
const TEAM: &str = "19:channel0001@thread.tacv2";
const CHANNEL: &str = "19:channel0002@thread.tacv2";

/// The fixture's response body, its placeholders filled in.
fn fixture() -> Value {
    let text = FIXTURE
        .replace("<region>", "amer")
        .replace("<guid:a>", "0000000a-0000-4000-8000-00000000000a")
        .replace("<guid:tenant>", "00000000-0000-4000-8000-0000000000ff")
        .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
        .replace("<token:1>", "made-up-sync-token");
    serde_json::from_str(&text).unwrap()
}

/// Hands out placeholder tokens and records the audiences asked for.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Secret {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        Secret::new("placeholder-csa-token")
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }
}

/// A client on the mock, with the CSA base when `csa`.
async fn setup(csa: bool) -> (MockServer, Arc<FakeTokens>, Client<HttpTransport<FakeTokens>>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let mut transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    if csa {
        transport = transport.with_csa(CSA_BASE).unwrap();
    }
    (server, tokens, Client::new(transport))
}

#[tokio::test]
async fn the_teams_read_is_a_get_of_its_probed_shape_with_the_csa_token_and_its_teams_are_read() {
    let (server, tokens, client) = setup(true).await;
    let fixture = fixture();
    Mock::given(method("GET"))
        .and(path(TEAMS_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture["response"]["body"].to_string(), "application/json"),
        )
        .mount(&server)
        .await;
    let answer = client.read(&CsaTeams).await.unwrap();
    let read = CsaTeams::read_answer(answer.body()).unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "GET");
    assert!(request.body.is_empty());
    assert_eq!(request.url.path(), TEAMS_PATH);
    assert_eq!(
        request.url.query(),
        fixture["request"]["query"].as_str(),
        "the web client's query, exactly"
    );
    let mut names: Vec<&str> = request.headers.keys().map(wiremock::http::HeaderName::as_str).collect();
    names.sort_unstable();
    assert!(
        names.contains(&"accept") && names.contains(&"authorization"),
        "{names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("x-ms-") || *name == "behavioroverride"),
        "{names:?}"
    );
    assert_eq!(
        request.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer placeholder-csa-token"
    );
    assert_eq!(tokens.audiences(), [CSA_AUDIENCE]);

    // the deleted team and the deleted channel left out; the general channel first
    assert_eq!(read.skipped, 0);
    let teams: Vec<(&str, &str, bool)> = read
        .teams
        .iter()
        .map(|team| (team.id.as_str(), team.name.as_str(), team.archived))
        .collect();
    assert_eq!(
        teams,
        [
            (TEAM, "A made-up team", false),
            ("19:channel0004@thread.tacv2", "A made-up archived team", true)
        ]
    );
    let channels: Vec<(&str, &str, bool)> = read.teams[0]
        .channels
        .iter()
        .map(|channel| (channel.id.as_str(), channel.name.as_str(), channel.general))
        .collect();
    assert_eq!(
        channels,
        [(TEAM, "General", true), (CHANNEL, "A made-up channel", false)]
    );
    assert_eq!(read.teams[1].channels.len(), 1);
    assert!(read.teams[1].channels[0].general);
    // the fixture's favourites folder holds the team's standard channel
    assert_eq!(read.favorites, Some(vec![CHANNEL.to_owned()]));
    let shown = format!("{read:?}");
    assert!(!shown.contains("made-up team") && !shown.contains("General"), "{shown}");
}

#[test]
fn a_teams_answer_is_read_only_in_its_probed_shape() {
    for body in [&b"[]"[..], b"{}", b"{\"teams\": {}}", b"not json"] {
        assert_eq!(CsaTeams::read_answer(body), Err(CsaAnswerError::Malformed));
    }
    let answer = json!({"metadata": {"isPartialData": false}, "teams": [
        {"id": TEAM, "displayName": "A", "channels": [
            {"id": TEAM, "displayName": "General"},
            {"id": CHANNEL},
            {"displayName": "no id"},
            {"id": "19:x@thread.tacv2", "displayName": "B", "isGeneral": "true"},
        ]},
        {"id": "19:y@thread.tacv2"},
        {"id": 7, "displayName": "C"},
    ]});
    let read = CsaTeams::read_answer(answer.to_string().as_bytes()).unwrap();
    assert_eq!(read.skipped, 4);
    assert_eq!(read.teams.len(), 1);
    let general: Vec<bool> = read.teams[0].channels.iter().map(|channel| channel.general).collect();
    // a channel is general by its id or by a boolean `isGeneral`, never by a string
    assert_eq!(general, [true, false]);
}

#[test]
fn a_teams_answer_that_doesn_t_say_it_is_whole_is_refused() {
    let teams = json!([{"id": TEAM, "displayName": "A", "channels": [{"id": TEAM, "displayName": "General"}]}]);
    let whole = json!({"teams": teams, "metadata": {"isPartialData": false, "hasMoreChats": true}});
    assert_eq!(
        CsaTeams::read_answer(whole.to_string().as_bytes()).unwrap().teams.len(),
        1
    );
    // partial only in its chats (an account with many: its newest chats, the rest the chat service's): its teams whole
    let more_chats = json!({"teams": teams, "metadata": {"isPartialData": true, "hasMoreChats": true}});
    assert_eq!(
        CsaTeams::read_answer(more_chats.to_string().as_bytes())
            .unwrap()
            .teams
            .len(),
        1
    );
    // partial without more chats to say why, or not saying: never taken for the whole list the store replaces its own
    // with
    for metadata in [
        json!({"isPartialData": true}),
        json!({"isPartialData": true, "hasMoreChats": false}),
        json!({"isPartialData": true, "hasMoreChats": "true"}),
        json!({"isPartialData": "true", "hasMoreChats": true}),
        json!({"isPartialData": "false"}),
        json!({"isPartialData": 0}),
        json!({"isPartialData": null}),
        json!({}),
        json!(null),
        json!([]),
    ] {
        let answer = json!({"teams": teams, "metadata": metadata});
        assert_eq!(
            CsaTeams::read_answer(answer.to_string().as_bytes()),
            Err(CsaAnswerError::Malformed),
            "{metadata}"
        );
    }
    let bare = json!({"teams": teams});
    assert_eq!(
        CsaTeams::read_answer(bare.to_string().as_bytes()),
        Err(CsaAnswerError::Malformed)
    );
}

#[tokio::test]
async fn a_csa_read_without_a_csa_base_is_refused_before_anything_is_sent() {
    let (server, tokens, client) = setup(false).await;
    assert!(!client.transport().has_csa());
    match client.read(&CsaTeams).await {
        Err(HttpError::Refused(Refusal::NoCsa)) => {}
        other => panic!("{other:?}"),
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.audiences().is_empty());
}

/// A read of CSA in a shape the probe didn't show: any type may implement `Read`.
struct OtherCsaRead(Endpoint);

impl Read for OtherCsaRead {
    fn service(&self) -> Service {
        Service::Csa
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }
}

#[tokio::test]
async fn any_other_csa_read_goes_to_the_csa_base_with_its_query() {
    let (server, _tokens, client) = setup(true).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&server)
        .await;
    let others = [
        Endpoint::new(["api", "v3", "teams", "users", "me", "updates"]),
        Endpoint::new(["api", "v1", "teams", "users", "me", "pinnedChannels"]),
        Endpoint::new(["api", "v1", "containers", CHANNEL, "posts"]).with_query("pageSize", "20"),
    ];
    for endpoint in others {
        client.read(&OtherCsaRead(endpoint)).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let urls: Vec<String> = requests
        .iter()
        .map(|request| format!("{}?{}", request.url.path(), request.url.query().unwrap_or_default()))
        .collect();
    assert_eq!(
        urls,
        [
            "/api/csa/amer/api/v3/teams/users/me/updates?".to_owned(),
            "/api/csa/amer/api/v1/teams/users/me/pinnedChannels?".to_owned(),
            format!("/api/csa/amer/api/v1/containers/{CHANNEL}/posts?pageSize=20"),
        ]
    );
    assert!(requests.iter().all(|request| request.method.as_str() == "GET"));
}

#[test]
fn only_a_csa_base_of_the_gtm_form_is_taken() {
    assert!(HttpTransport::<FakeTokens>::accepts_csa_base(CSA_BASE));
    assert!(HttpTransport::<FakeTokens>::accepts_csa_base(
        "https://teams.cloud.microsoft/api/csa/emea/"
    ));
    for base in [
        "https://chatsvcagg.teams.microsoft.com/api/csa/amer",
        "http://teams.microsoft.com/api/csa/amer",
        "https://teams.microsoft.com:8443/api/csa/amer",
        "https://teams.microsoft.com/api/csa/AMER",
        "https://teams.microsoft.com/api/csa/amer/api",
        "https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/csa/amer?x=1",
        " https://teams.microsoft.com/api/csa/amer",
        "https://user@teams.microsoft.com/api/csa/amer",
    ] {
        assert!(!HttpTransport::<FakeTokens>::accepts_csa_base(base), "{base}");
    }
    let transport = HttpTransport::new(Arc::new(FakeTokens::default()), CHAT_BASE).unwrap();
    assert!(matches!(
        transport.with_csa("https://elsewhere.example/api/csa/amer"),
        Err(BuildError::CsaBase)
    ));
}

/// A channel message as the probe saw one: a `conversationLink` ending in `;messageid=<post>`.
fn channel_message(id: &str, link: &str) -> Message {
    serde_json::from_value(json!({
        "id": id,
        "type": "Message",
        "messagetype": "RichText/Html",
        "conversationid": CHANNEL,
        "conversationLink": link,
        "version": "1",
        "content": "<p>made up</p>",
    }))
    .unwrap()
}

#[test]
fn a_channel_message_names_its_post_and_a_channel_thread_is_a_channel() {
    let link = |rest: &str| format!("{CHAT_BASE}/v1/users/ME/conversations/{rest}");
    let post = channel_message("1700000000001", &link(&format!("{CHANNEL};messageid=1700000000001")));
    assert_eq!(post.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    let reply = channel_message("1700000000002", &link(&format!("{CHANNEL};messageid=1700000000001")));
    assert_eq!(reply.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    // percent-encoded as one segment
    let encoded = channel_message(
        "1700000000003",
        &link("19%3Achannel0002%40thread.tacv2%3Bmessageid%3D1700000000001"),
    );
    assert_eq!(encoded.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    for other in [
        link(CHANNEL),
        link(&format!("{TEAM};messageid=1700000000001")),
        link(&format!("{CHANNEL};messageid=")),
        link(&format!("{CHANNEL};messageid=17x")),
        "not a link".to_owned(),
    ] {
        assert_eq!(channel_message("1", &other).reply_root(CHANNEL), None, "{other}");
    }
    assert_eq!(ConversationKind::of(CHANNEL, Some("topic")), ConversationKind::Channel);
    assert_eq!(ConversationKind::of(TEAM, Some("space")), ConversationKind::Channel);
    assert!(ConversationKind::of(CHANNEL, Some("topic")).is_read_only());
}

/// A whole teams answer around `folders` (the `conversationFolders` object).
fn with_folders(folders: &Value) -> Vec<u8> {
    json!({"metadata": {"isPartialData": false}, "teams": [], "conversationFolders": folders})
        .to_string()
        .into_bytes()
}

/// The folders fixture's `conversationFolders`, its placeholders filled in.
fn folders() -> Value {
    let text = FOLDERS
        .replace("<guid:a>", "0000000a-0000-4000-8000-00000000000a")
        .replace("<guid:tenant>", "00000000-0000-4000-8000-0000000000ff");
    serde_json::from_str::<Value>(&text).unwrap()["conversationFolders"].clone()
}

/// The folders fixture's favourites, in its order: the account's notes, a group chat and a team's channel.
fn favourites() -> Vec<String> {
    [
        "48:notes",
        "19:favorite0001@thread.v2",
        "19:favoritechannel0001@thread.tacv2",
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn the_favourites_are_the_favorites_folder_s_conversations_in_its_order() {
    let read = CsaTeams::read_answer(&with_folders(&folders())).unwrap();
    // a channel as a chat, the quick views' slice and the other folders left out
    assert_eq!(read.favorites, Some(favourites()));
}

#[test]
fn favourites_are_read_only_in_their_captured_shape() {
    let whole = folders();
    let changed = |change: &dyn Fn(&mut Value)| {
        let mut folders = whole.clone();
        change(&mut folders);
        CsaTeams::read_answer(&with_folders(&folders)).unwrap().favorites
    };
    // partial, unsaid or not a list: not known
    assert_eq!(changed(&|f| f["isPartialResponse"] = json!(true)), None);
    assert_eq!(changed(&|f| f["isPartialResponse"] = json!("false")), None);
    assert_eq!(
        changed(&|f| f.as_object_mut().unwrap().retain(|k, _| k != "isPartialResponse")),
        None
    );
    assert_eq!(changed(&|f| f["conversationFolders"] = json!({})), None);
    // an item without a string id
    assert_eq!(
        changed(&|f| f["conversationFolders"][0]["conversationFolderItems"][1]["conversationId"] = json!(7)),
        None
    );
    // two favourites folders: which one is Teams' isn't known
    assert_eq!(
        changed(&|f| {
            let favourites = f["conversationFolders"][0].clone();
            f["conversationFolders"].as_array_mut().unwrap().push(favourites);
        }),
        None
    );
    // a deleted favourites folder, or none: none, known
    assert_eq!(
        changed(&|f| f["conversationFolders"][0]["isDeleted"] = json!(true)),
        Some(Vec::new())
    );
    assert_eq!(
        changed(&|f| {
            f["conversationFolders"].as_array_mut().unwrap().remove(0);
        }),
        Some(Vec::new())
    );
    // a conversation listed twice keeps its first place
    assert_eq!(
        changed(&|f| {
            let first = f["conversationFolders"][0]["conversationFolderItems"][0].clone();
            f["conversationFolders"][0]["conversationFolderItems"]
                .as_array_mut()
                .unwrap()
                .push(first);
        }),
        Some(favourites())
    );
    // no folders at all: not known
    let bare = json!({"metadata": {"isPartialData": false}, "teams": []}).to_string();
    assert_eq!(CsaTeams::read_answer(bare.as_bytes()).unwrap().favorites, None);
}

/// One team entry of an answer, as `CsaTeams` reads it.
fn read_team(team: &serde_json::Value) -> teams_proto::Team {
    let answer = serde_json::json!({
        "teams": [team],
        "metadata": { "isPartialData": false },
    });
    let mut read = CsaTeams::read_answer(answer.to_string().as_bytes()).unwrap();
    read.teams.remove(0)
}

#[test]
fn who_may_add_a_channel_is_read_as_the_web_client_decides() {
    let team = |role: &str, archived: bool, topic: bool, private: bool, guest_topic: bool| {
        read_team(&serde_json::json!({
            "id": "19:team@thread.tacv2",
            "displayName": "A team",
            "memberRoleString": role,
            "isArchived": archived,
            "teamSettings": { "createTopic": topic, "isPrivateChannelCreationEnabled": private },
            "teamGuestSettings": { "createTopic": guest_topic },
        }))
    };
    let may = |team: teams_proto::Team| (team.owner, team.can_add_channel, team.can_add_private_channel);
    assert_eq!(may(team("admin", false, false, false, false)), (true, true, true));
    assert_eq!(may(team("user", false, true, true, false)), (false, true, true));
    assert_eq!(may(team("User", false, true, false, false)), (false, true, false));
    assert_eq!(may(team("user", false, false, true, false)), (false, false, false));
    assert_eq!(may(team("guest", false, false, false, true)), (false, true, false));
    assert_eq!(
        may(team("admin", true, true, true, true)),
        (true, false, false),
        "archived"
    );
    assert_eq!(may(team("unheard-of", false, true, true, true)), (false, false, false));
}

#[test]
fn a_team_s_group_and_mail_and_a_channel_s_type_are_read() {
    let team = read_team(&serde_json::json!({
        "id": "19:team@thread.tacv2",
        "displayName": "A team",
        "smtpAddress": "team@example.invalid",
        "teamSiteInformation": { "groupId": "made-up-group", "sharepointSiteUrl": "https://made-up.example.invalid/sites/team" },
        "channels": [
            { "id": "19:a@thread.tacv2", "displayName": "a", "channelType": 0 },
            { "id": "19:b@thread.tacv2", "displayName": "b", "channelType": 1 },
            { "id": "19:c@thread.tacv2", "displayName": "c", "channelType": 2 },
            { "id": "19:d@thread.tacv2", "displayName": "d", "channelType": 9 },
            { "id": "19:e@thread.tacv2", "displayName": "e" },
        ],
    }));
    assert_eq!(team.group_id.as_deref(), Some("made-up-group"));
    assert_eq!(team.mailbox.as_deref(), Some("team@example.invalid"));
    assert_eq!(
        team.about.site.as_deref(),
        Some("https://made-up.example.invalid/sites/team")
    );
    let kinds: Vec<teams_proto::ChannelType> = team.channels.iter().map(|channel| channel.kind).collect();
    assert_eq!(
        kinds,
        [
            teams_proto::ChannelType::Standard,
            teams_proto::ChannelType::Private,
            teams_proto::ChannelType::Shared,
            teams_proto::ChannelType::Unknown,
            teams_proto::ChannelType::Unknown,
        ]
    );
}

#[test]
fn a_channel_s_running_meeting_is_read_from_its_active_meetups() {
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticMeetup1?i=10-0-0-1&e=1";
    let meetup = |status: &str, url: &str| {
        json!({
            "messageId": "1700000000000", "conversationUrl": url, "conversationId": "made-up",
            "groupCallInitiator": "8:orgid:00000000-0000-4000-8000-00000000000a", "status": status,
            "tenantId": "00000000-0000-4000-8000-0000000000d0", "organizerId": "00000000-0000-4000-8000-00000000000a",
            "callMeetingType": -1, "conversationType": "adHocMeeting"
        })
    };
    let answer = |meetups: Value| {
        json!({"teams": [{"id": TEAM, "displayName": "a", "channels": [
            {"id": TEAM, "displayName": "General", "activeMeetups": meetups},
            {"id": CHANNEL, "displayName": "other"},
        ]}]})
        .to_string()
        .into_bytes()
    };
    let found = CsaTeams::meetup_in(&answer(json!([meetup("Active", conversation)])), TEAM).unwrap();
    assert_eq!(found.message_id, "1700000000000");
    assert_eq!(found.conversation.as_str(), conversation);
    assert_eq!(found.meeting.organizer_id, "00000000-0000-4000-8000-00000000000a");
    assert_eq!(found.meeting.tenant_id, "00000000-0000-4000-8000-0000000000d0");
    // none in another channel, none ended, none at a link off the calling service, none in an answer that doesn't read
    let running = answer(json!([meetup("Active", conversation)]));
    assert_eq!(CsaTeams::meetup_in(&running, CHANNEL), None);
    assert_eq!(
        CsaTeams::meetup_in(&answer(json!([meetup("Ended", conversation)])), TEAM),
        None
    );
    let elsewhere = answer(json!([meetup("Active", "https://example.invalid/conv/x")]));
    assert_eq!(CsaTeams::meetup_in(&elsewhere, TEAM), None);
    assert_eq!(CsaTeams::meetup_in(b"not json", TEAM), None);
}

/// A channel's call post (`Event/Call`), synthetic in the shape a channel's Meet now posted (2026-11-03).
fn call_post(id: &str, kind: &str, call: &str, conversation: &str) -> Message {
    let meeting = json!({"@type": "http://schema.skype.com/ScheduledMeetingCreated", "meetingtitle": null,
        "tenantId": "00000000-0000-4000-8000-0000000000d0",
        "organizerId": "00000000-0000-4000-8000-00000000000a"})
    .to_string();
    serde_json::from_value(json!({
        "id": id,
        "messagetype": "Event/Call",
        "content": format!("<partlist alt =\"\"></partlist><callEventType>{kind}</callEventType>\n<conversationUrl>{conversation}</conversationUrl>\n<callId>{call}</callId>\n"),
        "properties": {"meeting": meeting},
    }))
    .unwrap()
}

#[test]
fn a_channel_s_running_meeting_is_read_from_its_posts_when_csa_names_none() {
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticMeetup2?i=10-0-0-1&amp;e=1";
    let started = call_post("1700000000002", "callStarted", "call-2", conversation);
    let found = teams_proto::ChannelMeetup::of_posts(std::slice::from_ref(&started)).unwrap();
    assert_eq!(found.message_id, "1700000000002");
    assert_eq!(found.conversation.as_str(), conversation.replace("&amp;", "&"));
    assert_eq!(found.meeting.organizer_id, "00000000-0000-4000-8000-00000000000a");
    // newest first: a call a newer post ended runs no more; an older one still running is found
    let ended = call_post("1700000000003", "callEnded", "call-2", conversation);
    assert_eq!(
        teams_proto::ChannelMeetup::of_posts(&[ended.clone(), started.clone()]),
        None
    );
    let older = call_post("1700000000001", "callStarted", "call-1", conversation);
    let found = teams_proto::ChannelMeetup::of_posts(&[ended, started, older]).unwrap();
    assert_eq!(found.message_id, "1700000000001");
    // a link off the calling service, or no organizer, names none
    let elsewhere = call_post(
        "1700000000004",
        "callStarted",
        "call-4",
        "https://example.invalid/conv/x",
    );
    assert_eq!(teams_proto::ChannelMeetup::of_posts(&[elsewhere]), None);
    let mut nobody = call_post("1700000000005", "callStarted", "call-5", conversation);
    nobody.properties.clear();
    assert_eq!(teams_proto::ChannelMeetup::of_posts(&[nobody]), None);
}

#[test]
fn a_group_chat_s_running_call_is_where_its_newest_unended_start_says() {
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticHuddle?i=10-0-0-1&amp;e=1";
    let event = |kind: &str, call: &str| {
        format!(
            "<partlist type=\"started\" alt=\"\"></partlist><conversationType>\"huddleGroupCall\"</conversationType>\
             <callEventType>{kind}</callEventType><conversationUrl>{conversation}</conversationUrl><callId>{call}</callId>"
        )
    };
    let (started, ended, older) = (
        event("callStarted", "call-2"),
        event("callEnded", "call-2"),
        event("callStarted", "call-1"),
    );
    let found = teams_proto::running_call([("Event/Call", started.as_str())]).unwrap();
    assert_eq!(found.as_str(), conversation.replace("&amp;", "&"));
    // newest first: a text message between changes nothing; an ended call runs no more, an older start still does
    let text = ("RichText/Html", "<p>hi</p>");
    assert!(teams_proto::running_call([text, ("Event/Call", started.as_str())]).is_some());
    assert_eq!(
        teams_proto::running_call([("Event/Call", ended.as_str()), ("Event/Call", started.as_str())]),
        None
    );
    let found = teams_proto::running_call([
        ("Event/Call", ended.as_str()),
        ("Event/Call", started.as_str()),
        ("Event/Call", older.as_str()),
    ]);
    assert!(found.is_some());
    // an end naming no call ends whatever ran; a start with no link, or one off the calling service, names none
    let bare_end = "<ended/><partlist alt=\"\" count=\"0\"></partlist><callEventType>callEnded</callEventType>";
    assert_eq!(
        teams_proto::running_call([("Event/Call", bare_end), ("Event/Call", started.as_str())]),
        None
    );
    assert_eq!(
        teams_proto::running_call([("Event/Call", "<partlist alt =\"\"></partlist>")]),
        None
    );
    let elsewhere = event("callStarted", "call-3").replace(conversation, "https://example.invalid/conv/x");
    assert_eq!(teams_proto::running_call([("Event/Call", elsewhere.as_str())]), None);
    assert_eq!(teams_proto::running_call([text]), None);
}
