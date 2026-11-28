//! Substrate's search (`docs/protocol/search.md`): what Teams' title-bar search asks as the user types and once the
//! query is entered, on [`Service::Substrate`] with the account's token for [`SUBSTRATE_AUDIENCE`].
//!
//! - **The suggestions** ([`SubstrateSearch::suggestions`]): `POST search/api/v1/suggestions?scenario=powerbar`,
//!   people, chats, teams and channels, files and messages, a few of each, as the web client asks them for every
//!   character typed. The answer is a list of groups, each of one kind.
//! - **The results** ([`SubstrateSearch::query`]): `POST searchservice/api/v2/query`, one entity request per kind
//!   asked (messages, people, files, group chats, teams and channels), as the web client asks them for its results'
//!   tabs. The answer is a list of entity sets, each of result sets of results, a result's `Source` the item found.
//!
//! Both are reads, whoever their answer lists; [`crate::HttpTransport::search`] sends them in read-only mode too.
//!
//! An answer is read leniently, one item at a time: an item missing what the app needs (a message without its chat or
//! id, a person without a name, a file without its link) is left out, never guessed. The text around a match comes
//! with the matched terms between `<c0>` and `</c0>` and a cut as `<ddd/>`; any other markup in it is dropped and its
//! entities decoded ([`highlighted`]).

use std::fmt::Write as _;
use std::ops::Range;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::endpoint::{Endpoint, Service};

/// The Entra audience of the token Substrate's search takes (the web client asks it as
/// `https://outlook.office.com/search/.default`).
pub const SUBSTRATE_AUDIENCE: &str = "https://outlook.office.com/search";

/// Substrate's host.
pub const SUBSTRATE_HOST: &str = "substrate.office.com";

/// How many of each kind the suggestions ask for (the web client's numbers).
const SUGGESTED_PEOPLE: u32 = 5;
const SUGGESTED_FILES: u32 = 3;
const SUGGESTED_CHATS: u32 = 5;
const SUGGESTED_TEAMS: u32 = 3;
const SUGGESTED_CHANNELS: u32 = 4;
const SUGGESTED_MESSAGES: u32 = 11;

/// How many of a kind the results ask for: a tab's page (the web client's), and the All tab's few of each.
pub const PAGE: u32 = 25;
pub const ALL_PAGE: u32 = 5;

/// The fields of a person the web client asks for.
const PEOPLE_FIELDS: [&str; 17] = [
    "Id",
    "MRI",
    "DisplayName",
    "EmailAddresses",
    "PeopleType",
    "PeopleSubtype",
    "Phones",
    "UserPrincipalName",
    "GivenName",
    "Surname",
    "Cid",
    "CompanyName",
    "JobTitle",
    "ImAddress",
    "ExternalDirectoryObjectId",
    "ConcatenatedId",
    "Department",
];

/// The fields of a message the web client asks for beside the default ones.
const MESSAGE_FIELDS: [&str; 8] = [
    "Extension_SkypeSpaces_ConversationPost_Extension_FromSkypeInternalId_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_FileData_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_ThreadType_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_SkypeGroupId_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_SenderTenantId_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_ParentMessageId_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_ImageSrc_String",
    "Extension_SkypeSpaces_ConversationPost_Extension_AmsReferences_StringArray",
];

/// The fields of a file the web client asks for.
const FILE_FIELDS: [&str; 14] = [
    "DefaultEncodingUrl",
    "FileName",
    "FileType",
    "HitHighlightedSummary",
    "LastModifiedTime",
    "LinkingUrl",
    "ModifiedBy",
    "Path",
    "Title",
    "SpWebUrl",
    "SiteTitle",
    "ParentLink",
    "FileExtension",
    "UniqueId",
];

/// A kind of thing the search finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Messages,
    People,
    Files,
    /// Group chats (and meetings' chats), by their names and members.
    Chats,
    /// Teams and their channels.
    Channels,
}

impl SearchKind {
    /// The results' `QueryType` dimension of a tab of this kind alone.
    fn query_type(self) -> &'static str {
        match self {
            Self::Messages => "Messages",
            Self::People => "People",
            Self::Files => "Files",
            Self::Chats => "GroupChats",
            // no capture shows this tab's own request (the app shows the All answer's channels for it instead)
            Self::Channels => "TeamsAndChannels",
        }
    }
}

/// A message found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundMessage {
    /// Its chat or channel (`19:…`).
    pub conversation: String,
    /// Its id in the chat service (its arrival time in ms, as text).
    pub id: String,
    /// The thread it replies to in a channel (its root's id); the message itself in a chat.
    pub parent: Option<String>,
    /// When it was sent, in ms since the epoch.
    pub sent_ms: i64,
    pub sender_mri: Option<String>,
    pub sender_name: Option<String>,
    /// Who else the chat holds, as the service names them (`"Ann; Bob"`).
    pub to: Option<String>,
    /// The text around the match, plain.
    pub snippet: String,
    /// Where the matched terms are in `snippet` (byte ranges).
    pub matched: Vec<Range<usize>>,
}

/// A person found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundPerson {
    /// Their MRI (`8:orgid:<guid>`) when the service gave one.
    pub mri: Option<String>,
    pub name: String,
    /// The address to show: their UPN, else their first mail address.
    pub email: Option<String>,
    /// Each address the service gave, once: the UPN and each mail address (a guest's or an alias's mail is not
    /// its UPN).
    pub addresses: Vec<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
}

/// A file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    pub name: String,
    /// Where it opens (its web link).
    pub url: String,
    /// Its type as its extension says (`pptx`).
    pub kind: Option<String>,
    /// Where it is: its site or folder, as the service names it.
    pub location: Option<String>,
    pub modified_by: Option<String>,
    pub modified_ms: Option<i64>,
    /// The text around the match, plain, and where the matched terms are in it.
    pub snippet: String,
    pub matched: Vec<Range<usize>>,
}

/// A chat found by its name or members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundChat {
    pub id: String,
    pub name: String,
}

/// A team or a channel found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundChannel {
    /// The channel's id, a team's its General channel's (`19:…@thread.tacv2`).
    pub id: String,
    pub name: String,
    /// The team's name, for a channel; `None` for a team itself.
    pub team: Option<String>,
}

/// What an answer held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub messages: Vec<FoundMessage>,
    pub people: Vec<FoundPerson>,
    pub files: Vec<FoundFile>,
    pub chats: Vec<FoundChat>,
    pub channels: Vec<FoundChannel>,
    /// Whether the service has more of the kinds asked for than it gave.
    pub more: bool,
}

/// Why an answer wasn't taken.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SearchAnswerError {
    #[error("the answer isn't JSON")]
    NotJson,
    #[error("the answer has neither groups nor entity sets")]
    Shape,
}

/// Which of Substrate's two search paths a request goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Suggestions,
    Query,
}

/// One search request (see the module docs): its path and body, built whole when it is made.
#[derive(Clone, Debug, PartialEq)]
pub struct SubstrateSearch {
    path: Path,
    body: Value,
}

impl SubstrateSearch {
    /// The suggestions for `query` being typed: a few people, files, chats, teams, channels and messages, as the
    /// web client asks them.
    #[must_use]
    pub fn suggestions(query: &str) -> Self {
        let query = query.trim();
        let plain = || json!({ "QueryString": query, "NormalizedQueryString": query });
        let entity = |kind: &str, size: u32| json!({ "Query": plain(), "EntityType": kind, "Size": size });
        let cvid = fresh_id();
        let body = json!({
            "EntityRequests": [
                {
                    "Query": { "QueryString": query, "DisplayQueryString": query, "NormalizedQueryString": query },
                    "EntityType": "People",
                    "Size": SUGGESTED_PEOPLE,
                    "Fields": PEOPLE_FIELDS,
                    "Filter": people_filter(false),
                    "Provenances": ["Mailbox", "Directory"],
                    "From": 0,
                },
                entity("File", SUGGESTED_FILES),
                entity("Chat", SUGGESTED_CHATS),
                entity("Team", SUGGESTED_TEAMS),
                {
                    "Query": plain(),
                    "EntityType": "Channel",
                    "Size": SUGGESTED_CHANNELS,
                    "Filter": { "Or": [
                        { "Term": { "ChannelType": "Standard" } },
                        { "Term": { "ChannelType": "Shared" } },
                    ] },
                },
                entity("Message", SUGGESTED_MESSAGES),
            ],
            "Scenario": {
                "Name": "powerbar",
                "Dimensions": [{ "DimensionName": "QueryType", "DimensionValue": "PeopleCentricSearch" }],
            },
            "Cvid": cvid,
            "AppName": "Microsoft Teams",
            "LogicalId": fresh_id(),
        });
        Self {
            path: Path::Suggestions,
            body,
        }
    }

    /// The results of `query` entered, for `kinds` (every kind for the All tab, one for another tab), `size` of each
    /// from `from` on.
    #[must_use]
    pub fn query(query: &str, kinds: &[SearchKind], size: u32, from: u32) -> Self {
        let query = query.trim();
        let all = kinds.len() > 1;
        let requests: Vec<Value> = kinds
            .iter()
            .map(|kind| entity_request(*kind, query, all, size, from))
            .collect();
        let query_type = match kinds {
            [one] => one.query_type(),
            _ => "All",
        };
        let body = json!({
            "EntityRequests": requests,
            "cvid": fresh_id(),
            "logicalId": fresh_id(),
            "scenario": {
                "Name": "powerbar",
                "Dimensions": [
                    { "DimensionName": "QueryType", "DimensionValue": query_type },
                    { "DimensionName": "FormFactor", "DimensionValue": "general.web.reactSearch" },
                ],
            },
        });
        Self {
            path: Path::Query,
            body,
        }
    }

    /// Its service: [`Service::Substrate`].
    #[must_use]
    pub fn service(&self) -> Service {
        Service::Substrate
    }

    /// Its path under Substrate's base, with its query.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        match self.path {
            Path::Suggestions => {
                Endpoint::new(["search", "api", "v1", "suggestions"]).with_query("scenario", "powerbar")
            }
            Path::Query => Endpoint::new(["searchservice", "api", "v2", "query"]),
        }
    }

    /// Its JSON body.
    #[must_use]
    pub fn body(&self) -> &Value {
        &self.body
    }

    /// What the answer `body` holds.
    ///
    /// # Errors
    /// [`SearchAnswerError`] when it isn't JSON, or has neither the suggestions' groups nor the results' entity sets.
    pub fn read_answer(&self, body: &[u8]) -> Result<Found, SearchAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| SearchAnswerError::NotJson)?;
        let mut found = Found::default();
        if let Some(groups) = answer.get("Groups").and_then(Value::as_array) {
            for group in groups {
                let items = group.get("Suggestions").and_then(Value::as_array);
                for item in items.into_iter().flatten() {
                    take(&mut found, item, None, group.get("Type").and_then(Value::as_str));
                }
            }
            return Ok(found);
        }
        let Some(sets) = answer.get("EntitySets").and_then(Value::as_array) else {
            return Err(SearchAnswerError::Shape);
        };
        for set in sets {
            for results in set.get("ResultSets").and_then(Value::as_array).into_iter().flatten() {
                found.more |= results.get("MoreResultsAvailable").and_then(Value::as_bool) == Some(true);
                for result in results.get("Results").and_then(Value::as_array).into_iter().flatten() {
                    let source = result.get("Source").unwrap_or(&Value::Null);
                    let summary = result.get("HitHighlightedSummary").and_then(Value::as_str);
                    take(&mut found, source, summary, result.get("Type").and_then(Value::as_str));
                }
            }
        }
        Ok(found)
    }
}

/// The people the web client looks for: persons and others, of the organization, guests and contacts, not hidden
/// (the results' filter also takes rooms and implicit contacts).
fn people_filter(results: bool) -> Value {
    let mut subtypes = vec!["OrganizationUser", "MTOUser", "PersonalContact", "Guest"];
    if results {
        subtypes.extend(["Room", "ImplicitContact"]);
    }
    let subtypes: Vec<Value> = subtypes
        .into_iter()
        .map(|subtype| json!({ "Term": { "PeopleSubtype": subtype } }))
        .collect();
    json!({ "And": [
        { "Or": [{ "Term": { "PeopleType": "Person" } }, { "Term": { "PeopleType": "Other" } }] },
        { "Or": subtypes },
        { "Or": [{ "Term": { "Flags": "NonHidden" } }] },
    ] })
}

/// The results' entity request of `kind` for `query`, `size` of them from `from` on; `all` when it is one of the All
/// tab's.
fn entity_request(kind: SearchKind, query: &str, all: bool, size: u32, from: u32) -> Value {
    let asked = json!({ "queryString": query, "displayQueryString": query });
    match kind {
        SearchKind::Messages => {
            // the All tab's message request leaves out messages deleted on the client, as the web client's does; a tab
            // of messages alone is asked with the bare query, as captured
            let asked = if all {
                json!({
                    "queryString": format!("NOT (isClientSoftDeleted:TRUE) AND {query}"),
                    "displayQueryString": query,
                })
            } else {
                asked
            };
            json!({
                "entityType": "Message",
                "contentSources": ["Teams"],
                "fields": MESSAGE_FIELDS,
                "propertySet": "Optimized",
                "query": asked,
                "from": from,
                "size": size,
                "topResultsCount": 5,
            })
        }
        SearchKind::People => json!({
            "entityType": "People",
            "contentSources": ["Exchange"],
            "Filter": people_filter(true),
            "query": asked,
            "from": from,
            "size": size,
        }),
        SearchKind::Files => json!({
            "entityType": "File",
            "contentSources": ["OneDriveBusiness", "SharePoint", "Exchange"],
            "Fields": FILE_FIELDS,
            "EnableQueryUnderstanding": false,
            "EnableSpeller": false,
            "HitHighlight": { "HitHighlightedProperties": ["HitHighlightedSummary"], "SummaryLength": 200 },
            "IdFormat": "EwsId",
            "ParserType": "None",
            "propertySet": "Optimized",
            "query": asked,
            "from": from,
            "size": size,
        }),
        SearchKind::Chats => json!({
            "entityType": "Chat",
            "contentSources": ["Teams"],
            "propertySet": "Optimized",
            "fields": [],
            "query": asked,
            "from": from,
            "size": size,
        }),
        SearchKind::Channels => json!({
            "entityType": "TeamsChannel",
            "contentSources": ["Teams"],
            "HitHighlight": { "HitHighlightedProperties": ["HitHighlightedSummary"], "SummaryLength": 200 },
            "fields": [],
            "query": asked,
            "from": from,
            "size": size,
        }),
    }
}

/// A fresh lowercase GUID: a search's correlation ids (`cvid`, `logicalId`) and a presence subscription's, which name
/// nothing of the account.
pub(crate) fn fresh_id() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // a correlation id only: a fixed one asks the same
        return "00000000-0000-4000-8000-000000000000".to_owned();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Takes one item of an answer: `item` the suggestion or the result's source, `summary` the result's text around the
/// match, `kind` the group's or the result's type.
fn take(found: &mut Found, item: &Value, summary: Option<&str>, kind: Option<&str>) {
    let entity = item.get("EntityType").and_then(Value::as_str).unwrap_or_default();
    let kind = kind.unwrap_or(entity);
    match kind {
        "Message" | "ChatMessage" => found.messages.extend(message(item, summary)),
        "People" | "Person" | "PeopleSuggestion" | "PersonSuggestion" => found.people.extend(person(item)),
        "File" | "FileSuggestion" | "Document" => found.files.extend(file(item, summary)),
        "Chat" | "ChatSuggestion" | "GroupChat" => found.chats.extend(chat(item)),
        "Team" | "TeamSuggestion" => found.channels.extend(team(item)),
        "Channel" | "ChannelSuggestion" | "TeamsChannel" => found.channels.extend(channel(item)),
        _ => {}
    }
}

fn text<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn owned(item: &Value, key: &str) -> Option<String> {
    text(item, key).map(str::to_owned)
}

/// The first of `keys` `item` has as text.
fn first(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| owned(item, key))
}

/// An RFC 3339 time as ms since the epoch.
fn time_ms(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A name and address as an answer gives one: `{"EmailAddress": {"Name", "Address"}}`.
fn mailbox(item: &Value, key: &str) -> (Option<String>, Option<String>) {
    let address = item.get(key).and_then(|value| value.get("EmailAddress"));
    match address {
        Some(address) => (owned(address, "Name"), owned(address, "Address")),
        None => (None, None),
    }
}

fn message(source: &Value, summary: Option<&str>) -> Option<FoundMessage> {
    let conversation = owned(source, "ClientThreadId")?;
    let id = owned(source, "InternetMessageId").filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()))?;
    let extensions = source.get("Extensions").unwrap_or(&Value::Null);
    let sent_ms = text(source, "DateTimeSent")
        .or_else(|| text(source, "DateTimeReceived"))
        .and_then(time_ms)
        .or_else(|| id.parse().ok())?;
    let (sender_name, _) = mailbox(source, "Sender");
    let sender_name = sender_name.or_else(|| mailbox(source, "From").0);
    let raw = summary
        .filter(|summary| !summary.trim().is_empty())
        .or_else(|| text(source, "Preview"))
        .unwrap_or_default();
    let (snippet, matched) = highlighted(raw);
    Some(FoundMessage {
        conversation,
        parent: owned(extensions, "SkypeSpaces_ConversationPost_Extension_ParentMessageId"),
        id,
        sent_ms,
        sender_mri: owned(extensions, "SkypeSpaces_ConversationPost_Extension_FromSkypeInternalId"),
        sender_name,
        to: owned(source, "DisplayTo"),
        snippet,
        matched,
    })
}

fn person(item: &Value) -> Option<FoundPerson> {
    let name = first(item, &["DisplayName", "Name", "Text"])?;
    let mut addresses: Vec<String> = Vec::new();
    let listed = item
        .get("EmailAddresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let single = ["UserPrincipalName", "EmailAddress", "PrimarySmtpAddress"]
        .into_iter()
        .filter_map(|key| text(item, key));
    for address in single
        .chain(listed)
        .map(str::trim)
        .filter(|address| !address.is_empty())
    {
        if !addresses.iter().any(|known| known.eq_ignore_ascii_case(address)) {
            addresses.push(address.to_owned());
        }
    }
    let email = addresses.first().cloned();
    let mri = first(item, &["MRI", "Mri"])
        .filter(|mri| mri.starts_with("8:"))
        .or_else(|| {
            first(item, &["ExternalDirectoryObjectId"])
                .filter(|oid| oid.len() == 36)
                .map(|oid| format!("8:orgid:{}", oid.to_lowercase()))
        });
    Some(FoundPerson {
        mri,
        name,
        email,
        addresses,
        job_title: owned(item, "JobTitle"),
        department: owned(item, "Department"),
    })
}

fn file(source: &Value, summary: Option<&str>) -> Option<FoundFile> {
    let name = first(source, &["FileName", "Title", "Name", "Text"])?;
    let url = first(source, &["LinkingUrl", "Path", "DefaultEncodingUrl", "WebUrl", "Url"])
        .filter(|url| url.starts_with("https://"))?;
    let kind = first(source, &["FileType", "FileExtension"]).map(|kind| kind.to_lowercase());
    let location = first(source, &["SiteTitle", "SiteName"])
        .map(|site| site.rsplit('/').next().unwrap_or(&site).to_owned())
        .filter(|site| !site.is_empty());
    let (snippet, matched) = highlighted(summary.unwrap_or_default());
    Some(FoundFile {
        name,
        url,
        kind,
        location,
        modified_by: owned(source, "ModifiedBy"),
        modified_ms: text(source, "LastModifiedTime").and_then(time_ms),
        snippet,
        matched,
    })
}

/// A chat: its thread, named by its topic when it has one, else by its members (the results' `Members`, the
/// suggestions' matching and other members).
fn chat(item: &Value) -> Option<FoundChat> {
    let id = first(item, &["ThreadId", "ClientThreadId"]).filter(|id| id.starts_with("19:"))?;
    let members: Vec<String> = match item.get("Members").and_then(Value::as_array) {
        Some(names) => names
            .iter()
            .filter_map(|name| name.as_str().map(str::trim).filter(|name| !name.is_empty()))
            .map(str::to_owned)
            .collect(),
        None => ["MatchingMembers", "ChatMembers"]
            .iter()
            .filter_map(|key| item.get(*key).and_then(Value::as_array))
            .flatten()
            .filter_map(|member| owned(member, "DisplayName"))
            .collect(),
    };
    let name = first(item, &["Topic", "Name", "DisplayName"]).or_else(|| {
        let mut members = members;
        members.dedup();
        (!members.is_empty()).then(|| members.join(", "))
    })?;
    Some(FoundChat { id, name })
}

fn team(item: &Value) -> Option<FoundChannel> {
    let id = owned(item, "ThreadId").filter(|id| id.starts_with("19:"))?;
    Some(FoundChannel {
        id,
        name: first(item, &["Name", "DisplayName"])?,
        team: None,
    })
}

fn channel(item: &Value) -> Option<FoundChannel> {
    let id = first(item, &["ThreadId", "ChannelThreadId", "Id"]).filter(|id| id.starts_with("19:"))?;
    let name = first(item, &["Name", "DisplayName", "ChannelName"])?;
    Some(FoundChannel {
        id,
        name,
        team: first(item, &["TeamName", "TeamDisplayName"]),
    })
}

/// Text the service marked up: the matched terms between `<c0>` and `</c0>`, a cut as `<ddd/>`, any other tag dropped
/// and the entities decoded; spaces folded. The plain text, and where the matched terms are in it (byte ranges).
#[must_use]
pub fn highlighted(raw: &str) -> (String, Vec<Range<usize>>) {
    let mut out = String::with_capacity(raw.len());
    let mut matched = Vec::new();
    let mut open: Option<usize> = None;
    let mut rest = raw;
    while let Some(at) = rest.find(['<', '&']) {
        push_folded(&mut out, &rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else {
                push_folded(&mut out, rest);
                rest = "";
                break;
            };
            let tag = rest[1..end].trim().to_ascii_lowercase();
            match tag.as_str() {
                "c0" => open = Some(out.len()),
                "/c0" => {
                    if let Some(from) = open.take()
                        && from < out.len()
                    {
                        matched.push(from..out.len());
                    }
                }
                "ddd/" | "ddd /" | "ddd" => push_folded(&mut out, "…"),
                tag if tag.starts_with("br") || tag.starts_with("/p") || tag.starts_with("/div") => {
                    push_folded(&mut out, " ");
                }
                _ => {}
            }
            rest = &rest[end + 1..];
        } else {
            let end = rest.find(';').filter(|end| *end <= 10);
            let decoded = end.and_then(|end| entity(&rest[1..end]));
            if let (Some(end), Some(ch)) = (end, decoded) {
                let mut buf = [0_u8; 4];
                push_folded(&mut out, ch.encode_utf8(&mut buf));
                rest = &rest[end + 1..];
            } else {
                push_folded(&mut out, "&");
                rest = &rest[1..];
            }
        }
    }
    push_folded(&mut out, rest);
    let trimmed_end = out.trim_end().len();
    out.truncate(trimmed_end);
    matched.retain(|range| range.end <= out.len());
    (out, matched)
}

/// Appends `text` with runs of white space folded into one space (none at the start).
fn push_folded(out: &mut String, text: &str) {
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
}

/// An HTML entity's character (`amp`, `#39`, `#x27`).
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}
