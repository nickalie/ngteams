//! Substrate's search (`docs/protocol/search.md`): the two requests as the web client sends them, and their answers
//! read into what the app shows. Every answer here is written by hand in the captured shape, with made-up values.

use serde_json::{Value, json};
use teams_proto::{Found, SearchKind, Service, SubstrateSearch, highlighted};

const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const TEAM: &str = "19:made-up-general@thread.tacv2";

#[test]
fn the_suggestions_ask_every_kind_on_the_powerbar_scenario() {
    let ask = SubstrateSearch::suggestions("  lab ");
    assert_eq!(ask.service(), Service::Substrate);
    let endpoint = ask.endpoint();
    assert_eq!(endpoint.segments(), ["search", "api", "v1", "suggestions"]);
    assert_eq!(endpoint.query(), [("scenario".to_owned(), "powerbar".to_owned())]);
    let body = ask.body();
    let kinds: Vec<&str> = body["EntityRequests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| request["EntityType"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["People", "File", "Chat", "Team", "Channel", "Message"]);
    assert_eq!(body["EntityRequests"][0]["Query"]["QueryString"], "lab", "trimmed");
    assert_eq!(body["Scenario"]["Name"], "powerbar");
    assert_eq!(body["AppName"], "Microsoft Teams");
    // the correlation ids are fresh GUIDs, one per ask
    let cvid = body["Cvid"].as_str().unwrap();
    assert_eq!(cvid.len(), 36);
    assert_ne!(SubstrateSearch::suggestions("lab").body()["Cvid"], body["Cvid"]);
}

#[test]
fn the_results_ask_one_request_per_kind_and_name_a_single_kind_s_tab() {
    let all = SubstrateSearch::query(
        "lab",
        &[SearchKind::Messages, SearchKind::People, SearchKind::Files],
        5,
        0,
    );
    assert_eq!(all.endpoint().segments(), ["searchservice", "api", "v2", "query"]);
    assert!(all.endpoint().query().is_empty());
    let body = all.body();
    let kinds: Vec<&str> = body["EntityRequests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| request["entityType"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["Message", "People", "File"]);
    assert_eq!(body["scenario"]["Dimensions"][0]["DimensionValue"], "All");
    let tab = SubstrateSearch::query("lab", &[SearchKind::Chats], 25, 25);
    let body = tab.body();
    assert_eq!(body["EntityRequests"][0]["entityType"], "Chat");
    assert_eq!(body["EntityRequests"][0]["size"], 25);
    assert_eq!(body["EntityRequests"][0]["from"], 25);
    assert_eq!(body["scenario"]["Dimensions"][0]["DimensionValue"], "GroupChats");
}

#[test]
fn the_all_tab_leaves_out_messages_deleted_on_the_client_and_the_messages_tab_asks_the_bare_query() {
    let all = SubstrateSearch::query(" lab ", &[SearchKind::Messages, SearchKind::People], 5, 0);
    let body = all.body();
    let query = &body["EntityRequests"][0]["query"];
    assert_eq!(query["queryString"], "NOT (isClientSoftDeleted:TRUE) AND lab");
    assert_eq!(query["displayQueryString"], "lab");
    assert_eq!(
        body["EntityRequests"][1]["query"]["queryString"], "lab",
        "people asked plainly"
    );
    let messages = SubstrateSearch::query("lab", &[SearchKind::Messages], 25, 0);
    let query = &messages.body()["EntityRequests"][0]["query"];
    assert_eq!(query["queryString"], "lab");
    assert_eq!(query["displayQueryString"], "lab");
}

fn message_result(id: &str, conversation: &str, summary: &str) -> Value {
    json!({
        "Type": "Message",
        "HitHighlightedSummary": summary,
        "Source": {
            "ClientThreadId": conversation,
            "InternetMessageId": id,
            "DateTimeSent": "2026-01-01T12:00:00.000Z",
            "DisplayTo": "Lab Bee",
            "Sender": { "EmailAddress": { "Name": "Lab Sea", "Address": "sea@lab.example.com" } },
            "Extensions": {
                "SkypeSpaces_ConversationPost_Extension_FromSkypeInternalId": B,
                "SkypeSpaces_ConversationPost_Extension_ParentMessageId": id,
            },
        },
    })
}

#[test]
fn a_results_answer_reads_into_messages_people_files_chats_and_teams() {
    let answer = json!({
        "ApiVersion": "2.0",
        "EntitySets": [
            { "ResultSets": [{ "Total": 3, "MoreResultsAvailable": true, "Results": [
                message_result("1767268800000", AB, "the <c0>zeppelin</c0> plan"),
                // no id the chat service knows: left out
                message_result("not-a-number", AB, "x"),
            ] }] },
            { "ResultSets": [{ "Results": [{ "Type": "People", "Source": {
                "DisplayName": "Lab Bee", "MRI": B, "UserPrincipalName": "bee@lab.example.com",
                "JobTitle": "Tester", "EmailAddresses": ["BEE@lab.example.com", "bee.alias@lab.example.com"],
            } }] }] },
            { "ResultSets": [{ "Results": [{ "Type": "File", "HitHighlightedSummary": "<ddd/> the <c0>plan</c0>",
                "Source": {
                    "FileName": "Plan.docx", "FileType": "docx",
                    "Path": "https://contoso.example.com/sites/lab/Plan.docx",
                    "LastModifiedTime": "2026-01-01T12:00:00Z", "ModifiedBy": "Lab Sea",
                    "SiteTitle": "Lab site",
                } }] }] },
            { "ResultSets": [{ "Results": [{ "Type": "Chat", "Source": {
                "ThreadId": TRIO, "Members": ["Lab Bee", "Lab Sea"],
            } }] }] },
            { "ResultSets": [{ "Results": [{ "Type": "Team", "HitHighlightedSummary": "<c0>Lab</c0> team",
                "Source": { "Name": "Lab team", "ThreadId": TEAM } }] }] },
        ],
    });
    let ask = SubstrateSearch::query("zeppelin", &[SearchKind::Messages], 25, 0);
    let found = ask.read_answer(answer.to_string().as_bytes()).unwrap();
    assert!(found.more);
    the_message_reads(&found);
    the_others_read(&found);
}

fn the_message_reads(found: &Found) {
    assert_eq!(found.messages.len(), 1);
    let message = &found.messages[0];
    assert_eq!(message.conversation, AB);
    assert_eq!(message.id, "1767268800000");
    assert_eq!(message.sent_ms, 1_767_268_800_000);
    assert_eq!(message.sender_mri.as_deref(), Some(B));
    assert_eq!(message.sender_name.as_deref(), Some("Lab Sea"));
    assert_eq!(message.snippet, "the zeppelin plan");
    assert_eq!(&message.snippet[message.matched[0].clone()], "zeppelin");
}

fn the_others_read(found: &Found) {
    assert_eq!(found.people.len(), 1);
    assert_eq!(found.people[0].mri.as_deref(), Some(B));
    assert_eq!(found.people[0].email.as_deref(), Some("bee@lab.example.com"));
    assert_eq!(found.people[0].upn.as_deref(), Some("bee@lab.example.com"));
    assert_eq!(
        found.people[0].addresses,
        ["bee@lab.example.com", "bee.alias@lab.example.com"]
    );
    assert_eq!(found.files.len(), 1);
    assert_eq!(found.files[0].url, "https://contoso.example.com/sites/lab/Plan.docx");
    assert_eq!(found.files[0].location.as_deref(), Some("Lab site"));
    assert_eq!(found.files[0].snippet, "… the plan");
    assert_eq!(found.chats.len(), 1);
    assert_eq!(
        found.chats[0].name, "Lab Bee, Lab Sea",
        "a chat with no topic named by its members"
    );
    assert_eq!(found.channels.len(), 1);
    assert_eq!(found.channels[0].id, TEAM);
    assert_eq!(found.channels[0].team, None, "a team itself");
}

#[test]
fn a_suggestions_answer_reads_its_groups() {
    let answer = json!({ "Groups": [
        { "Type": "People", "Suggestions": [
            { "DisplayName": "Lab Bee", "MRI": B, "UserPrincipalName": "bee@lab.example.com" },
            // no name: left out
            { "MRI": B },
        ] },
        { "Type": "Chat", "Suggestions": [{
            "ThreadId": TRIO,
            "MatchingMembers": [{ "DisplayName": "Lab Bee", "MRI": B }],
            "ChatMembers": [{ "DisplayName": "Lab Sea" }],
            "EntityType": "ChatSuggestion",
        }] },
        { "Type": "Team", "Suggestions": [{ "Name": "Lab team", "ThreadId": TEAM, "EntityType": "TeamSuggestion" }] },
        { "Type": "Event", "Suggestions": [{ "Subject": "lab meeting" }] },
        { "Type": "Text", "Suggestions": [{ "Text": "Messages from Lab Bee" }] },
    ] });
    let found = SubstrateSearch::suggestions("lab")
        .read_answer(answer.to_string().as_bytes())
        .unwrap();
    assert_eq!(found.people.len(), 1);
    assert_eq!(found.people[0].name, "Lab Bee");
    assert_eq!(found.chats.len(), 1);
    assert_eq!(found.chats[0].name, "Lab Bee, Lab Sea");
    assert_eq!(found.channels.len(), 1);
    assert!(found.messages.is_empty() && found.files.is_empty());
}

#[test]
fn an_answer_of_another_shape_is_refused() {
    let ask = SubstrateSearch::suggestions("lab");
    assert!(ask.read_answer(b"not json").is_err());
    assert!(ask.read_answer(b"{\"Something\": []}").is_err());
}

#[test]
fn marked_text_becomes_plain_text_and_the_ranges_of_its_matches() {
    let (text, matched) = highlighted("a&#39;s <c0>Lab</c0>  &lt;team&gt; <b>x</b><br>y&unknown;");
    assert_eq!(text, "a's Lab <team> x y&unknown;");
    assert_eq!(&text[matched[0].clone()], "Lab");
    // an open mark never closed marks nothing
    let (text, matched) = highlighted("<c0>open");
    assert_eq!(text, "open");
    assert!(matched.is_empty());
}
