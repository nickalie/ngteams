//! The mock chat service against its fixtures: reads replay `tests/fixtures/chatsvc/`, writes read back as the
//! fixtures recorded and queue an event for every other member, the headers M1 Task 3 found are required, and the
//! native transport reaches it through its `test-support` mock origin. Its Trouter
//! (`tests/fixtures/trouter/`): the session's frames, the registrar, and deliveries only to a registered endpoint. Its
//! calling service takes the account's skypetoken alone (the calls themselves: `crates/teams-call/tests/mock.rs`).
//!
//! Every id and token here is the mock's made-up one, never a real one.

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHANNEL, CHAT_BASE, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B,
    LAB_C, NOTIFICATIONS, OUTSIDER, REGIONAL_HOST, TEAM_CHANNEL, TENANT,
};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, Endpoint, HttpTransport, Read, SendMessage, Service};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");
const TROUTER_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/trouter");
const CSA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/csa");
const CALL_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/calling/chat");
const ECHO_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/calling/echo");
const LOKI_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/loki");

fn oid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// A fixture with its placeholders filled in as the mock fills them.
fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name))?;
    let group = ABC_CHAT.trim_start_matches("19:").trim_end_matches("@thread.v2");
    let text = text
        .replace("<region>", "amer")
        .replace("<guid:a>", oid(LAB_A))
        .replace("<guid:b>", oid(LAB_B))
        .replace("<guid:c>", oid(LAB_C))
        .replace("<guid:tenant>", TENANT)
        .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
        .replace("<guid:context>", "00000000-0000-4000-8000-000000000003")
        .replace("<group>", group)
        .replace("<token:1>", "mock-fixture-1")
        .replace("<token:2>", "mock-fixture-2");
    Ok(serde_json::from_str(&text)?)
}

/// A chat id as the web client puts it in a path: one segment, `:` and `@` encoded.
fn segment(id: &str) -> String {
    id.replace(':', "%3A").replace('@', "%40")
}

/// The mock and a plain HTTP client for it, sending the web client's headers unless told otherwise.
struct Lab {
    mock: ChatServiceMock,
    http: reqwest::Client,
}

/// An answer: its status, `Location` and body text.
struct Answer {
    status: u16,
    location: Option<String>,
    text: String,
}

impl Answer {
    fn json(&self) -> TestResult<Value> {
        Ok(serde_json::from_str(&self.text)?)
    }
}

impl Lab {
    async fn start() -> TestResult<Self> {
        Ok(Self {
            mock: ChatServiceMock::start().await?,
            http: reqwest::Client::builder().no_proxy().build()?,
        })
    }

    /// `path` (with its query) under the chat base, on the mock's origin.
    fn url(&self, path: &str) -> String {
        format!("{}/api/chatsvc/amer/v1/{path}", self.mock.origin())
    }

    /// A link the mock handed out (a full URL under the chat base), on the mock's origin.
    fn link(&self, link: &str) -> String {
        link.replace("https://teams.microsoft.com", &self.mock.origin())
    }

    /// Sends as `who` with the web client's chat headers.
    async fn send(&self, who: &str, method: Method, path: &str, body: Option<&Value>) -> TestResult<Answer> {
        let token = format!("Bearer {}", self.mock.token(who).ok_or("no such account")?);
        let headers = [
            ("Authorization", token.as_str()),
            ("X-MS-Migration", "True"),
            ("BehaviorOverride", "redirectAs404"),
        ];
        self.send_raw(method, &self.url(path), &headers, body).await
    }

    async fn send_raw(
        &self,
        method: Method,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> TestResult<Answer> {
        let mut request = self.http.request(method, url).header("Accept", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let answer = request.send().await?;
        let status = answer.status().as_u16();
        let location = answer
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(Answer {
            status,
            location,
            text: answer.text().await?,
        })
    }

    /// A read as `who` that must succeed.
    async fn get(&self, who: &str, path: &str) -> TestResult<Value> {
        let answer = self.send(who, Method::GET, path, None).await?;
        assert_eq!(answer.status, 200, "GET {path}: {}", answer.text);
        answer.json()
    }

    /// A link the mock handed out, read as `who`.
    async fn follow(&self, who: &str, link: &str) -> TestResult<Value> {
        let token = format!("Bearer {}", self.mock.token(who).ok_or("no such account")?);
        let headers = [
            ("Authorization", token.as_str()),
            ("X-MS-Migration", "True"),
            ("BehaviorOverride", "redirectAs404"),
        ];
        let answer = self.send_raw(Method::GET, &self.link(link), &headers, None).await?;
        assert_eq!(answer.status, 200, "GET {link}: {}", answer.text);
        answer.json()
    }

    async fn list(&self, who: &str) -> TestResult<Value> {
        self.get(
            who,
            "users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All",
        )
        .await
    }

    /// `chat`'s messages as `who` reads them, newest first (the read around a time: every message).
    async fn messages(&self, who: &str, chat: &str) -> TestResult<Vec<Value>> {
        let path = format!(
            "users/ME/conversations/{}/messages/epochTimeStamp/1?pageSize=200&direction=BIDIRECTIONAL",
            segment(chat)
        );
        let page = self.get(who, &path).await?;
        Ok(page["messages"].as_array().cloned().unwrap_or_default())
    }

    async fn message(&self, who: &str, chat: &str, id: &str) -> TestResult<Value> {
        let messages = self.messages(who, chat).await?;
        messages
            .into_iter()
            .find(|message| message["id"] == id)
            .ok_or_else(|| format!("no message {id}").into())
    }

    /// Posts `body` to `chat` as `who`; returns the new message's id.
    async fn post(&self, who: &str, chat: &str, body: &Value) -> TestResult<String> {
        let path = format!("users/ME/conversations/{}/messages", segment(chat));
        let answer = self.send(who, Method::POST, &path, Some(body)).await?;
        assert_eq!(answer.status, 201, "{}", answer.text);
        Ok(answer.json()?["OriginalArrivalTime"].to_string())
    }
}

fn ids(list: &Value) -> Vec<String> {
    list["conversations"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn entry<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["conversations"].as_array()?.iter().find(|entry| entry["id"] == id)
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn kinds(events: &[ChatEvent]) -> Vec<EventKind> {
    events.iter().map(|event| event.kind).collect()
}

fn text_message(content: &str, client_id: &str) -> Value {
    json!({
        "content": content,
        "messagetype": "RichText/Html",
        "contenttype": "Text",
        "clientmessageid": client_id,
        "imdisplayname": "Lab A",
        "amsreferences": [],
        "properties": {"mentions": "[]", "files": "[]", "formatVariant": "TEAMS"},
    })
}

#[tokio::test]
async fn the_conversation_list_shows_each_member_their_chats_in_the_fixtures_shape() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("conversations.json")?;
    let recorded = &recorded["response"]["body"]["conversations"];
    let list = lab.list(LAB_A).await?;
    assert_eq!(
        ids(&list),
        [AB_CHAT, AC_CHAT, AX_CHAT, ABC_CHAT, CHANNEL, NOTIFICATIONS]
    );
    assert_eq!(ids(&lab.list(LAB_B).await?), [AB_CHAT, ABC_CHAT]);
    assert_eq!(ids(&lab.list(LAB_C).await?), [AC_CHAT, ABC_CHAT]);

    // the empty chat and the channel are their fixture entries as they are; the chat with messages keeps its keys
    assert_eq!(entry(&list, AC_CHAT), Some(&recorded[1]));
    assert_eq!(entry(&list, CHANNEL), Some(&recorded[2]));
    let ab = entry(&list, AB_CHAT).ok_or("no a-b chat")?;
    assert_eq!(keys(ab), keys(&recorded[0]));
    assert_eq!(keys(&ab["threadProperties"]), keys(&recorded[0]["threadProperties"]));
    assert_eq!(
        ab["properties"]["consumptionhorizon"],
        recorded[0]["properties"]["consumptionhorizon"]
    );
    assert_eq!(
        ab["lastMessage"]["id"], "1760000060000",
        "the deleted message is the newest"
    );
    assert!(ab["lastMessage"].get("properties").is_none());
    let b_list = lab.list(LAB_B).await?;
    let b_ab = entry(&b_list, AB_CHAT).ok_or("no a-b chat for b")?;
    assert!(
        b_ab["properties"].get("consumptionhorizon").is_none(),
        "a's horizon is a's own"
    );
    assert_eq!(b_ab["threadProperties"]["isCreator"], false);

    // a group chat's entry has no 1:1-only keys
    let abc = entry(&list, ABC_CHAT).ok_or("no group chat")?;
    assert_eq!(abc["threadProperties"]["threadType"], "chat");
    assert!(abc["threadProperties"].get("productThreadType").is_none());
    assert!(abc["threadProperties"].get("uniquerosterthread").is_none());

    let metadata = &list["_metadata"];
    assert_eq!(metadata["totalCount"], 6);
    assert_eq!(metadata["forwardLink"], "");
    let sync = metadata["syncState"].as_str().ok_or("no sync link")?;
    assert!(
        sync.starts_with(&format!("{CHAT_BASE}/v1/users/{LAB_A}/conversations?")),
        "{sync}"
    );
    Ok(())
}

#[tokio::test]
async fn the_conversation_lists_sync_link_gives_only_what_changed_since() -> TestResult {
    let lab = Lab::start().await?;
    let sync = lab.list(LAB_A).await?["_metadata"]["syncState"].clone();
    lab.post(LAB_B, AB_CHAT, &text_message("<p>new</p>", "1")).await?;
    let changed = lab.follow(LAB_A, sync.as_str().ok_or("no sync link")?).await?;
    assert_eq!(ids(&changed), [AB_CHAT]);
    let again = lab
        .follow(LAB_A, changed["_metadata"]["syncState"].as_str().ok_or("no sync link")?)
        .await?;
    assert!(ids(&again).is_empty());
    // the link names a's MRI: nobody else follows it
    let b_token = format!("Bearer {}", lab.mock.token(LAB_B).ok_or("no b")?);
    let headers = [
        ("Authorization", b_token.as_str()),
        ("X-MS-Migration", "True"),
        ("BehaviorOverride", "redirectAs404"),
    ];
    let link = lab.link(sync.as_str().ok_or("no sync link")?);
    assert_eq!(lab.send_raw(Method::GET, &link, &headers, None).await?.status, 403);
    Ok(())
}

#[tokio::test]
async fn a_paged_list_hands_out_the_newest_first_page_by_page_and_a_held_page_waits_for_its_release() -> TestResult {
    let lab = Lab::start().await?;
    let whole = ids(&lab.list(LAB_A).await?);
    let made = lab.mock.add_synthetic_chats(5);
    assert_eq!(made[0], teams_mock::synthetic_chat(0));
    lab.mock.page_conversation_list(3);
    let first = lab.list(LAB_A).await?;
    assert_eq!(ids(&first), made[..3]);
    assert!(
        first["_metadata"]["syncState"]
            .as_str()
            .is_some_and(|link| !link.is_empty())
    );
    let front = lab.mock.start_front()?;
    let token = format!("Bearer {}", lab.mock.token(LAB_A).ok_or("no a")?);
    let headers = [
        ("Authorization", token.as_str()),
        ("X-MS-Migration", "True"),
        ("BehaviorOverride", "redirectAs404"),
    ];
    let through_front = |link: &str| link.replace("https://teams.microsoft.com", front.origin());
    let later_page = |target: &str| target.contains("mock-page-");
    let mut seen = ids(&first);
    let mut forward = first["_metadata"]["forwardLink"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    while !forward.is_empty() {
        let hold = lab.mock.hold(later_page);
        let link = through_front(&forward);
        let (page, was_held) = tokio::join!(lab.send_raw(Method::GET, &link, &headers, None), async move {
            let entered =
                tokio::task::spawn_blocking(move || (hold.wait_entered(std::time::Duration::from_secs(5)), hold))
                    .await
                    .map_err(|error| error.to_string())?;
            Ok::<bool, String>(entered.0 && entered.1.release())
        });
        assert!(was_held?, "the page wasn't held");
        let page = page?;
        assert_eq!(page.status, 200, "{}", page.text);
        let page = page.json()?;
        seen.extend(ids(&page));
        forward = page["_metadata"]["forwardLink"].as_str().unwrap_or_default().to_owned();
    }
    let mut expected = whole;
    expected.extend(made);
    expected.sort();
    seen.sort();
    assert_eq!(seen, expected, "every chat once, over the pages");

    // a held page refused; a read the hold doesn't match goes on meanwhile
    let hold = lab.mock.hold(later_page);
    let link = through_front(first["_metadata"]["forwardLink"].as_str().ok_or("no forward link")?);
    let other = through_front(&format!(
        "{CHAT_BASE}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=2"
    ));
    let (refused, was_held) = tokio::join!(lab.send_raw(Method::GET, &link, &headers, None), async {
        let entered = tokio::task::spawn_blocking(move || (hold.wait_entered(std::time::Duration::from_secs(5)), hold))
            .await
            .map_err(|error| error.to_string())?;
        let meanwhile = lab
            .send_raw(Method::GET, &other, &headers, None)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<(bool, u16), String>((entered.0 && entered.1.refuse(503), meanwhile.status))
    });
    assert_eq!(was_held?, (true, 200));
    assert_eq!(refused?.status, 503);
    let again = lab.send_raw(Method::GET, &link, &headers, None).await?;
    assert_eq!(again.json()?["conversations"].as_array().map(Vec::len), Some(3));
    Ok(())
}

#[tokio::test]
async fn a_paged_list_synthetic_chat_edited_keeps_its_place_and_reads_give_the_new_text() -> TestResult {
    let lab = Lab::start().await?;
    let made = lab.mock.add_synthetic_chats(3);
    let before = lab.list(LAB_A).await?;
    assert!(lab.mock.edit_synthetic_message(1, "Edited 1"));
    assert!(!lab.mock.edit_synthetic_message(3, "no such chat"));
    let after = lab.list(LAB_A).await?;
    assert_eq!(ids(&after), ids(&before), "the edit moved no chat");
    let entry = after["conversations"]
        .as_array()
        .and_then(|chats| chats.iter().find(|chat| chat["id"] == made[1].as_str()))
        .ok_or("the edited chat isn't listed")?;
    assert_eq!(entry["lastMessage"]["content"], "<p>Edited 1</p>");
    let path = format!(
        "users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize=20",
        segment(&made[1])
    );
    let page = lab.get(LAB_A, &path).await?;
    let messages = page["messages"].as_array().ok_or("no messages")?;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], "<p>Edited 1</p>");
    assert!(lab.mock.take_events(LAB_A).is_empty(), "nobody is told of the edit");
    Ok(())
}

#[tokio::test]
async fn a_paged_list_synthetic_history_pages_back_and_a_new_message_keeps_the_chats_place() -> TestResult {
    let lab = Lab::start().await?;
    let made = lab.mock.add_synthetic_chats(3);
    lab.mock.page_conversation_list(2);
    let before = lab.list(LAB_A).await?;
    assert!(lab.mock.add_synthetic_history(1, 5));
    assert!(!lab.mock.add_synthetic_history(3, 5));
    let path = format!(
        "users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize=4",
        segment(&made[1])
    );
    let texts = |page: &Value| -> Vec<String> {
        page["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|message| message["content"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let first = lab.get(LAB_A, &path).await?;
    assert_eq!(
        texts(&first),
        [
            "<p>Synthetic message 0001</p>",
            "<p>History 0001-004</p>",
            "<p>History 0001-003</p>",
            "<p>History 0001-002</p>"
        ]
    );
    let newest_first_ids: Vec<u64> = first["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message["id"].as_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        newest_first_ids.windows(2).all(|pair| pair[0] > pair[1]),
        "ids grow with time: {newest_first_ids:?}"
    );
    let back = first["_metadata"]["backwardLink"].as_str().ok_or("no backward link")?;
    let older = lab.follow(LAB_A, back).await?;
    assert_eq!(texts(&older), ["<p>History 0001-001</p>", "<p>History 0001-000</p>"]);
    assert!(older["_metadata"]["backwardLink"].is_null(), "the history ends there");

    assert!(lab.mock.add_synthetic_message(1, "New 1"));
    assert!(!lab.mock.add_synthetic_message(3, "no such chat"));
    let after = lab.list(LAB_A).await?;
    assert_eq!(ids(&after), ids(&before), "neither moved a chat");
    let entry = after["conversations"]
        .as_array()
        .and_then(|chats| chats.iter().find(|chat| chat["id"] == made[1].as_str()))
        .ok_or("the chat isn't listed")?;
    assert_eq!(entry["lastMessage"]["content"], "<p>New 1</p>");
    assert_eq!(texts(&lab.get(LAB_A, &path).await?)[0], "<p>New 1</p>");
    assert!(lab.mock.take_events(LAB_A).is_empty(), "nobody is told");
    Ok(())
}

#[tokio::test]
async fn a_read_around_a_time_replays_the_message_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("messages.json")?;
    let path = format!(
        "users/ME/conversations/{}/messages/epochTimeStamp/1760000000000?pageSize=40&direction=BIDIRECTIONAL&includeMetadata=true&locationType=Primary",
        segment(AB_CHAT)
    );
    let page = lab.get(LAB_B, &path).await?;
    let mut expected: Vec<Value> = Vec::new();
    for read in ["sync", "around"] {
        expected.extend(
            recorded[read]["response"]["body"]["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }
    assert_eq!(page["messages"], Value::Array(expected));
    assert_eq!(page["tenantId"], TENANT);
    assert_eq!(page["_metadata"], recorded["around"]["response"]["body"]["_metadata"]);
    Ok(())
}

#[tokio::test]
async fn message_pages_go_back_through_backward_links_and_forward_through_sync_links() -> TestResult {
    let lab = Lab::start().await?;
    let path = format!(
        "users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize=2",
        segment(AB_CHAT)
    );
    let mut page = lab.get(LAB_B, &path).await?;
    let sync = page["_metadata"]["syncState"]
        .as_str()
        .ok_or("no sync link")?
        .to_owned();
    let mut sequences = Vec::new();
    loop {
        let messages = page["messages"].as_array().cloned().unwrap_or_default();
        sequences.push(
            messages
                .iter()
                .map(|message| message["sequenceId"].clone())
                .collect::<Vec<_>>(),
        );
        let Some(back) = page["_metadata"]["backwardLink"].as_str().map(str::to_owned) else {
            break;
        };
        page = lab.follow(LAB_B, &back).await?;
    }
    assert_eq!(
        sequences,
        [vec![json!(5), json!(4)], vec![json!(3), json!(2)], vec![json!(1)]]
    );

    // a edits its plain message: the sync link gives that message alone, as it now reads
    let edit = json!({"content": "<p>changed</p>", "messagetype": "RichText/Html"});
    let edit_path = format!("users/ME/conversations/{}/messages/1760000005000", segment(AB_CHAT));
    assert_eq!(lab.send(LAB_A, Method::PUT, &edit_path, Some(&edit)).await?.status, 200);
    let changed = lab.follow(LAB_B, &sync).await?;
    let changed = changed["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["content"], "<p>changed</p>");
    assert!(
        sync.starts_with(&format!("{CHAT_BASE}/v1/users/ME/conversations/{AB_CHAT}/messages?")),
        "{sync}"
    );
    Ok(())
}

#[tokio::test]
async fn a_backward_page_hands_out_its_first_pages_sync_link() -> TestResult {
    let lab = Lab::start().await?;
    let path = format!(
        "users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize=2",
        segment(AB_CHAT)
    );
    let first = lab.get(LAB_B, &path).await?;
    let back = first["_metadata"]["backwardLink"].as_str().ok_or("no backward link")?;
    // a posts while b pages back: the older page leaves it out, and its sync link still covers it
    let id = lab
        .post(LAB_A, AB_CHAT, &text_message("<p>while paging</p>", "5"))
        .await?;
    let older = lab.follow(LAB_B, back).await?;
    assert_eq!(older["_metadata"]["syncState"], first["_metadata"]["syncState"]);
    let older_ids: Vec<Value> = older["messages"]
        .as_array()
        .map(|messages| messages.iter().map(|message| message["id"].clone()).collect())
        .unwrap_or_default();
    assert!(!older_ids.contains(&json!(id)), "{older_ids:?}");
    let latest = older["_metadata"]["syncState"].as_str().ok_or("no sync link")?;
    let changed = lab.follow(LAB_B, latest).await?;
    assert_eq!(changed["messages"][0]["id"], id);

    // a backward token the mock didn't hand out
    let forged = format!("{path}&syncState=mock-before-3");
    assert_eq!(lab.send(LAB_B, Method::GET, &forged, None).await?.status, 400);
    // an empty chat's page has no messages and a number, not null, for its last segment's end
    let empty = lab
        .get(
            LAB_A,
            &format!("users/ME/conversations/{}/messages?pageSize=2", segment(AC_CHAT)),
        )
        .await?;
    assert_eq!(empty["messages"], json!([]));
    assert_eq!(empty["_metadata"]["lastCompleteSegmentEndTime"], 0);
    assert!(empty["_metadata"].get("backwardLink").is_none());
    Ok(())
}

#[tokio::test]
async fn thread_reads_are_the_group_chat_fixtures() -> TestResult {
    let lab = Lab::start().await?;
    let thread = lab
        .get(LAB_B, &format!("threads/{}?view=msnp24Equivalent", segment(ABC_CHAT)))
        .await?;
    assert_eq!(thread, fixture("thread-group.json")?["response"]["body"]);
    let members_path = format!("threads/{}/members?view=msnp24Equivalent&pageSize=1", segment(ABC_CHAT));
    let members = lab.get(LAB_C, &members_path).await?;
    assert_eq!(members, fixture("thread-group-members.json")?["response"]["body"]);
    let filtered = lab
        .send(
            LAB_A,
            Method::GET,
            &format!("threads/{}/filtered?view=msnp24Equivalent", segment(ABC_CHAT)),
            None,
        )
        .await?;
    let recorded = fixture("thread-group-filtered.json")?;
    assert_eq!(filtered.status, 403);
    assert_eq!(filtered.json()?, recorded["response"]["body"]);

    // a 1:1 chat's thread takes the same shape with its two members; outsiders of a chat and unknown chats get nothing
    let one_to_one = lab
        .get(LAB_A, &format!("threads/{}?view=msnp24Equivalent", segment(AB_CHAT)))
        .await?;
    assert_eq!(one_to_one["id"], AB_CHAT);
    let listed: Vec<Value> = one_to_one["members"]
        .as_array()
        .map(|members| members.iter().map(|member| member["id"].clone()).collect())
        .unwrap_or_default();
    assert_eq!(listed, [json!(LAB_A), json!(LAB_B)]);
    // with the unique-roster key the live 1:1 read carried, valued as the conversation list gives it
    assert_eq!(one_to_one["properties"]["uniquerosterthread"], "True");
    assert!(thread["properties"].get("uniquerosterthread").is_none());
    let refused = lab
        .send(LAB_C, Method::GET, &format!("threads/{}", segment(AB_CHAT)), None)
        .await?;
    assert_eq!(refused.status, 403);
    let unknown = lab
        .send(LAB_A, Method::GET, "threads/19%3Anone%40thread.v2", None)
        .await?;
    assert_eq!(unknown.status, 404);
    Ok(())
}

/// The account's token as the transport asks for it.
struct MockToken(String);

impl Credentials for MockToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

#[tokio::test]
async fn the_thread_read_lists_the_mocks_one_to_one_members() -> TestResult {
    let lab = Lab::start().await?;
    for (id, other) in [(AB_CHAT, LAB_B), (AX_CHAT, OUTSIDER)] {
        let thread = lab
            .get(LAB_A, &format!("threads/{}?view=msnp24Equivalent", segment(id)))
            .await?;
        let members: Vec<&str> = thread["members"]
            .as_array()
            .ok_or("no members")?
            .iter()
            .filter_map(|member| member["id"].as_str())
            .collect();
        assert_eq!(members, [LAB_A, other]);
    }
    Ok(())
}

/// A read of the test's choosing, for the transport.
struct Get(Endpoint);

impl Read for Get {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }
}

#[tokio::test]
async fn a_typed_message_through_the_native_transport_reaches_the_other_member() -> TestResult {
    let lab = Lab::start().await?;
    let token = Arc::new(MockToken(lab.mock.token(LAB_A).ok_or("no a")?));
    let transport = HttpTransport::with_mock_origin(token, CHAT_BASE, &lab.mock.origin())?;
    let client = Client::new(transport);

    let list = Get(Endpoint::new(["v1", "users", "ME", "conversations"])
        .with_query("view", "msnp24Equivalent")
        .with_query("pageSize", "200"));
    let read = client.read(&list).await?;
    assert_eq!(ids(&serde_json::from_slice(read.body())?).len(), 6);

    let answer = client
        .write(SendMessage::new(
            AB_CHAT,
            Author::new(Mri::new(LAB_A), "Lab A"),
            MessageText::plain("hello b")?,
        ))
        .await?;
    assert_eq!(answer.status(), 201);
    let arrival = serde_json::from_slice::<Value>(answer.body())?["OriginalArrivalTime"].to_string();
    let location = format!("{REGIONAL_HOST}/v1/users/ME/conversations/{AB_CHAT}/messages/{arrival}");
    assert_eq!(answer.location(), Some(location.as_str()));

    let events = lab.mock.take_events(LAB_B);
    assert_eq!(kinds(&events), [EventKind::Message]);
    assert_eq!(
        (events[0].conversation.as_str(), events[0].from.as_str()),
        (AB_CHAT, LAB_A)
    );
    assert_eq!(events[0].resource["content"], "<p>hello b</p>");
    assert!(lab.mock.take_events(LAB_A).is_empty(), "the writer is told nothing");
    assert!(lab.mock.take_events(LAB_B).is_empty(), "taken once");
    assert_eq!(lab.message(LAB_B, AB_CHAT, &arrival).await?, events[0].resource);
    Ok(())
}

/// `message` with the values the service sets on arrival taken out, to compare with a fixture's read-back.
fn without_service_values(message: &Value) -> Value {
    let mut message = message.clone();
    if let Some(fields) = message.as_object_mut() {
        for key in ["id", "version", "sequenceId", "composetime", "originalarrivaltime"] {
            fields.remove(key);
        }
    }
    if let Some(properties) = message["properties"].as_object_mut() {
        properties.remove("languageStamp");
    }
    message
}

#[tokio::test]
async fn a_quoted_reply_reads_back_as_the_reply_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("reply.json")?;
    let path = format!("users/ME/conversations/{}/messages", segment(AB_CHAT));
    let answer = lab
        .send(LAB_A, Method::POST, &path, Some(&recorded["request"]["body"]))
        .await?;
    assert_eq!(answer.status, 201);
    assert_eq!(keys(&answer.json()?), keys(&recorded["response"]["body"]));
    assert!(
        answer
            .location
            .as_ref()
            .is_some_and(|location| location.starts_with(REGIONAL_HOST))
    );
    let id = answer.json()?["OriginalArrivalTime"].to_string();
    let read = lab.message(LAB_B, AB_CHAT, &id).await?;
    assert_eq!(
        without_service_values(&read),
        without_service_values(&recorded["read_back"])
    );
    assert_eq!(read["sequenceId"], 6);
    assert_eq!(read["version"], id);
    assert!(
        read["composetime"].as_str().is_some_and(|time| time.len() == 28),
        "{read}"
    );
    Ok(())
}

#[tokio::test]
async fn an_edit_reads_back_as_the_edit_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("edit.json")?;
    let path = format!("users/ME/conversations/{}/messages/1760000010000", segment(AB_CHAT));
    let answer = lab
        .send(LAB_A, Method::PUT, &path, Some(&recorded["request"]["body"]))
        .await?;
    assert_eq!((answer.status, answer.text.as_str()), (200, ""));
    let read = lab.message(LAB_B, AB_CHAT, "1760000010000").await?;
    let expected = &recorded["read_back"];
    for key in ["id", "clientmessageid", "content", "messagetype", "from", "composetime"] {
        assert_eq!(read[key], expected[key], "{key}");
    }
    assert_eq!(read["properties"]["composetime"], expected["properties"]["composetime"]);
    // the service's own time: the version and the edit time move together
    assert_eq!(
        read["version"].as_str().map(str::to_owned),
        Some(read["properties"]["edittime"].to_string())
    );
    let events = lab.mock.take_events(LAB_B);
    assert_eq!(kinds(&events), [EventKind::Edit]);
    assert_eq!(events[0].resource, read);
    Ok(())
}

#[tokio::test]
async fn a_soft_delete_reads_back_as_the_delete_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("delete.json")?;
    let id = lab
        .post(LAB_A, AB_CHAT, &text_message("<p>to delete</p>", "7777777777777777777"))
        .await?;
    let path = format!("users/ME/conversations/{}/messages/{id}", segment(AB_CHAT));
    let answer = lab
        .send(LAB_A, Method::DELETE, &format!("{path}?behavior=softDelete"), None)
        .await?;
    assert_eq!((answer.status, answer.text.as_str()), (200, "null"));
    let read = lab.message(LAB_B, AB_CHAT, &id).await?;
    let mut read_keys = keys(&read);
    let mut recorded_keys = keys(&recorded["read_back"]);
    read_keys.sort();
    recorded_keys.sort();
    assert_eq!(read_keys, recorded_keys);
    assert_eq!(read["content"], "");
    assert_eq!(read["skypeeditedid"], "7777777777777777777");
    assert_eq!(
        read["properties"],
        json!({"deletetime": read["version"].as_str().unwrap_or_default().parse::<u64>()?})
    );
    assert_eq!(
        kinds(&lab.mock.take_events(LAB_B)),
        [EventKind::Message, EventKind::Delete]
    );

    // a deleted message isn't edited or deleted again, and nobody is told
    let edit = text_message("<p>back</p>", "7777777777777777777");
    assert_eq!(lab.send(LAB_A, Method::PUT, &path, Some(&edit)).await?.status, 404);
    let again = lab
        .send(LAB_A, Method::DELETE, &format!("{path}?behavior=softDelete"), None)
        .await?;
    assert_eq!(again.status, 404);
    assert!(lab.mock.take_events(LAB_B).is_empty());
    assert_eq!(lab.message(LAB_B, AB_CHAT, &id).await?, read);
    Ok(())
}

#[tokio::test]
async fn reactions_read_back_as_the_reaction_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let recorded = fixture("reaction.json")?;
    let path = format!(
        "users/ME/conversations/{}/messages/1760000050000/properties?name=emotions",
        segment(AB_CHAT)
    );
    let like = json!({"emotions": {"key": "like", "value": 1_760_000_055_000_u64}});
    let added = lab.send(LAB_A, Method::PUT, &path, Some(&like)).await?;
    assert_eq!((added.status, added.text.as_str()), (200, "200"));
    let read = lab.message(LAB_B, AB_CHAT, "1760000050000").await?;
    let kept = &recorded["read_back_kept"];
    assert_eq!(read["annotationsSummary"], kept["annotationsSummary"]);
    let users = &read["properties"]["emotions"][0]["users"];
    assert_eq!(users[0]["mri"], LAB_A);
    assert_eq!(
        users[0]["value"],
        kept["properties"]["emotions"][0]["users"][0]["value"]
    );
    assert_eq!(
        users[0]["time"].to_string(),
        read["version"].as_str().unwrap_or_default()
    );

    let removed = lab
        .send(
            LAB_A,
            Method::DELETE,
            &path,
            Some(&recorded["remove"]["request"]["body"]),
        )
        .await?;
    assert_eq!((removed.status, removed.text.as_str()), (200, "null"));
    let read = lab.message(LAB_B, AB_CHAT, "1760000050000").await?;
    let taken_back = &recorded["read_back_taken_back"];
    assert_eq!(read["annotationsSummary"], taken_back["annotationsSummary"]);
    assert_eq!(read["properties"]["emotions"], taken_back["properties"]["emotions"]);

    // taking it back again (a retry), or one never there, is answered the same and changes nothing
    let plain = "1760000005000";
    let plain_before = lab.message(LAB_B, AB_CHAT, plain).await?;
    let plain_path = format!(
        "users/ME/conversations/{}/messages/{plain}/properties?name=emotions",
        segment(AB_CHAT)
    );
    for retry in [&path, &plain_path] {
        let answer = lab
            .send(
                LAB_A,
                Method::DELETE,
                retry,
                Some(&recorded["remove"]["request"]["body"]),
            )
            .await?;
        assert_eq!((answer.status, answer.text.as_str()), (200, "null"), "{retry}");
    }
    assert_eq!(lab.message(LAB_B, AB_CHAT, "1760000050000").await?, read);
    assert_eq!(lab.message(LAB_B, AB_CHAT, plain).await?, plain_before);
    assert_eq!(
        kinds(&lab.mock.take_events(LAB_B)),
        [EventKind::Reaction, EventKind::Reaction]
    );
    Ok(())
}

#[tokio::test]
async fn a_read_receipt_sets_only_the_readers_own_horizon() -> TestResult {
    let lab = Lab::start().await?;
    let a_horizon =
        entry(&lab.list(LAB_A).await?, AB_CHAT).ok_or("no chat")?["properties"]["consumptionhorizon"].clone();
    let horizon = "1760000060000;1760001000500;5555555555555555555";
    let path = format!(
        "users/ME/conversations/{}/properties?name=consumptionhorizon",
        segment(AB_CHAT)
    );
    let answer = lab
        .send(LAB_B, Method::PUT, &path, Some(&json!({"consumptionhorizon": horizon})))
        .await?;
    assert_eq!((answer.status, answer.text.as_str()), (200, ""));
    let b_list = lab.list(LAB_B).await?;
    assert_eq!(
        entry(&b_list, AB_CHAT).ok_or("no chat")?["properties"]["consumptionhorizon"],
        horizon
    );
    let a_list = lab.list(LAB_A).await?;
    assert_eq!(
        entry(&a_list, AB_CHAT).ok_or("no chat")?["properties"]["consumptionhorizon"],
        a_horizon
    );
    let events = lab.mock.take_events(LAB_A);
    assert_eq!(kinds(&events), [EventKind::ReadReceipt]);
    assert_eq!(events[0].resource, json!({"consumptionhorizon": horizon}));
    assert!(lab.mock.take_events(LAB_B).is_empty());
    Ok(())
}

#[tokio::test]
async fn a_channel_s_own_notification_settings_read_back_only_for_who_set_them() -> TestResult {
    let lab = Lab::start().await?;
    let read = format!("users/ME/conversations/{}?view=msnp24Equivalent", segment(TEAM_CHANNEL));
    assert_eq!(
        lab.get(LAB_A, &read).await?,
        json!({"id": TEAM_CHANNEL, "properties": {}})
    );
    let settings = json!({"allNewPosts": "On", "includeReplies": true}).to_string();
    for (name, value) in [
        ("channelNotificationSettings", settings.as_str()),
        ("followAllRc", "1760000060000"),
    ] {
        let path = format!(
            "users/ME/conversations/{}/properties?name={name}",
            segment(TEAM_CHANNEL)
        );
        let answer = lab
            .send(LAB_A, Method::PUT, &path, Some(&json!({ name: value })))
            .await?;
        assert_eq!(answer.status, 200, "{name}");
    }
    assert_eq!(
        lab.get(LAB_A, &read).await?["properties"],
        json!({"channelNotificationSettings": settings, "followAllRc": "1760000060000"})
    );
    assert_eq!(
        lab.get(LAB_B, &read).await?["properties"],
        json!({}),
        "b's own view is b's"
    );
    assert!(lab.mock.take_events(LAB_B).is_empty(), "nobody is told");
    Ok(())
}

#[tokio::test]
async fn a_post_made_for_a_test_reads_back_and_reaches_the_others_as_a_client_s_does() -> TestResult {
    let lab = Lab::start().await?;
    let id = lab
        .mock
        .post_as(LAB_C, ABC_CHAT, "<p>from c</p>")
        .ok_or("c may write in the group chat")?;
    let posted = lab.post(LAB_A, ABC_CHAT, &text_message("<p>from a</p>", "7")).await?;
    let read = lab.messages(LAB_B, ABC_CHAT).await?;
    let made = read
        .iter()
        .find(|message| message["id"] == id.as_str())
        .ok_or("the post reads back")?;
    let sent = read
        .iter()
        .find(|message| message["id"] == posted)
        .ok_or("a client's post reads back")?;
    assert_eq!(keys(made), keys(sent));
    assert_eq!(made["content"], "<p>from c</p>");
    assert_eq!(made["messagetype"], "RichText/Html");
    for member in [LAB_A, LAB_B] {
        let events = lab.mock.take_events(member);
        let from_c: Vec<&ChatEvent> = events.iter().filter(|event| event.from == LAB_C).collect();
        assert_eq!(from_c.len(), 1, "{member}");
        assert_eq!(from_c[0].kind, EventKind::Message);
        assert_eq!(from_c[0].resource["id"], id.as_str());
    }
    assert!(lab.mock.take_events(LAB_C).iter().all(|event| event.from != LAB_C));
    // nobody posts where they aren't a member, nor to the notifications stream
    assert_eq!(lab.mock.post_as(LAB_C, AB_CHAT, "<p>no</p>"), None);
    assert_eq!(lab.mock.post_as(LAB_A, NOTIFICATIONS, "<p>no</p>"), None);
    Ok(())
}

#[tokio::test]
async fn typing_and_group_messages_reach_every_other_member() -> TestResult {
    let lab = Lab::start().await?;
    let typing = json!({"content": "", "messagetype": "Control/Typing", "contenttype": "Application/Message"});
    lab.post(LAB_A, ABC_CHAT, &typing).await?;
    assert!(
        lab.messages(LAB_B, ABC_CHAT).await?.is_empty(),
        "typing isn't a message"
    );
    let id = lab
        .post(LAB_A, ABC_CHAT, &text_message("<p>hello both</p>", "2"))
        .await?;
    for member in [LAB_B, LAB_C] {
        let events = lab.mock.take_events(member);
        assert_eq!(kinds(&events), [EventKind::Typing, EventKind::Message], "{member}");
        assert_eq!(events[0].resource, json!({"messagetype": "Control/Typing"}));
        assert_eq!(events[1].resource["id"], id);
    }
    assert!(lab.mock.take_events(LAB_A).is_empty());
    let list = lab.list(LAB_C).await?;
    let abc = entry(&list, ABC_CHAT).ok_or("no group chat")?;
    assert_eq!(abc["lastMessage"]["id"], id);
    assert_eq!(abc["properties"]["isemptyconversation"], "False");
    Ok(())
}

#[tokio::test]
async fn a_chat_create_makes_the_one_to_one_chat_or_a_new_group_chat() -> TestResult {
    let lab = Lab::start().await?;
    let members =
        |mris: &[&str]| -> Vec<Value> { mris.iter().map(|mri| json!({"id": mri, "role": "Admin"})).collect() };
    let one_to_one = json!({
        "members": members(&[LAB_B, LAB_C]),
        "properties": {"threadType": "chat", "fixedRoster": true, "uniquerosterthread": true},
    });
    let created = lab.send(LAB_B, Method::POST, "threads", Some(&one_to_one)).await?;
    assert_eq!(created.status, 201);
    let bc = format!("19:{}_{}@unq.gbl.spaces", oid(LAB_B), oid(LAB_C));
    assert_eq!(created.json()?["threadResource"]["id"], bc);
    assert_eq!(
        created.json()?["threadResource"]["properties"]["uniquerosterthread"],
        "True"
    );
    assert_eq!(created.location, Some(format!("{REGIONAL_HOST}/v1/threads/{bc}")));
    assert!(ids(&lab.list(LAB_C).await?).contains(&bc));
    assert_eq!(kinds(&lab.mock.take_events(LAB_C)), [EventKind::ChatCreated]);
    // asked again, it is the same chat, and nobody is told again
    let again = lab.send(LAB_C, Method::POST, "threads", Some(&one_to_one)).await?;
    assert_eq!(again.json()?["threadResource"]["id"], bc);
    assert!(lab.mock.take_events(LAB_B).is_empty());

    let group = json!({"members": members(&[LAB_A, LAB_B, LAB_C]), "properties": {"threadType": "chat"}});
    let made = lab.send(LAB_A, Method::POST, "threads", Some(&group)).await?.json()?;
    let id = made["threadResource"]["id"].as_str().ok_or("no id")?.to_owned();
    assert!(
        id.starts_with("19:") && id.ends_with("@thread.v2") && id != ABC_CHAT,
        "{id}"
    );
    assert_eq!(made["threadResource"]["members"].as_array().map(Vec::len), Some(3));
    assert!(made["threadResource"]["properties"].get("uniquerosterthread").is_none());
    for member in [LAB_B, LAB_C] {
        assert_eq!(kinds(&lab.mock.take_events(member)), [EventKind::ChatCreated]);
    }

    let without_me = json!({"members": members(&[LAB_B, LAB_C]), "properties": {"threadType": "chat"}});
    assert_eq!(
        lab.send(LAB_A, Method::POST, "threads", Some(&without_me))
            .await?
            .status,
        403
    );
    let no_properties = json!({"members": members(&[LAB_A, LAB_B])});
    assert_eq!(
        lab.send(LAB_A, Method::POST, "threads", Some(&no_properties))
            .await?
            .status,
        400
    );
    Ok(())
}

/// The headers a request carries, what it is, and the status it gets.
struct HeaderCase<'a> {
    case: &'a str,
    headers: Vec<(&'a str, &'a str)>,
    expected: u16,
}

#[tokio::test]
async fn a_request_without_the_headers_task_3_found_is_refused_with_401() -> TestResult {
    let lab = Lab::start().await?;
    let token = lab.mock.token(LAB_A).ok_or("no a")?;
    let bearer = format!("Bearer {token}");
    let skype = format!("skypetoken={token}");
    let list = lab.url("users/ME/conversations?view=msnp24Equivalent");
    let (migration, behavior) = (("X-MS-Migration", "True"), ("BehaviorOverride", "redirectAs404"));
    let case = |case, headers, expected| HeaderCase {
        case,
        headers,
        expected,
    };
    let cases = [
        case("no token", vec![migration, behavior], 401),
        case(
            "a skypetoken",
            vec![("Authentication", &skype), migration, behavior],
            401,
        ),
        case(
            "a token the mock didn't hand out",
            vec![
                ("Authorization", "Bearer mock-ic3-token-unknown-0"),
                migration,
                behavior,
            ],
            401,
        ),
        case("no X-MS-Migration", vec![("Authorization", &bearer), behavior], 401),
        case("no BehaviorOverride", vec![("Authorization", &bearer), migration], 401),
        case(
            "the web client's set",
            vec![("Authorization", &bearer), migration, behavior],
            200,
        ),
    ];
    for HeaderCase {
        case,
        headers,
        expected,
    } in cases
    {
        let answer = lab.send_raw(Method::GET, &list, &headers, None).await?;
        assert_eq!(answer.status, expected, "{case}");
        if expected == 401 {
            assert_eq!(answer.text, "", "{case}");
        }
    }

    // a write without them changes nothing and tells nobody
    let post = lab.url(&format!("users/ME/conversations/{}/messages", segment(AB_CHAT)));
    let body = text_message("<p>unheard</p>", "3");
    let refused = lab
        .send_raw(Method::POST, &post, &[("Authorization", &bearer)], Some(&body))
        .await?;
    assert_eq!(refused.status, 401);
    assert!(lab.mock.take_events(LAB_B).is_empty());
    assert_eq!(lab.messages(LAB_B, AB_CHAT).await?.len(), 5);
    Ok(())
}

#[tokio::test]
async fn the_thread_read_takes_the_token_alone_and_a_replaced_token_is_refused() -> TestResult {
    let lab = Lab::start().await?;
    let bearer = format!("Bearer {}", lab.mock.token(LAB_A).ok_or("no a")?);
    // the direct thread read sends the token and `Accept` only, as Task 3 saw the service answer in full
    let thread = lab.url(&format!("threads/{}?view=msnp24Equivalent", segment(AB_CHAT)));
    let alone = lab
        .send_raw(Method::GET, &thread, &[("Authorization", &bearer)], None)
        .await?;
    assert_eq!(alone.status, 200);
    assert_eq!(lab.send_raw(Method::GET, &thread, &[], None).await?.status, 401);

    assert!(lab.mock.set_token(LAB_A, "mock-ic3-token-renewed-1"));
    let stale = lab
        .send_raw(Method::GET, &thread, &[("Authorization", &bearer)], None)
        .await?;
    assert_eq!(stale.status, 401);
    assert_eq!(lab.mock.token(LAB_A).as_deref(), Some("mock-ic3-token-renewed-1"));
    assert_eq!(
        lab.send(LAB_A, Method::GET, "users/ME/conversations", None)
            .await?
            .status,
        200
    );
    assert!(!lab.mock.set_token("8:orgid:nobody", "mock-ic3-token-nobody-0"));
    Ok(())
}

/// A request the mock refuses: who sends it, how, and the status it gets.
struct Refusal {
    case: &'static str,
    who: &'static str,
    method: Method,
    path: String,
    body: Option<Value>,
    expected: u16,
}

impl Refusal {
    fn new(
        case: &'static str,
        who: &'static str,
        method: Method,
        path: String,
        body: Option<Value>,
        expected: u16,
    ) -> Self {
        Self {
            case,
            who,
            method,
            path,
            body,
            expected,
        }
    }
}

/// Sends every refusal, then checks that none of them changed anything or told anyone.
async fn refused(lab: &Lab, refusals: Vec<Refusal>) -> TestResult {
    for refusal in refusals {
        let answer = lab
            .send(refusal.who, refusal.method, &refusal.path, refusal.body.as_ref())
            .await?;
        assert_eq!(answer.status, refusal.expected, "{}: {}", refusal.case, answer.text);
    }
    for member in [LAB_A, LAB_B, LAB_C] {
        assert!(lab.mock.take_events(member).is_empty(), "{member}");
    }
    assert_eq!(lab.messages(LAB_A, AB_CHAT).await?.len(), 5);
    let edited = lab.message(LAB_A, AB_CHAT, "1760000010000").await?;
    assert_eq!(edited["content"], "<p>a made-up message, edited</p>");
    Ok(())
}

fn messages_path(chat: &str) -> String {
    format!("users/ME/conversations/{}/messages", segment(chat))
}

fn message_path(id: &str) -> String {
    format!("users/ME/conversations/{}/messages/{id}", segment(AB_CHAT))
}

#[tokio::test]
async fn writes_by_someone_who_may_not_make_them_change_nothing() -> TestResult {
    let lab = Lab::start().await?;
    let body = || Some(text_message("<p>no</p>", "4"));
    let a_message = message_path("1760000010000");
    let refusals = vec![
        Refusal::new("not a member", LAB_C, Method::POST, messages_path(AB_CHAT), body(), 403),
        Refusal::new(
            "the notifications stream",
            LAB_A,
            Method::POST,
            messages_path(NOTIFICATIONS),
            body(),
            403,
        ),
        Refusal::new(
            "someone else's edit",
            LAB_B,
            Method::PUT,
            a_message.clone(),
            body(),
            403,
        ),
        Refusal::new(
            "someone else's delete",
            LAB_B,
            Method::DELETE,
            format!("{a_message}?behavior=softDelete"),
            None,
            403,
        ),
        Refusal::new(
            "an unknown chat",
            LAB_A,
            Method::POST,
            messages_path("19:none@thread.v2"),
            body(),
            404,
        ),
        Refusal::new("an unknown message", LAB_A, Method::PUT, message_path("1"), body(), 404),
    ];
    refused(&lab, refusals).await
}

#[tokio::test]
async fn requests_the_mock_does_not_model_or_cannot_read_change_nothing() -> TestResult {
    let lab = Lab::start().await?;
    let a_message = message_path("1760000010000");
    let post = || messages_path(AB_CHAT);
    let refusals = vec![
        Refusal::new(
            "a body that isn't an object",
            LAB_A,
            Method::POST,
            post(),
            Some(json!("text")),
            400,
        ),
        Refusal::new(
            "an unknown message type",
            LAB_A,
            Method::POST,
            post(),
            Some(json!({"content": "", "messagetype": "Event/Call"})),
            400,
        ),
        Refusal::new(
            "mentions that aren't a string",
            LAB_A,
            Method::POST,
            post(),
            Some(json!({"content": "", "messagetype": "Text", "properties": {"mentions": []}})),
            400,
        ),
        Refusal::new(
            "another message property",
            LAB_A,
            Method::PUT,
            format!("{a_message}/properties?name=isread"),
            Some(json!({"isread": "true"})),
            404,
        ),
        Refusal::new("a hard delete", LAB_A, Method::DELETE, a_message, None, 404),
        Refusal::new(
            "an unmodelled read",
            LAB_A,
            Method::GET,
            "users/ME/properties".to_owned(),
            None,
            404,
        ),
    ];
    refused(&lab, refusals).await?;

    // a chat create naming a member that isn't a work account's MRI makes no chat (and no header out of its id)
    let one_to_one = json!({
        "members": [{"id": LAB_A, "role": "Admin"}, {"id": "8:orgid:bad\nid", "role": "Admin"}],
        "properties": {"threadType": "chat", "uniquerosterthread": true},
    });
    let group = json!({
        "members": [{"id": LAB_A}, {"id": LAB_B}, {"id": "8:orgid:not-a-guid"}],
        "properties": {"threadType": "chat"},
    });
    for body in [one_to_one, group] {
        let answer = lab.send(LAB_A, Method::POST, "threads", Some(&body)).await?;
        assert_eq!((answer.status, answer.location), (400, None), "{body}");
    }
    assert_eq!(ids(&lab.list(LAB_A).await?).len(), 6);
    for member in [LAB_A, LAB_B, LAB_C] {
        assert!(lab.mock.take_events(member).is_empty(), "{member}");
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether the dependency `name = spec` is `teams-mock`: by its name, its `package` (a renamed key), its `path`, or as
/// a workspace dependency named in `aliases`.
fn is_mock(name: &str, spec: &toml::Value, aliases: &[String]) -> bool {
    let package = spec.get("package").and_then(toml::Value::as_str);
    let path = spec.get("path").and_then(toml::Value::as_str);
    let inherited = spec.get("workspace").and_then(toml::Value::as_bool) == Some(true);
    name == "teams-mock"
        || package == Some("teams-mock")
        || path.is_some_and(|path| Path::new(path).file_name().is_some_and(|dir| dir == "teams-mock"))
        || (inherited && aliases.iter().any(|alias| alias == name))
}

/// The keys `[workspace.dependencies]` of `workspace` takes `teams-mock` under.
fn mock_aliases(workspace: &toml::Table) -> Vec<String> {
    workspace
        .get("workspace")
        .and_then(|table| table.get("dependencies"))
        .and_then(toml::Value::as_table)
        .map(|deps| {
            deps.iter()
                .filter(|(name, spec)| is_mock(name, spec, &[]))
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `manifest` takes `teams-mock` (under any key, `aliases` being the workspace's keys for it) in a dependency
/// table other than `[dev-dependencies]` (target-specific tables included).
fn normal_dependency(manifest: &toml::Table, aliases: &[String]) -> bool {
    let names = |table: Option<&toml::Value>| {
        ["dependencies", "build-dependencies"].iter().any(|kind| {
            table
                .and_then(|table| table.get(*kind))
                .and_then(toml::Value::as_table)
                .is_some_and(|deps| deps.iter().any(|(name, spec)| is_mock(name, spec, aliases)))
        })
    };
    let root = Some(toml::Value::Table(manifest.clone()));
    let targets = manifest.get("target").and_then(toml::Value::as_table);
    names(root.as_ref()) || targets.is_some_and(|targets| targets.values().any(|target| names(Some(target))))
}

#[test]
fn the_mock_is_never_a_normal_dependency_of_any_crate() -> TestResult {
    let root = workspace_root();
    let workspace: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))?.parse()?;
    let members = workspace["workspace"]["members"].as_array().ok_or("no members")?;
    let aliases = mock_aliases(&workspace);
    assert_eq!(aliases, ["teams-mock"]);
    assert!(
        members
            .iter()
            .any(|member| member.as_str() == Some("crates/teams-mock"))
    );
    let mut checked = 0;
    for member in members {
        let path = root.join(member.as_str().ok_or("not a path")?).join("Cargo.toml");
        let manifest: toml::Table = std::fs::read_to_string(&path)?.parse()?;
        assert!(
            !normal_dependency(&manifest, &aliases),
            "{} takes teams-mock as a normal dependency",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 10);
    // the check itself sees one
    let planted: toml::Table = "[target.'cfg(unix)'.dependencies]\nteams-mock.workspace = true\n".parse()?;
    assert!(normal_dependency(&planted, &aliases));
    let dev_only: toml::Table = "[dev-dependencies]\nteams-mock.workspace = true\n".parse()?;
    assert!(!normal_dependency(&dev_only, &aliases));
    // under a renamed key: by its package, by its path, or through a renamed workspace dependency
    let renamed: toml::Table =
        "[dependencies]\nmock = { package = \"teams-mock\", path = \"../teams-mock\" }\n".parse()?;
    assert!(normal_dependency(&renamed, &aliases));
    let by_path: toml::Table = "[build-dependencies]\nmock = { path = \"../teams-mock/\" }\n".parse()?;
    assert!(normal_dependency(&by_path, &aliases));
    let renamed_workspace: toml::Table =
        "[workspace.dependencies]\nmock = { package = \"teams-mock\", path = \"crates/teams-mock\" }\n".parse()?;
    let inherited: toml::Table = "[dependencies]\nmock.workspace = true\n".parse()?;
    assert!(normal_dependency(&inherited, &mock_aliases(&renamed_workspace)));
    assert!(!normal_dependency(&inherited, &aliases));
    let renamed_dev: toml::Table =
        "[dev-dependencies]\nmock = { package = \"teams-mock\", path = \"../teams-mock\" }\n".parse()?;
    assert!(!normal_dependency(&renamed_dev, &aliases));
    Ok(())
}

#[tokio::test]
async fn csa_gives_the_teams_of_its_fixture_to_the_read_in_its_probed_shape_only() -> TestResult {
    let lab = Lab::start().await?;
    let query = "isPrefetch=false&enableMembershipSummary=true&supportsAdditionalSystemGeneratedFolders=true\
                 &supportsSliceItems=true&enableEngageCommunities=false";
    let url = |path: &str, query: &str| format!("{}/api/csa/amer/{path}?{query}", lab.mock.origin());
    let token = format!("Bearer {}", lab.mock.token(LAB_B).ok_or("no such account")?);
    let headers = [("Authorization", token.as_str())];
    let answer = lab
        .send_raw(Method::GET, &url("api/v3/teams/users/me", query), &headers, None)
        .await?;
    assert_eq!(answer.status, 200, "{}", answer.text);
    let teams = answer.json()?;
    let recorded = serde_json::from_str::<Value>(
        &std::fs::read_to_string(Path::new(CSA_FIXTURES).join("teams.json"))?
            .replace("<guid:a>", oid(LAB_A))
            .replace("<guid:tenant>", TENANT)
            .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
            .replace("<token:1>", "mock-fixture-1"),
    )?;
    assert_eq!(teams, recorded["response"]["body"]);
    let channels: Vec<&str> = teams["teams"][0]["channels"]
        .as_array()
        .ok_or("no channels")?
        .iter()
        .filter_map(|channel| channel["id"].as_str())
        .collect();
    assert!(channels.contains(&CHANNEL) && channels.contains(&TEAM_CHANNEL));

    // another query, another path, another method, no token: nothing
    for (method, path, query, status) in [
        (Method::GET, "api/v3/teams/users/me", "isPrefetch=true", 404),
        (Method::GET, "api/v3/teams/users/me/updates", query, 404),
        (Method::POST, "api/v3/teams/users/me", query, 404),
    ] {
        let answer = lab.send_raw(method.clone(), &url(path, query), &headers, None).await?;
        assert_eq!(answer.status, status, "{method} {path}?{query}");
    }
    let bare = lab
        .send_raw(Method::GET, &url("api/v3/teams/users/me", query), &[], None)
        .await?;
    assert_eq!(bare.status, 401);
    Ok(())
}

#[tokio::test]
async fn the_teams_other_channel_is_read_by_its_id_and_listed_for_nobody() -> TestResult {
    let lab = Lab::start().await?;
    for who in [LAB_A, LAB_B, LAB_C] {
        assert!(!ids(&lab.list(who).await?).iter().any(|id| id == TEAM_CHANNEL), "{who}");
    }
    let page = lab
        .get(
            LAB_B,
            &format!(
                "users/ME/conversations/{}/messages?view=msnp24Equivalent%7CsupportsMessageProperties&pageSize=200",
                segment(TEAM_CHANNEL)
            ),
        )
        .await?;
    let recorded = fixture("channel-messages.json")?;
    assert_eq!(page["messages"], recorded["response"]["body"]["messages"]);
    let thread = lab.get(LAB_A, &format!("threads/{}", segment(TEAM_CHANNEL))).await?;
    assert_eq!(thread["properties"]["threadType"], "topic");
    Ok(())
}

#[tokio::test]
async fn a_channel_takes_a_post_and_a_reply_in_a_posts_thread_posted_to_its_chain() -> TestResult {
    let lab = Lab::start().await?;
    let message = json!({"content": "<p>hi</p>", "messagetype": "RichText/Html", "contenttype": "Text"});
    let root = lab.post(LAB_A, TEAM_CHANNEL, &message).await?;
    let chain = format!("{TEAM_CHANNEL};messageid={root}");
    lab.post(LAB_B, &chain, &message).await?;
    let page = lab
        .get(
            LAB_C,
            &format!("users/ME/conversations/{}/messages?pageSize=200", segment(TEAM_CHANNEL)),
        )
        .await?;
    let newest = &page["messages"][0];
    assert_eq!(newest["conversationid"], TEAM_CHANNEL);
    assert!(
        newest["conversationLink"]
            .as_str()
            .is_some_and(|link| link.ends_with(&format!(";messageid={root}"))),
        "{newest}"
    );
    // a reply under no post the channel holds is refused, and so is a write to the notifications stream
    let orphan = format!(
        "users/ME/conversations/{}/messages",
        segment(&format!("{TEAM_CHANNEL};messageid=1"))
    );
    let refused = lab.send(LAB_A, Method::POST, &orphan, Some(&message)).await?;
    assert_eq!(refused.status, 404);
    assert_eq!(lab.mock.post_as(LAB_A, NOTIFICATIONS, "<p>no</p>"), None);
    // a member's post seeded by the mock is stored as a client's: its own chain's post
    let seeded = lab.mock.post_as(LAB_B, TEAM_CHANNEL, "<p>yes</p>").ok_or("seeded")?;
    let page = lab
        .get(
            LAB_C,
            &format!("users/ME/conversations/{}/messages?pageSize=200", segment(TEAM_CHANNEL)),
        )
        .await?;
    let newest = &page["messages"][0];
    assert_eq!(newest["id"], seeded.as_str());
    assert!(
        newest["conversationLink"]
            .as_str()
            .is_some_and(|link| link.ends_with(&format!(";messageid={seeded}"))),
        "{newest}"
    );
    Ok(())
}

#[test]
fn the_mock_reads_only_the_synthetic_chat_fixtures() -> TestResult {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![crate_dir.join("src"), crate_dir.join("tests")];
    while let Some(dir) = pending.pop() {
        for file in std::fs::read_dir(dir)? {
            let path = file?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let source = std::fs::read_to_string(&path)?;
            // split, so this file doesn't name them itself
            let forbidden = [
                concat!("captures", "/"),
                concat!("reference", "/"),
                concat!("teams", "-lab"),
                concat!(".local", "/share"),
            ];
            for forbidden in forbidden {
                assert!(!source.contains(forbidden), "{} names {forbidden}", path.display());
            }
            for include in source.split("include_str!(\"").skip(1) {
                assert!(
                    include.starts_with("../../../tests/fixtures/chatsvc/")
                        || include.starts_with("../../../tests/fixtures/trouter/")
                        || include.starts_with("../../../tests/fixtures/csa/")
                        || include.starts_with("../../../tests/fixtures/calling/chat/")
                        || include.starts_with("../../../tests/fixtures/calling/echo/")
                        || include.starts_with("../../../tests/fixtures/loki/"),
                    "{}: {include:.60}",
                    path.display()
                );
            }
        }
    }
    for dir in [
        FIXTURES,
        TROUTER_FIXTURES,
        CSA_FIXTURES,
        CALL_FIXTURES,
        ECHO_FIXTURES,
        LOKI_FIXTURES,
    ] {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let text = std::fs::read_to_string(&path)?;
            let fixture: Value = serde_json::from_str(&text)?;
            assert!(
                fixture.get("_synthetic").is_some(),
                "{} isn't marked synthetic",
                path.display()
            );
        }
    }
    Ok(())
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The next text frame from the mock's Trouter (at most 5 s away).
async fn read_frame(socket: &mut Socket) -> TestResult<String> {
    use futures_util::StreamExt as _;
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await?
            .ok_or("the socket closed")??;
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            return Ok(text.as_str().to_owned());
        }
    }
}

async fn write_frame(socket: &mut Socket, text: &str) -> TestResult {
    use futures_util::SinkExt as _;
    socket
        .send(tokio_tungstenite::tungstenite::Message::text(text.to_owned()))
        .await?;
    Ok(())
}

/// `user.authenticate` as the messaging stack sends it, with `token`.
fn authenticate_frame(token: &str) -> String {
    let event = json!({"name": "user.authenticate", "args": [{"headers": {
        "X-Ms-Test-User": "False", "Authorization": format!("Bearer {token}"), "X-MS-Migration": "True"}}]});
    format!("5:::{event}")
}

/// A socket to the mock's Trouter, authenticated with `token`; its `trouter.connected` argument.
async fn connect_as(trouter: &teams_mock::TrouterMock, token: &str) -> TestResult<(Socket, Value)> {
    let url = format!(
        "{}/v4/c?tc=%7B%7D&timeout=40&epid=00000000-0000-4000-8000-0000000000e1",
        trouter.origin()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    assert_eq!(read_frame(&mut socket).await?, "1::");
    write_frame(&mut socket, &authenticate_frame(token)).await?;
    let connected = read_frame(&mut socket).await?;
    let event: Value = serde_json::from_str(connected.strip_prefix("5:1::").ok_or("not trouter.connected")?)?;
    assert_eq!(event["name"], "trouter.connected");
    Ok((socket, event["args"][0].clone()))
}

/// A registration of `push_url` for `app`, in `registration.json`'s shape.
fn registration_body(app: &str, push_url: &str) -> Value {
    json!({
        "clientDescription": {"appId": app, "aesKey": "", "languageId": "en-US", "platform": "chrome",
            "templateKey": format!("{app}_2.6"), "platformUIVersion": "1/1"},
        "registrationId": "00000000-0000-4000-8000-0000000000e1",
        "nodeId": "",
        "transports": {"TROUTER": [{"context": "", "path": push_url, "ttl": 3600}]},
    })
}

/// A fixture of `tests/fixtures/trouter/`, as it is.
fn trouter_fixture(name: &str) -> TestResult<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        Path::new(TROUTER_FIXTURES).join(name),
    )?)?)
}

/// The JSON of a recorded frame, after its packet prefix.
fn recorded_frame(frames: &Value, name: &str, prefix: &str) -> TestResult<Value> {
    let frame = frames
        .as_array()
        .and_then(|frames| frames.iter().find(|frame| frame["name"] == name))
        .and_then(|frame| frame["frame"].as_str())
        .ok_or_else(|| format!("no frame {name:?}"))?;
    Ok(serde_json::from_str(
        frame.strip_prefix(prefix).ok_or("another packet")?,
    )?)
}

impl Lab {
    async fn register(&self, token: Option<&str>, body: &Value) -> TestResult<u16> {
        let url = format!("{}/registrar/prod/V2/registrations", self.mock.origin());
        let bearer = token.map(|token| format!("Bearer {token}"));
        let mut headers = vec![("X-MS-Migration", "True"), ("X-Ms-Test-User", "False")];
        if let Some(bearer) = &bearer {
            headers.push(("Authorization", bearer.as_str()));
        }
        Ok(self.send_raw(Method::POST, &url, &headers, Some(body)).await?.status)
    }
}

#[tokio::test]
async fn the_mocks_trouter_connects_authenticates_and_answers_pings_as_the_session_fixture() -> TestResult {
    let lab = Lab::start().await?;
    let trouter = lab.mock.start_trouter()?;
    let token = lab.mock.token(LAB_B).ok_or("no b")?;
    let (mut socket, connected) = connect_as(&trouter, &token).await?;

    let session = trouter_fixture("session.json")?;
    let recorded = recorded_frame(&session["frames"], "the endpoint and its push URLs", "5:1::")?;
    assert_eq!(keys(&connected), keys(&recorded["args"][0]));
    let endpoint = connected["id"].as_str().ok_or("no id")?.to_owned();
    assert_eq!(
        connected["surl"],
        format!(
            "https://{}-f.trouter.teams.microsoft.com:3443/v4/f/{endpoint}/",
            teams_mock::REGIONAL_NAME
        )
    );
    assert_eq!(connected["connectparams"]["sr"], endpoint.as_str());

    write_frame(&mut socket, r#"5:1+::{"name":"ping"}"#).await?;
    assert_eq!(read_frame(&mut socket).await?, r#"6:::1+["pong"]"#);
    let record = &trouter.sockets()[0];
    assert_eq!(record.account.as_deref(), Some(LAB_B));
    assert_eq!(record.endpoint.as_deref(), Some(endpoint.as_str()));
    assert!(record.query.contains("epid=00000000-0000-4000-8000-0000000000e1"));
    assert_eq!(
        record.frames,
        [authenticate_frame(&token), r#"5:1+::{"name":"ping"}"#.to_owned()]
    );
    Ok(())
}

#[tokio::test]
async fn a_socket_that_is_late_to_authenticate_or_brings_a_strange_token_is_closed_with_the_reason() -> TestResult {
    let lab = Lab::start().await?;
    let trouter = lab.mock.start_trouter()?;
    trouter.set_auth_timeout(std::time::Duration::from_millis(100));
    let url = format!("{}/v4/c?timeout=40", trouter.origin());
    let (mut late, _) = tokio_tungstenite::connect_async(&url).await?;
    assert_eq!(read_frame(&mut late).await?, "1::");
    assert_eq!(read_frame(&mut late).await?, r#"0:::{"reason":"timeout"}"#);

    let (mut strange, _) = tokio_tungstenite::connect_async(&url).await?;
    assert_eq!(read_frame(&mut strange).await?, "1::");
    write_frame(&mut strange, &authenticate_frame("not-a-token-of-the-mocks")).await?;
    assert_eq!(read_frame(&mut strange).await?, r#"0:::{"reason":"unauthorized"}"#);
    Ok(())
}

#[tokio::test]
async fn the_registrar_takes_either_stacks_registration_with_the_accounts_token() -> TestResult {
    let lab = Lab::start().await?;
    let trouter = lab.mock.start_trouter()?;
    let token = lab.mock.token(LAB_B).ok_or("no b")?;
    let push = "https://mock-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/MockEndpoint000009/";
    let messaging = registration_body("TeamsCDLWebWorker", push);
    assert_eq!(lab.register(None, &messaging).await?, 401);
    assert_eq!(
        lab.register(Some(&token), &registration_body("SomeOtherApp", push))
            .await?,
        400
    );
    let mut mismatched = registration_body("SkypeSpacesWeb", push);
    mismatched["clientDescription"]["templateKey"] = json!("TeamsCDLWebWorker_2.6");
    assert_eq!(lab.register(Some(&token), &mismatched).await?, 400);
    assert_eq!(lab.register(Some(&token), &json!({"registrationId": "x"})).await?, 400);
    assert!(trouter.registrations().is_empty());
    assert_eq!(lab.register(Some(&token), &messaging).await?, 202);
    let calling_push = "https://mock-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/MockEndpoint000010/";
    assert_eq!(
        lab.register(Some(&token), &registration_body("SkypeSpacesWeb", calling_push))
            .await?,
        202
    );
    let registrations = trouter.registrations();
    assert_eq!(registrations.len(), 2);
    assert_eq!(
        (
            registrations[0].account.as_str(),
            registrations[0].app_id.as_str(),
            registrations[0].path.as_str()
        ),
        (LAB_B, teams_mock::MESSAGING_APP, push)
    );
    assert_eq!(
        (registrations[1].app_id.as_str(), registrations[1].path.as_str()),
        (teams_mock::CALLING_APP, calling_push)
    );
    assert_eq!(registrations[0].ttl, 3600);
    let url = format!("{}/registrar/prod/V2/registrations", lab.mock.origin());
    let bearer = format!("Bearer {token}");
    let read = lab
        .send_raw(Method::GET, &url, &[("Authorization", bearer.as_str())], None)
        .await?;
    assert_eq!(read.status, 404);
    Ok(())
}

#[tokio::test]
async fn deliveries_wait_for_the_registration_and_come_in_the_deliveries_fixture_shape() -> TestResult {
    let lab = Lab::start().await?;
    let trouter = lab.mock.start_trouter()?;
    let token = lab.mock.token(LAB_B).ok_or("no b")?;
    let (mut socket, connected) = connect_as(&trouter, &token).await?;
    let id = lab
        .post(LAB_A, AB_CHAT, &text_message("<p>pushed</p>", "4444444444444444444"))
        .await?;
    // nothing is delivered before a registration names the endpoint
    write_frame(&mut socket, r#"5:1+::{"name":"ping"}"#).await?;
    assert_eq!(read_frame(&mut socket).await?, r#"6:::1+["pong"]"#);

    let push = connected["surl"].as_str().ok_or("no surl")?;
    let registration = registration_body("TeamsCDLWebWorker", push);
    assert_eq!(lab.register(Some(&token), &registration).await?, 202);
    let frame = read_frame(&mut socket).await?;
    let delivery: Value = serde_json::from_str(frame.strip_prefix("3:::").ok_or("not a delivery")?)?;
    let recorded = recorded_frame(
        &trouter_fixture("deliveries.json")?["deliveries"],
        "a's new message",
        "3:::",
    )?;
    assert_eq!(keys(&delivery), keys(&recorded));
    let mut got_headers = keys(&delivery["headers"]);
    let mut recorded_headers = keys(&recorded["headers"]);
    got_headers.sort();
    recorded_headers.sort();
    assert_eq!(got_headers, recorded_headers);
    let endpoint = connected["id"].as_str().ok_or("no id")?;
    assert_eq!(delivery["url"], format!("/v4/f/{endpoint}/messaging"));
    let body: Value = serde_json::from_str(delivery["body"].as_str().ok_or("no body")?)?;
    assert_eq!(
        (body["type"].as_str(), body["resourceType"].as_str()),
        (Some("EventMessage"), Some("NewMessage"))
    );
    assert_eq!(body["resource"]["id"].as_str(), Some(id.as_str()));
    assert_eq!(body["resource"]["to"], AB_CHAT);
    assert_eq!(body["resource"]["content"], "<p>pushed</p>");

    // a read receipt has no captured delivery to another member: it stays queued
    let horizon = format!(
        "users/ME/conversations/{}/properties?name=consumptionhorizon",
        segment(AB_CHAT)
    );
    let receipt = json!({"consumptionhorizon": format!("{id};{id};4444444444444444444")});
    let answer = lab.send(LAB_A, Method::PUT, &horizon, Some(&receipt)).await?;
    assert_eq!(answer.status, 200);
    write_frame(&mut socket, r#"5:2+::{"name":"ping"}"#).await?;
    assert_eq!(read_frame(&mut socket).await?, r#"6:::2+["pong"]"#);
    assert_eq!(kinds(&lab.mock.take_events(LAB_B)), [EventKind::ReadReceipt]);
    Ok(())
}

#[tokio::test]
async fn the_calling_service_takes_the_accounts_skypetoken_alone_and_answers_a_start_with_its_conversation()
-> TestResult {
    let lab = Lab::start().await?;
    let start = format!("{}/api/v2/epconv", lab.mock.origin());
    let body = json!({"participants": {"from": {"id": LAB_A}, "to": [{"id": LAB_B}]}, "groupChat": null});
    let bearer = format!("Bearer {}", lab.mock.token(LAB_A).ok_or("no a")?);
    let skypetoken = lab.mock.skypetoken(LAB_A).ok_or("no a")?;
    for (case, headers) in [
        ("no credential", vec![]),
        (
            "the chat service's bearer token",
            vec![("Authorization", bearer.as_str())],
        ),
        (
            "a skypetoken the mock didn't hand out",
            vec![("X-Skypetoken", "mock-skypetoken-unknown")],
        ),
    ] {
        let refused = lab.send_raw(Method::POST, &start, &headers, Some(&body)).await?;
        assert_eq!((refused.status, refused.text.as_str()), (401, ""), "{case}");
    }
    assert!(lab.mock.calls().is_empty(), "nothing started");

    let headers = [("X-Skypetoken", skypetoken.as_str())];
    let answer = lab.send_raw(Method::POST, &start, &headers, Some(&body)).await?;
    assert_eq!(answer.status, 200);
    let conversation = answer.json()?;
    let controller = conversation["conversationController"]
        .as_str()
        .ok_or("no conversation")?;
    assert!(controller.starts_with("https://api.flightproxy.teams.microsoft.com/api/v2/ep/"));
    assert!(conversation["links"]["leave"].is_string());
    let calls = lab.mock.calls();
    let [call] = calls.as_slice() else {
        return Err(format!("one call: {calls:#?}").into());
    };
    assert_eq!((call.caller.as_str(), call.legs[0].callee.as_str()), (LAB_A, LAB_B));
    assert_eq!(call.requests, [("start".to_owned(), LAB_A.to_owned())]);

    let unknown = format!(
        "{}/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/forked/NoSuchCall/1/i1/1/attach",
        lab.mock.origin()
    );
    let missing = lab.send_raw(Method::POST, &unknown, &headers, Some(&json!({}))).await?;
    assert_eq!(missing.status, 404, "a link of no call");
    Ok(())
}

/// Made-up people join a running group call, one shares and stops, one leaves; nothing is made up in a call that isn't
/// a group call or isn't there. (What a client hears of them: `crates/ngteams/tests/large_call_window.rs`.)
#[tokio::test]
async fn made_up_people_join_share_and_leave_only_a_group_call() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let people = [
        ("8:orgid:00000000-0000-4000-8000-00000000a001", "Ann Able"),
        ("8:orgid:00000000-0000-4000-8000-00000000a002", "Ben Able"),
    ];
    assert!(!mock.join_made_up("NoSuchCall", &people));
    let one_to_one = mock.ring(LAB_B, LAB_A);
    assert!(!mock.join_made_up(&one_to_one, &people), "a 1:1 call");
    let group = mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]);
    assert!(mock.join_made_up(&group, &people));
    assert!(mock.share_made_up(&group, people[0].0, true));
    assert!(mock.share_made_up(&group, people[0].0, false));
    assert!(!mock.share_made_up(&group, OUTSIDER, true), "not in the call");
    assert!(!mock.share_made_up(&one_to_one, people[0].0, true));
    assert!(!mock.share_made_up("NoSuchCall", people[0].0, true));
    assert!(mock.leave_made_up(&group, people[1].0));
    assert!(!mock.leave_made_up(&group, people[1].0), "left already");
    assert!(!mock.share_made_up(&group, people[1].0, true), "gone");
    Ok(())
}

#[path = "support/helpers.rs"]
mod helpers;

#[tokio::test]
async fn activity_settings_are_json_text_owned_by_the_caller() -> TestResult {
    let lab = Lab::start().await?;
    let route = "users/ME/properties?name=userGraphSettings";
    let settings = json!({"made-up-app": {"toastSettings": {"systemDefault": false}}});
    let answer = lab
        .send(
            LAB_A,
            Method::PUT,
            route,
            Some(&json!({"userGraphSettings": settings.to_string()})),
        )
        .await?;
    assert_eq!(answer.status, 200);
    assert_eq!(
        lab.get(LAB_A, "users/ME/properties").await?["userGraphSettings"],
        settings.to_string()
    );
    assert_eq!(
        lab.send(LAB_B, Method::GET, "users/ME/properties", None).await?.status,
        404
    );
    for body in [
        json!({"userGraphSettings": {}}),
        json!({"userGraphSettings": "[]"}),
        json!({"userGraphSettings": "broken"}),
    ] {
        assert_eq!(lab.send(LAB_A, Method::PUT, route, Some(&body)).await?.status, 400);
    }
    assert_eq!(
        lab.get(LAB_A, "users/ME/properties").await?["userGraphSettings"],
        settings.to_string()
    );
    Ok(())
}

#[tokio::test]
async fn the_made_up_app_s_tile_is_served_on_the_cdn_s_path_as_a_picture() -> TestResult {
    let lab = Lab::start().await?;
    let token = Arc::new(MockToken(lab.mock.token(LAB_A).ok_or("no a")?));
    let client = Client::new(HttpTransport::with_mock_origin(token, CHAT_BASE, &lab.mock.origin())?);
    let tile = teams_proto::AppPicture::of(
        "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-updates_largeImage.png?v=1",
    )
    .ok_or("an app tile's URL")?;
    let answer = client.read(&tile).await?;
    let picture = tile.read_answer(&answer).ok_or("a picture")?;
    assert_eq!(picture.format, teams_proto::PastedFormat::Png);
    let decoded = image::load_from_memory(&picture.bytes)?;
    assert_eq!((decoded.width(), decoded.height()), (192, 192));
    // another app's tile isn't there
    let other = teams_proto::AppPicture::of("https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/other.png")
        .ok_or("an app tile's URL")?;
    assert!(client.read(&other).await.is_err());
    Ok(())
}
