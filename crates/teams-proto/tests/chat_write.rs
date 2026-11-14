//! Typed chat requests against synthetic mocks: rich-text serialization, captured wire bodies, input validation and one send per write.
#![cfg(test)]

use std::collections::BTreeSet;
use std::future::{self, Future};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_mock::{AB_CHAT, ABC_CHAT, AC_CHAT, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, OUTSIDER};
use teams_proto::Mri;
use teams_proto::chat::{
    Code, CodeRun, ComposeError, Image, List, MAX_CONTENT, MAX_DEPTH, Marks, MessageText, Node, Quote, RichText,
    parse_html,
};
use teams_proto::{
    AddChatMembers, Author, CreateChat, Credentials, DeleteMessage, EditMessage, HttpTransport, Original, ReadReceipt,
    RemoveReaction, Response, SendMessage, SetReaction, SharedHistory, Transport, Typing,
};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");

/// 2026-01-01T00:00:00Z in milliseconds, the day the fixtures' made-up times are on.
const NEW_YEAR_MS: i64 = 1_767_225_600_000;

/// The path of a chat service request on the mock's chat base, `v1` included.
const BASE_PATH: &str = "/api/chatsvc/amer/v1";

fn oid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// The mock's group chat id without its `19:` and `@thread.v2`: what a fixture's `<group>` stands for.
fn group_inner() -> &'static str {
    ABC_CHAT.trim_start_matches("19:").trim_end_matches("@thread.v2")
}

/// A fixture with its placeholders filled in with the mock's made-up ids.
fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name))?;
    let text = text
        .replace("<region>", "amer")
        .replace("<guid:a>", oid(LAB_A))
        .replace("<guid:b>", oid(LAB_B))
        .replace("<guid:x>", oid(OUTSIDER))
        .replace("<group>", group_inner());
    Ok(serde_json::from_str(&text)?)
}

/// The case of `writes.json` named `name`, its path made a full one under the chat base.
fn writes_case(name: &str) -> TestResult<Value> {
    let writes = fixture("writes.json")?;
    let mut case = writes["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["name"] == name))
        .ok_or("no such case")?
        .clone();
    let path = format!("{BASE_PATH}{}", case["path"].as_str().ok_or("no path")?);
    case["path"] = json!(path);
    Ok(case)
}

fn mri(text: &str) -> Mri {
    Mri::new(text)
}

fn lab_a() -> Author {
    Author::new(mri(LAB_A), "Lab A")
}

fn rich(nodes: Vec<Node>) -> RichText {
    RichText {
        nodes,
        truncated: false,
    }
}

fn run(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

fn mention(item_id: &str, name: &str) -> Node {
    Node::Mention {
        item_id: item_id.to_owned(),
        text: name.to_owned(),
    }
}

fn plain(words: &str) -> MessageText {
    MessageText::plain(words).unwrap()
}

/// "Lab B\u{a0}a made-up mention", mentioning b: `mention.json`'s text (a no-break space after the mention, as the web
/// client wrote it).
fn mentioning_b() -> MessageText {
    mentioning(LAB_B, "Lab B")
}

fn mentioning(who: &str, name: &str) -> MessageText {
    MessageText::new(
        rich(vec![Node::Paragraph(vec![
            mention("m", name),
            run("\u{a0}a made-up mention"),
        ])]),
        [("m".to_owned(), mri(who))],
    )
    .unwrap()
}

/// `reply.json`'s text: a's message quoted, and the reply.
fn reply() -> MessageText {
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![Node::Paragraph(vec![run("a made-up message, edited")])],
        forward: false,
    };
    MessageText::new(
        rich(vec![Node::Quote(quote), Node::Paragraph(vec![run("a made-up reply")])]),
        [],
    )
    .unwrap()
}

/// One chat write of any kind.
enum ChatWrite {
    Send(SendMessage),
    Edit(EditMessage),
    Delete(DeleteMessage),
    React(SetReaction),
    Unreact(RemoveReaction),
    Receipt(ReadReceipt),
    Typing(Typing),
    Create(CreateChat),
    Add(AddChatMembers),
}

impl ChatWrite {
    /// Sends the typed request through the transport.
    async fn deliver<T: Transport>(self, transport: &T) -> Result<Response, T::Error> {
        match self {
            Self::Send(request) => transport.deliver(request).await,
            Self::Edit(request) => transport.deliver(request).await,
            Self::Delete(request) => transport.deliver(request).await,
            Self::React(request) => transport.deliver(request).await,
            Self::Unreact(request) => transport.deliver(request).await,
            Self::Receipt(request) => transport.deliver(request).await,
            Self::Typing(request) => transport.deliver(request).await,
            Self::Create(request) => transport.deliver(request).await,
            Self::Add(request) => transport.deliver(request).await,
        }
    }
}

fn original() -> Original {
    Original {
        client_message_id: "1111111111111111111".to_owned(),
        compose_time_ms: NEW_YEAR_MS + 10_000,
        arrival_time_ms: NEW_YEAR_MS + 10_000,
    }
}

/// One write of each kind on chat `id`; the message and edit mention b.
fn every_write(id: &str, message: &str) -> Vec<ChatWrite> {
    vec![
        ChatWrite::Send(SendMessage::new(id, lab_a(), mentioning_b())),
        ChatWrite::Typing(Typing::new(id)),
        ChatWrite::Edit(EditMessage::new(id, message, lab_a(), mentioning_b(), original())),
        ChatWrite::React(SetReaction::new(id, message, "like")),
        ChatWrite::Unreact(RemoveReaction::new(id, message, "like")),
        ChatWrite::Receipt(ReadReceipt::new(id, message, "1111111111111111111")),
        ChatWrite::Delete(DeleteMessage::new(id, message)),
    ]
}

// --- one rich-text value ---

#[test]
fn a_mention_is_in_the_content_and_the_target_from_one_value() {
    let text = mentioning_b();
    assert_eq!(
        text.html(),
        "<p><readonly class=\"skipProofing\" itemtype=\"http://schema.skype.com/Mention\" spellcheck=\"false\"><span \
         itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Lab B</span></readonly>&nbsp;a made-up \
         mention</p>"
    );
    assert_eq!(text.participants(), &BTreeSet::from([mri(LAB_B)]));
    assert_eq!(text.mentioned().get("m"), Some(&mri(LAB_B)));
}

#[test]
fn a_list_is_written_as_html_and_reads_back_the_same() {
    let list = Node::List(List {
        ordered: true,
        start: 3,
        items: vec![
            vec![run("three")],
            vec![
                run("four"),
                Node::List(List {
                    ordered: false,
                    start: 1,
                    items: vec![vec![run("inner")]],
                }),
            ],
        ],
    });
    let text = MessageText::new(rich(vec![list.clone()]), []).unwrap();
    assert_eq!(
        text.html(),
        "<ol start=\"3\"><li>three</li><li>four<ul><li>inner</li></ul></li></ol>"
    );
    assert_eq!(parse_html(text.html()).nodes, [list]);
}

#[test]
fn a_code_block_is_written_as_the_editor_writes_one_and_reads_back_the_same() {
    let code = |language: Option<&str>, text: &str| {
        Node::Code(Code {
            language: language.map(str::to_owned),
            runs: vec![CodeRun {
                text: text.to_owned(),
                token: teams_proto::chat::Token::Plain,
            }],
        })
    };
    let written = "fn x() {\n    <y> & \"z\"\n}";
    let text = MessageText::new(rich(vec![code(Some("rust"), written)]), []).unwrap();
    assert_eq!(
        text.html(),
        "<pre class=\"language-rust\"><code>fn&nbsp;x()&nbsp;{<br>&nbsp;&nbsp;&nbsp;&nbsp;&lt;y&gt;&nbsp;&amp;&nbsp;\
         &quot;z&quot;<br>}</code></pre>"
    );
    assert_eq!(parse_html(text.html()).nodes, [code(Some("rust"), written)]);
    // a code ending in a blank line, or two, reads back with them
    for ending in ["a\n", "a\n\n", "\n"] {
        let text = MessageText::new(rich(vec![code(None, ending)]), []).unwrap();
        assert_eq!(parse_html(text.html()).nodes, [code(None, ending)], "{ending:?}");
    }
    // a language id of another shape is left out
    let odd = MessageText::new(rich(vec![code(Some("\"><b>"), "x")]), []).unwrap();
    assert_eq!(odd.html(), "<pre><code>x</code></pre>");
}

#[test]
fn mentions_are_numbered_as_they_appear() {
    let text = MessageText::new(
        rich(vec![Node::Paragraph(vec![
            mention("second", "C"),
            run(" and "),
            mention("first", "B"),
        ])]),
        [("first".to_owned(), mri(LAB_B)), ("second".to_owned(), mri(LAB_C))],
    )
    .unwrap();
    let html = text.html();
    assert!(html.contains("itemid=\"0\">C</span>"), "{html}");
    assert!(html.contains("itemid=\"1\">B</span>"), "{html}");
    assert_eq!(text.participants(), &BTreeSet::from([mri(LAB_B), mri(LAB_C)]));
}

#[test]
fn a_mention_must_name_someone_and_be_named_once() {
    let one = |nodes: Vec<Node>, mentioned: Vec<(&str, &str)>| {
        MessageText::new(
            rich(vec![Node::Paragraph(nodes)]),
            mentioned
                .into_iter()
                .map(|(item_id, who)| (item_id.to_owned(), mri(who))),
        )
    };
    assert_eq!(
        one(vec![mention("0", "B")], vec![]),
        Err(ComposeError::UnknownMention("0".to_owned()))
    );
    assert_eq!(
        one(vec![run("nobody")], vec![("0", LAB_B)]),
        Err(ComposeError::UnusedMention("0".to_owned())),
        "a mentioned MRI the text doesn't show would be in the target only"
    );
    assert_eq!(
        one(vec![mention("0", "B"), mention("0", "B")], vec![("0", LAB_B)]),
        Err(ComposeError::RepeatedMention("0".to_owned()))
    );
    assert_eq!(
        one(vec![mention("0", "B")], vec![("0", LAB_B), ("0", LAB_C)]),
        Err(ComposeError::RepeatedMention("0".to_owned()))
    );
    for who in [
        "28:bot-service",
        "8:orgid:",
        "8:orgid:not hex",
        LAB_B.trim_start_matches("8:orgid:"),
    ] {
        assert_eq!(
            one(vec![mention("0", "B")], vec![("0", who)]),
            Err(ComposeError::NotAPerson(mri(who))),
            "{who}"
        );
    }
}

#[test]
fn what_isnt_a_messages_text_is_refused() {
    let image = Node::Image(Image {
        src: "https://example.com/image.png".to_owned(),
        alt: None,
        item_type: None,
        ..Image::default()
    });
    assert_eq!(MessageText::new(rich(vec![image]), []), Err(ComposeError::Image));
    for href in ["javascript:alert(1)", "data:text/html,hi", "not a url"] {
        let link = Node::Link {
            href: href.to_owned(),
            children: vec![run("x")],
        };
        assert_eq!(
            MessageText::new(rich(vec![link]), []),
            Err(ComposeError::LinkScheme),
            "{href}"
        );
    }
    let incomplete = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        ..Quote::default()
    };
    assert_eq!(
        MessageText::new(rich(vec![Node::Quote(incomplete)]), []),
        Err(ComposeError::Quote)
    );
    let mut truncated = rich(vec![run("cut")]);
    truncated.truncated = true;
    assert_eq!(MessageText::new(truncated, []), Err(ComposeError::Truncated));

    let mut deep = run("deep");
    for _ in 0..=MAX_DEPTH {
        deep = Node::Paragraph(vec![deep]);
    }
    assert_eq!(MessageText::new(rich(vec![deep]), []), Err(ComposeError::TooDeep));
    assert_eq!(MessageText::plain(&"x".repeat(MAX_CONTENT)), Err(ComposeError::TooLong));
}

/// A reply quoting a message whose preview is `levels` paragraphs deep.
fn reply_previewing(levels: usize) -> Result<MessageText, ComposeError> {
    let mut preview = run("deep");
    for _ in 0..levels {
        preview = Node::Paragraph(vec![preview]);
    }
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![preview],
        forward: false,
    };
    MessageText::new(rich(vec![Node::Quote(quote)]), [])
}

#[test]
fn a_replys_preview_is_held_to_the_same_depth_as_the_rest() {
    // the preview is sent as its plain text, but kept as given: too deep a preview could be stored and not read back
    for levels in [MAX_DEPTH, 128] {
        assert_eq!(reply_previewing(levels), Err(ComposeError::TooDeep), "{levels} deep");
    }
    let deepest = reply_previewing(MAX_DEPTH - 1).unwrap();
    let stored = serde_json::to_string(&deepest).unwrap();
    assert_eq!(serde_json::from_str::<MessageText>(&stored).unwrap(), deepest);
}

#[test]
fn text_is_escaped_so_no_markup_comes_from_it() {
    let text = plain("<script>alert(1)</script> & \"<b>\"");
    assert_eq!(
        text.html(),
        "<p>&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;&lt;b&gt;&quot;</p>"
    );
    let name = MessageText::new(
        rich(vec![mention("0", "<img src=x onerror=alert(1)>")]),
        [("0".to_owned(), mri(LAB_B))],
    )
    .unwrap();
    assert!(!name.html().contains("<img"), "{}", name.html());
}

#[test]
fn what_is_sent_reads_back_as_the_same_rich_text() {
    let nodes = vec![
        Node::Paragraph(vec![
            Node::Text {
                text: "bold".to_owned(),
                marks: Marks::BOLD,
            },
            Node::Text {
                text: " and code".to_owned(),
                marks: Marks::ITALIC.with(Marks::CODE),
            },
            Node::Break,
            mention("0", "Lab B"),
            run(" see "),
            Node::Link {
                href: "https://example.com/a?b=c&d=e".to_owned(),
                children: vec![run("this")],
            },
        ]),
        Node::Quote(Quote {
            body: vec![Node::Paragraph(vec![run("a plain quote")])],
            ..Quote::default()
        }),
    ];
    let text = MessageText::new(rich(nodes.clone()), [("0".to_owned(), mri(LAB_B))]).unwrap();
    assert_eq!(parse_html(text.html()), rich(nodes));

    let reply = reply();
    assert_eq!(&parse_html(reply.html()), reply.rich_text());
}

#[test]
fn formatted_text_is_written_as_teams_editor_writes_it_and_reads_back() {
    let style = teams_proto::chat::SpanStyle {
        color: Some(teams_proto::chat::Rgb(0x1E_53A3)),
        highlight: Some(teams_proto::chat::Rgb(0xEB_D3E1)),
        size: Some(teams_proto::chat::FontSize::Large),
    };
    let nodes = vec![
        Node::Heading {
            level: 2,
            children: vec![run("Plan")],
        },
        Node::Paragraph(vec![Node::Span {
            style,
            children: vec![Node::Text {
                text: "bold".to_owned(),
                marks: Marks::BOLD,
            }],
        }]),
        Node::Indented {
            depth: 1,
            children: vec![run("in")],
        },
        Node::Rule,
    ];
    let text = MessageText::new(rich(nodes.clone()), []).unwrap();
    assert_eq!(
        text.html(),
        "<h2>Plan</h2><p><span style=\"background-color:#EBD3E1; color:#1E53A3; font-size:x-large\">\
         <strong>bold</strong></span></p><p style=\"margin-left:40px\">in</p><hr>"
    );
    assert_eq!(parse_html(text.html()), rich(nodes));
    // what the editor writes for Medium, and a colour of no form read, give nothing
    assert_eq!(
        parse_html("<p><span style=\"font-size:inherit; color:var(--x)\">x</span></p>"),
        rich(vec![Node::Paragraph(vec![run("x")])])
    );
}

#[test]
fn a_stored_text_is_checked_again_when_read() {
    let text = mentioning_b();
    let stored = serde_json::to_string(&text).unwrap();
    assert_eq!(serde_json::from_str::<MessageText>(&stored).unwrap(), text);

    let mut tampered: Value = serde_json::from_str(&stored).unwrap();
    tampered["mentioned"] = json!({});
    assert!(
        serde_json::from_value::<MessageText>(tampered).is_err(),
        "a mention naming nobody"
    );
}

#[test]
fn a_stored_text_reports_an_invalid_account_origin() {
    let text = MessageText::plain("no image").unwrap();
    for origin in ["http://chat.example", "https://chat.example/sp/", "not a URL"] {
        let mut stored = serde_json::to_value(&text).unwrap();
        stored["origin"] = origin.into();
        let error = serde_json::from_value::<MessageText>(stored).unwrap_err();
        assert_eq!(error.to_string(), ComposeError::Origin.to_string(), "{origin}");
    }
}

// --- the bodies sent, as captured ---

/// The synthetic token given to the transport; `None` represents a signed-out account.
#[derive(Default)]
struct Token {
    current: Mutex<Option<String>>,
}

impl Token {
    fn new(token: Option<String>) -> Self {
        Self {
            current: Mutex::new(token),
        }
    }
}

impl Credentials for Token {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        let token = self.current.lock().unwrap_or_else(PoisonError::into_inner).clone();
        future::ready(token.map(Secret::new).ok_or(TokenError::NotSignedIn {
            account: "lab-a".to_owned(),
        }))
    }

    fn bearer_rejected(
        &self,
        audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// What the transport sent: the method, the decoded path, the query and the body.
#[derive(Debug, PartialEq)]
struct Sent {
    method: String,
    path: String,
    query: Option<String>,
    body: Option<Value>,
}

/// Sends `write` through the native transport to a server that answers
/// everything with 200, and returns what reached it.
async fn sent(write: ChatWrite) -> TestResult<Sent> {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let token = Arc::new(Token::new(Some("a-made-up-token-for-the-transport".to_owned())));
    let transport = HttpTransport::with_mock_origin(token, CHAT_BASE, &server.uri())?;

    write.deliver(&transport).await?;
    let requests = server.received_requests().await.unwrap_or_default();
    let [request] = requests.as_slice() else {
        return Err(format!("{} requests", requests.len()).into());
    };
    let body = (!request.body.is_empty())
        .then(|| serde_json::from_slice(&request.body))
        .transpose()?;
    Ok(Sent {
        method: request.method.as_str().to_owned(),
        path: percent_decode_str(request.url.path()).decode_utf8()?.into_owned(),
        query: request.url.query().map(str::to_owned),
        body,
    })
}

/// A recorded request (a fixture's `request`, or a `writes.json` case), as [`Sent`].
fn recorded(request: &Value) -> TestResult<Sent> {
    let path = request["path"].as_str().ok_or("no path")?;
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (path, request["query"].as_str().map(str::to_owned)),
    };
    Ok(Sent {
        method: request["method"].as_str().ok_or("no method")?.to_owned(),
        path: percent_decode_str(path).decode_utf8()?.into_owned(),
        query,
        body: Some(request["body"].clone()).filter(|body| !body.is_null()),
    })
}

/// A message body with its JSON-string properties (`mentions`, `qtdMsgs`) read, so their entries compare key for key.
fn read_listed(mut sent: Sent) -> TestResult<Sent> {
    if let Some(properties) = sent
        .body
        .as_mut()
        .and_then(|body| body.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        for key in ["mentions", "qtdMsgs"] {
            if let Some(value) = properties.get_mut(key) {
                *value = serde_json::from_str(value.as_str().ok_or("not a string")?)?;
            }
        }
    }
    Ok(sent)
}

#[tokio::test]
async fn a_message_with_a_mention_is_sent_as_the_mention_fixture() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), mentioning_b())
        .composed_at(NEW_YEAR_MS + 40_000)
        .with_client_message_id("6666666666666666666");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    assert_eq!(sent, read_listed(recorded(&fixture("mention.json")?["request"])?)?);
    Ok(())
}

/// "Everyone\u{a0}a made-up mention of everyone" in the mock's group chat: `mention-everyone.json`'s text.
fn mentioning_everyone() -> MessageText {
    MessageText::new(
        rich(vec![Node::Paragraph(vec![
            mention("e", "Everyone"),
            run("\u{a0}a made-up mention of everyone"),
        ])]),
        [("e".to_owned(), mri(ABC_CHAT))],
    )
    .unwrap()
}

#[tokio::test]
async fn a_mention_of_everyone_is_sent_as_the_everyone_fixture() -> TestResult {
    let request = SendMessage::new(ABC_CHAT, lab_a(), mentioning_everyone())
        .composed_at(NEW_YEAR_MS + 180_000)
        .with_client_message_id("7777777777777777777");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    assert_eq!(
        sent,
        read_listed(recorded(&fixture("mention-everyone.json")?["request"])?)?
    );
    Ok(())
}

#[tokio::test]
async fn a_mention_of_everyone_goes_only_to_the_chat_it_names() -> TestResult {
    let text = mentioning_everyone();
    assert!(text.participants().is_empty());
    assert_eq!(text.everyone(), &BTreeSet::from([ABC_CHAT.to_owned()]));
    assert!(text.mentions_everyone_only_in(ABC_CHAT));
    assert!(!text.mentions_everyone_only_in(AC_CHAT));
    // Posted or edited anywhere else, it has no wire form and the transport sends nothing.
    for write in [
        ChatWrite::Send(SendMessage::new(AC_CHAT, lab_a(), text.clone())),
        ChatWrite::Edit(EditMessage::new(AC_CHAT, "1", lab_a(), text.clone(), original())),
    ] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let token = Arc::new(Token::new(Some("a-made-up-token-for-the-transport".to_owned())));
        let transport = HttpTransport::with_mock_origin(token, CHAT_BASE, &server.uri())?;

        assert!(write.deliver(&transport).await.is_err());
        assert!(server.received_requests().await.unwrap_or_default().is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn a_quoted_reply_is_sent_as_the_reply_fixture() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), reply())
        .composed_at(NEW_YEAR_MS + 49_500)
        .with_client_message_id("4444444444444444444");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    assert_eq!(sent, read_listed(recorded(&fixture("reply.json")?["request"])?)?);
    Ok(())
}

#[tokio::test]
async fn a_plain_message_is_sent_as_the_captured_one() -> TestResult {
    let request = SendMessage::new(AB_CHAT, lab_a(), plain("made-up text"))
        .composed_at(NEW_YEAR_MS)
        .with_client_message_id("1111111111111111111");

    let sent = read_listed(sent(ChatWrite::Send(request)).await?)?;

    let case = writes_case("a message as the web client sends it, no mention (shape of ab-chat-full, 2026-10-20)")?;
    assert_eq!(sent, read_listed(recorded(&case)?)?);
    Ok(())
}

#[tokio::test]
async fn an_edit_is_sent_as_the_edit_fixture() -> TestResult {
    let new_text = MessageText::new(rich(vec![Node::Paragraph(vec![run("a made-up message, edited")])]), [])?;
    let edited_at = 1_760_000_020_000;
    let request = EditMessage::new(AB_CHAT, "1760000010000", lab_a(), new_text, original()).edited_at(edited_at);

    let sent = read_listed(sent(ChatWrite::Edit(request)).await?)?;

    let mut expected = read_listed(recorded(&fixture("edit.json")?["request"])?)?;
    // The fixture's made-up `version` and `edittime` aren't one instant; an edit sends one time as both.
    let body = expected.body.as_mut().ok_or("no body")?;
    body["version"] = json!("2025-10-09T08:53:40.000Z");
    body["properties"]["edittime"] = json!(edited_at);
    assert_eq!(sent, expected);
    Ok(())
}

#[tokio::test]
async fn preserving_attachments_cannot_restore_stale_quotes_mentions_or_edit_time() -> TestResult {
    let text = reply();
    let baseline = EditMessage::new(AB_CHAT, "1760000010000", lab_a(), text.clone(), original());
    let baseline = read_listed(sent(ChatWrite::Edit(baseline)).await?)?
        .body
        .ok_or("no body")?;
    let properties = serde_json::from_value(json!({
        "files": [{"futureFile": {"keep": [null, 7]}}],
        "cards": null,
        "mentions": "stale mentions",
        "qtdMsgs": "stale quotes",
        "edittime": "stale time",
        "emotions": "stale reactions"
    }))?;
    let request = EditMessage::new(AB_CHAT, "1760000010000", lab_a(), text, original())
        .with_original_attachments(properties)
        .edited_at(NEW_YEAR_MS);
    let sent = read_listed(sent(ChatWrite::Edit(request)).await?)?;
    let body = sent.body.ok_or("no body")?;
    assert_eq!(
        body["properties"]["files"],
        json!([{"futureFile": {"keep": [null, 7]}}])
    );
    assert_eq!(body["properties"]["cards"], Value::Null);
    assert_eq!(body["properties"]["qtdMsgs"], baseline["properties"]["qtdMsgs"]);
    assert_eq!(body["properties"]["mentions"], json!([]));
    assert_eq!(body["properties"]["edittime"], NEW_YEAR_MS);
    assert!(body["properties"].get("emotions").is_none());
    assert!(
        body["content"]
            .as_str()
            .ok_or("no content")?
            .contains("a made-up reply")
    );
    Ok(())
}

#[tokio::test]
async fn a_delete_is_sent_as_the_delete_fixture() -> TestResult {
    let sent = sent(ChatWrite::Delete(DeleteMessage::new(AB_CHAT, "1760000060000"))).await?;

    assert_eq!(sent, recorded(&fixture("delete.json")?["request"])?);
    Ok(())
}

#[tokio::test]
async fn a_reaction_and_taking_it_back_are_sent_as_the_reaction_fixture() -> TestResult {
    let reaction = fixture("reaction.json")?;
    let add = SetReaction::new(AB_CHAT, "1760000010000", "like").at(1_760_000_025_000);
    let remove = RemoveReaction::new(AB_CHAT, "1760000010000", "like");

    assert_eq!(
        sent(ChatWrite::React(add)).await?,
        recorded(&reaction["add"]["request"])?
    );
    assert_eq!(
        sent(ChatWrite::Unreact(remove)).await?,
        recorded(&reaction["remove"]["request"])?
    );
    Ok(())
}

#[tokio::test]
async fn a_read_receipt_typing_and_a_new_chat_are_sent_as_captured() -> TestResult {
    let receipt = ReadReceipt::new(AB_CHAT, "1760000000000", "1").at(1_760_000_000_000);
    assert_eq!(receipt.horizon(), "1760000000000;1760000000000;1");
    assert_eq!(
        sent(ChatWrite::Receipt(receipt)).await?,
        recorded(&writes_case("a read receipt")?)?
    );
    assert_eq!(
        sent(ChatWrite::Typing(Typing::new(AB_CHAT))).await?,
        recorded(&writes_case("typing")?)?
    );
    let create = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).ok_or("one chat")?;
    assert_eq!(
        sent(ChatWrite::Create(create)).await?,
        recorded(&writes_case("a new chat")?)?
    );
    Ok(())
}

#[tokio::test]
async fn a_new_group_chat_is_sent_as_the_web_client_created_one() -> TestResult {
    let create = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).ok_or("a group")?;

    let sent = sent(ChatWrite::Create(create)).await?;

    // `docs/protocol/fence-targets.md`: the group's create has no `fixedRoster` and no `uniquerosterthread`
    let members: Vec<Value> = [LAB_A, LAB_B, LAB_C]
        .iter()
        .map(|id| json!({"id": id, "role": "Admin"}))
        .collect();
    assert_eq!(
        sent,
        Sent {
            method: "POST".to_owned(),
            path: format!("{BASE_PATH}/threads"),
            query: None,
            body: Some(json!({"members": members, "properties": {"threadType": "chat"}})),
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_named_group_chat_carries_its_name_as_the_web_client_sent_it() -> TestResult {
    // New message's "Add group name" (2026-11-06): the name trimmed, as `properties.topic` beside the thread type
    let create = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)])
        .ok_or("a group")?
        .named("  Lab trio ");
    assert_eq!(create.topic(), Some("Lab trio"));

    let sent = sent(ChatWrite::Create(create)).await?;

    let members: Vec<Value> = [LAB_A, LAB_B, LAB_C]
        .iter()
        .map(|id| json!({"id": id, "role": "Admin"}))
        .collect();
    assert_eq!(
        sent.body,
        Some(json!({"members": members, "properties": {"threadType": "chat", "topic": "Lab trio"}}))
    );
    // a blank name names nothing, and a 1:1 chat takes none
    let blank = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)])
        .ok_or("a group")?
        .named("   ");
    assert_eq!(blank.topic(), None);
    let one = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B))
        .ok_or("one chat")?
        .named("Pair");
    assert_eq!(one.topic(), None);
    Ok(())
}

#[tokio::test]
async fn adding_people_to_a_group_chat_puts_them_with_the_history_chosen() -> TestResult {
    // browser Teams' group chat "Add" (2026-11-10): one PUT naming everyone added, each an Admin with the history
    // chosen as `shareHistoryTime`: -1 none, 0 all, else the time the shared history starts
    let since = NEW_YEAR_MS - 3 * 86_400_000;
    for (history, time) in [
        (SharedHistory::None, -1),
        (SharedHistory::All, 0),
        (SharedHistory::Since(since), since),
    ] {
        let add = AddChatMembers::new(ABC_CHAT, vec![mri(LAB_B), mri(LAB_C)], history).ok_or("an add")?;
        assert_eq!(add.conversation(), ABC_CHAT);
        assert_eq!(add.members(), [mri(LAB_B), mri(LAB_C)]);
        assert_eq!(add.history(), history);
        assert_eq!(SharedHistory::from_share_history_time(time), history);

        let sent = sent(ChatWrite::Add(add)).await?;

        assert_eq!(sent.method, "PUT");
        assert_eq!(sent.path, format!("{BASE_PATH}/threads/{ABC_CHAT}/members"));
        assert_eq!(sent.query, None);
        let members: Vec<Value> = [LAB_B, LAB_C]
            .iter()
            .map(|id| json!({"id": id, "role": "Admin", "shareHistoryTime": time}))
            .collect();
        assert_eq!(sent.body, Some(json!({"members": members})));
    }
    // nobody, or someone twice, is no add
    assert_eq!(AddChatMembers::new(ABC_CHAT, Vec::new(), SharedHistory::All), None);
    assert_eq!(
        AddChatMembers::new(ABC_CHAT, vec![mri(LAB_B), mri(LAB_B)], SharedHistory::All),
        None
    );
    // a time before the epoch is no time: the history from its start, never "none" by accident
    assert_eq!(SharedHistory::Since(0).share_history_time(), 1);
    assert_eq!(SharedHistory::from_share_history_time(-5), SharedHistory::None);
    Ok(())
}

// --- on the mock chat service ---

/// The mock chat service and a's native transport.
struct Lab {
    mock: ChatServiceMock,
    transport: Arc<HttpTransport<Token>>,
}

impl Lab {
    /// The mock transport with a's synthetic token.
    async fn start() -> TestResult<Self> {
        let mock = ChatServiceMock::start().await?;
        let token = mock.token(LAB_A).ok_or("no a")?;
        let lab = Self::without_credential(mock, Some(token))?;
        Ok(lab)
    }

    /// A mock transport with `token`, or a signed-out credential source.
    fn without_credential(mock: ChatServiceMock, token: Option<String>) -> TestResult<Self> {
        let token = Arc::new(Token::new(token));
        let transport = HttpTransport::with_mock_origin(Arc::clone(&token), CHAT_BASE, &mock.origin())?;
        Ok(Self {
            mock,
            transport: Arc::new(transport),
        })
    }

    /// Sends one typed write and returns its answer.
    async fn write(&self, write: ChatWrite) -> Result<Response, teams_proto::HttpError> {
        Ok(write.deliver(&*self.transport).await.unwrap())
    }

    /// How many member reads (`GET`s) reached the mock.
    async fn reads_received(&self) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "GET")
            .count()
    }

    /// How many writes (anything but a `GET`) reached the mock.
    async fn writes_received(&self) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .count()
    }
}

fn arrival(answer: &Response) -> TestResult<String> {
    Ok(serde_json::from_slice::<Value>(answer.body())?["OriginalArrivalTime"].to_string())
}

#[tokio::test]
async fn each_write_to_a_lab_chat_is_allowed_and_sent_once() -> TestResult {
    let lab = Lab::start().await?;
    let posted = lab
        .write(ChatWrite::Send(SendMessage::new(AB_CHAT, lab_a(), mentioning_b())))
        .await?;
    assert_eq!(posted.status(), 201);
    assert_eq!(lab.writes_received().await, 1);
    let id = arrival(&posted)?;

    let mut sent = 1;
    for write in every_write(AB_CHAT, &id).into_iter().skip(1) {
        let answer = lab.write(write).await?;
        sent += 1;
        assert!(answer.is_success(), "{answer:?}");
        assert_eq!(lab.writes_received().await, sent, "each write sent once");
    }
    let create = CreateChat::one_to_one(mri(LAB_A), mri(LAB_B)).ok_or("one chat")?;
    assert_eq!(lab.write(ChatWrite::Create(create)).await?.status(), 201);
    assert_eq!(lab.writes_received().await, sent + 1);

    let told: Vec<EventKind> = lab.mock.take_events(LAB_B).iter().map(|event| event.kind).collect();
    assert_eq!(
        told,
        [
            EventKind::Message,
            EventKind::Typing,
            EventKind::Edit,
            EventKind::Reaction,
            EventKind::Reaction,
            EventKind::ReadReceipt,
            EventKind::Delete,
        ],
        "b was told each write, and of no new chat: theirs with a exists"
    );
    Ok(())
}

#[tokio::test]
async fn a_lab_only_group_chat_is_written_to_a_new_one_included() -> TestResult {
    let lab = Lab::start().await?;
    for write in [
        ChatWrite::Send(SendMessage::new(ABC_CHAT, lab_a(), plain("hello"))),
        ChatWrite::Typing(Typing::new(ABC_CHAT)),
    ] {
        assert_eq!(lab.write(write).await?.status(), 201);
    }
    assert_eq!(lab.writes_received().await, 2);
    assert_eq!(lab.reads_received().await, 0, "writes need no membership reads");

    // a new group of lab accounts is judged by its members alone …
    let create = CreateChat::group(mri(LAB_A), [mri(LAB_B), mri(LAB_C)]).ok_or("a group")?;
    let created = lab.write(ChatWrite::Create(create)).await?;
    assert_eq!(created.status(), 201);
    let group = serde_json::from_slice::<Value>(created.body())?["threadResource"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    // … and written to like any chat, by its fresh member read
    let posted = lab
        .write(ChatWrite::Send(SendMessage::new(&group, lab_a(), plain("hello"))))
        .await?;
    assert_eq!(posted.status(), 201);
    assert_eq!(lab.writes_received().await, 4);

    Ok(())
}

/// A made-up pasted picture's object.
const PICTURE: &str = "0-eus-d10-00000000000000000000000000000001";

#[tokio::test]
async fn a_pasted_picture_is_posted_as_teams_writes_it_with_its_object_in_amsreferences() -> TestResult {
    let picture = teams_proto::sent_picture(PICTURE, (198, 53)).ok_or("an object id")?;
    let text = MessageText::new(
        rich(vec![
            Node::Paragraph(vec![run("Look")]),
            Node::Paragraph(vec![Node::Image(picture.clone())]),
        ]),
        [],
    )?;
    assert_eq!(text.pictures(), [PICTURE]);
    let request = SendMessage::new(AB_CHAT, lab_a(), text).composed_at(NEW_YEAR_MS);
    let sent = sent(ChatWrite::Send(request)).await?;
    let body = sent.body.ok_or("a body")?;
    assert_eq!(body["amsreferences"], json!([PICTURE]));
    assert_eq!(
        body["content"],
        format!(
            "<p>Look</p><p><img src=\"https://us-api.asm.skype.com/v1/objects/{PICTURE}/views/imgo\" \
             itemtype=\"http://schema.skype.com/AMSImage\" width=\"198\" height=\"53\" alt=\"image\" \
             id=\"x_{PICTURE}\" itemid=\"{PICTURE}\"></p>"
        )
    );
    // it reads back as the picture it was
    let read = parse_html(body["content"].as_str().ok_or("content")?);
    assert_eq!(read.nodes[1], Node::Paragraph(vec![Node::Image(picture)]));
    Ok(())
}

#[test]
fn only_a_picture_sent_for_the_message_is_in_its_text() {
    let good = teams_proto::sent_picture(PICTURE, (10, 10)).unwrap();
    let refused = [
        // another host
        Image {
            src: format!("https://example.com/v1/objects/{PICTURE}/views/imgo"),
            ..good.clone()
        },
        // another object than its itemid
        Image {
            item_id: Some("0-eus-d10-other".to_owned()),
            ..good.clone()
        },
        // not an AMS picture
        Image {
            item_type: None,
            ..good.clone()
        },
        // no size
        Image {
            size: None,
            ..good.clone()
        },
    ];
    for image in refused {
        assert_eq!(
            MessageText::new(rich(vec![Node::Image(image.clone())]), []),
            Err(ComposeError::Image),
            "{image:?}"
        );
    }
    assert!(teams_proto::sent_picture("../x", (1, 1)).is_none());
}
