//! Reads: requests that change nothing anyone could notice, always sent as `GET`.
//!
//! Besides the chat service's reads (in [`crate::chat`]), the reads of Microsoft Graph a person's name and picture come
//! from (M2 Task 12): [`GraphUser`] and [`GraphPicture`], in the shapes `proto-lab probe profile` read live
//! (`docs/protocol/graph.md`). Graph is our own reach, not the web client's (it names people through the middle
//! tier's profile lookups, `crate::ShortProfileLookup`), so the probe's answers are the reviewed shape.
//!
//! And CSA's read of the account's teams and their channels (M2 Task 16): [`CsaTeams`], in the shape the web client
//! sent it and `proto-lab probe teams` read live with our own `chatsvcagg` token (`docs/protocol/channels.md`). A
//! channel's posts are read from the chat service like a chat's messages.
//!
//! Any other read on Graph ([`Service::Graph`]) or CSA ([`Service::Csa`]) goes to that service's base too; a mutating
//! request to either is a plain [`Request`](crate::Request).

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};

/// A read. It names only its service and its endpoint under that service's base; the transport always sends it as a
/// `GET`, with no body. So any type may implement it: a read can't become a request of another method, can't carry a
/// body and can't name a host.
pub trait Read {
    fn service(&self) -> Service;
    fn endpoint(&self) -> Endpoint;
}

/// The `$select` of a Graph user read: the record's id and the name it shows, nothing else of the person's record.
pub const GRAPH_USER_SELECT: &str = "id,displayName";

/// The picture size read: 96×96, what an avatar needs (the probe's 96×96 pictures were about 7 KB, the largest size
/// about 230 KB).
pub const GRAPH_PICTURE_SIZE: &str = "96x96";

/// The largest picture kept, in bytes; a longer answer is [`GraphAnswerError::TooLarge`].
pub const MAX_PICTURE: usize = 256 * 1024;

/// The object id an MRI `8:orgid:<object id>` names, when it is a GUID in its hyphenated form; `None` for any other
/// MRI (a bot's, a phone number's, an unknown form).
#[must_use]
pub fn object_id(mri: &str) -> Option<&str> {
    mri.strip_prefix("8:orgid:").filter(|id| is_guid(id))
}

/// A GUID in its hyphenated form, any case.
pub(crate) fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// `GET https://graph.microsoft.com/v1.0/users/<object id>?$select=id,displayName`: the name Graph shows for one
/// account, by its object id. Its answer is read with [`GraphUser::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct GraphUser {
    id: String,
}

impl GraphUser {
    /// The read of the account `mri` names; `None` unless it is `8:orgid:<object id>` ([`object_id`]).
    #[must_use]
    pub fn of(mri: &str) -> Option<Self> {
        object_id(mri).map(|id| Self { id: id.to_owned() })
    }

    /// The name the answer `body` gives (trimmed; `None` when it gives none or an empty one).
    ///
    /// # Errors
    /// [`GraphAnswerError::Malformed`] unless the body is a JSON object with a string `id` (and a `displayName` that
    /// is a string or `null`, when there is one); [`GraphAnswerError::OtherUser`] when its `id` isn't the one asked
    /// for (any case).
    pub fn read_answer(&self, body: &[u8]) -> Result<Option<String>, GraphAnswerError> {
        let answer: UserAnswer = serde_json::from_slice(body).map_err(|_| GraphAnswerError::Malformed)?;
        if !answer.id.eq_ignore_ascii_case(&self.id) {
            return Err(GraphAnswerError::OtherUser);
        }
        Ok(answer
            .display_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()))
    }
}

/// `Debug` leaves the object id out.
impl fmt::Debug for GraphUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphUser").finish_non_exhaustive()
    }
}

impl Read for GraphUser {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", self.id.as_str()]).with_query("$select", GRAPH_USER_SELECT)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserAnswer {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// `GET https://graph.microsoft.com/v1.0/users/<object id>/photos/96x96/$value`: one account's picture at 96×96, as
/// image bytes. An account without a picture answers 404 (`ImageNotFound`). Its answer is read with
/// [`GraphPicture::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct GraphPicture {
    id: String,
}

impl GraphPicture {
    /// The read of the account `mri` names; `None` unless it is `8:orgid:<object id>` ([`object_id`]).
    #[must_use]
    pub fn of(mri: &str) -> Option<Self> {
        object_id(mri).map(|id| Self { id: id.to_owned() })
    }

    /// The picture the answer gives.
    ///
    /// # Errors
    /// [`GraphAnswerError::UnknownType`] unless its `Content-Type` is `image/jpeg` (what the probe saw) or
    /// `image/png`; [`GraphAnswerError::TooLarge`] over [`MAX_PICTURE`] bytes; [`GraphAnswerError::Malformed`] for an
    /// empty body.
    pub fn read_answer(&self, answer: &Response) -> Result<Picture, GraphAnswerError> {
        let format = answer
            .content_type()
            .and_then(PictureFormat::of)
            .ok_or(GraphAnswerError::UnknownType)?;
        let bytes = answer.body();
        if bytes.len() > MAX_PICTURE {
            return Err(GraphAnswerError::TooLarge);
        }
        if bytes.is_empty() {
            return Err(GraphAnswerError::Malformed);
        }
        Ok(Picture {
            format,
            bytes: bytes.to_vec(),
        })
    }
}

/// `Debug` leaves the object id out.
impl fmt::Debug for GraphPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphPicture").finish_non_exhaustive()
    }
}

impl Read for GraphPicture {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", self.id.as_str(), "photos", GRAPH_PICTURE_SIZE, "$value"])
    }
}

/// A picture's image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureFormat {
    Jpeg,
    Png,
}

impl PictureFormat {
    /// The format a `Content-Type` names (its parameters and case ignored), if it is one kept.
    #[must_use]
    pub fn of(content_type: &str) -> Option<Self> {
        let media = content_type.split(';').next().unwrap_or_default().trim();
        if media.eq_ignore_ascii_case("image/jpeg") {
            Some(Self::Jpeg)
        } else if media.eq_ignore_ascii_case("image/png") {
            Some(Self::Png)
        } else {
            None
        }
    }

    /// Its media type.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

/// A person's picture as Graph gave it: its format and its bytes, undecoded. `Debug` shows the format and the size.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub format: PictureFormat,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Why a Graph answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GraphAnswerError {
    /// Not the probed answer's shape.
    #[error("not a Graph answer of the probed shape")]
    Malformed,
    /// A user record of another account than the one asked for.
    #[error("the answer is of another account than the one asked for")]
    OtherUser,
    /// A picture whose `Content-Type` isn't an image format kept.
    #[error("a picture of a type that isn't kept")]
    UnknownType,
    /// A picture over [`MAX_PICTURE`] bytes.
    #[error("a picture over {MAX_PICTURE} bytes")]
    TooLarge,
}

/// The query of CSA's teams read, pair by pair in order: the web client's in every capture of the full read
/// (`ab-chat`, `abc-chat`, `load`), the one `proto-lab probe teams` read live (M2 Task 16).
pub const CSA_TEAMS_QUERY: [(&str, &str); 5] = [
    ("isPrefetch", "false"),
    ("enableMembershipSummary", "true"),
    ("supportsAdditionalSystemGeneratedFolders", "true"),
    ("supportsSliceItems", "true"),
    ("enableEngageCommunities", "false"),
];

/// The path of CSA's teams read under the CSA base.
pub const CSA_TEAMS_PATH: [&str; 5] = ["api", "v3", "teams", "users", "me"];

/// `GET <CSA base>/api/v3/teams/users/me?`[`CSA_TEAMS_QUERY`]: the account's teams with their channels (and its chats,
/// which the native client reads from the chat service instead). Its answer is read with [`CsaTeams::read_answer`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CsaTeams;

impl CsaTeams {
    /// The teams the answer `body` gives, each with its channels: a team or a channel marked deleted is left out, and
    /// so is one without a string `id` and `displayName` (counted in [`TeamsAnswer::skipped`]). A team's general
    /// channel (`isGeneral` `true`, or the channel whose id is the team's) comes first, the others in the answer's
    /// order. The account's favourites (chats and channels) come with them ([`TeamsAnswer::favorites`]).
    ///
    /// The answer must say its teams are whole, since the store replaces its list with what a read gives:
    /// `metadata.isPartialData` exactly `false`, or exactly `true` with `metadata.hasMoreChats` exactly `true`. An
    /// account with many chats is answered the second way (a personal build's log): the answer holds only its newest
    /// chats, and the web client reads the rest from the chat service when it is told the answer is partial (the
    /// bundle's `shouldSkipGroupChatSyncBasedOnIsPartialData`, `partialDataStartupChatsLimit`), its teams and folders
    /// taken as they come. An answer that says it is partial without more chats, or doesn't say (no `metadata`, no
    /// flag, a flag that isn't a boolean), is refused rather than taken for complete.
    ///
    /// # Errors
    /// [`CsaAnswerError::Malformed`] unless the body is a JSON object with a `teams` array and a `metadata` that says
    /// its teams are whole (above).
    pub fn read_answer(body: &[u8]) -> Result<TeamsAnswer, CsaAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| CsaAnswerError::Malformed)?;
        if !teams_whole(answer.get("metadata")) {
            return Err(CsaAnswerError::Malformed);
        }
        let entries = answer
            .get("teams")
            .and_then(Value::as_array)
            .ok_or(CsaAnswerError::Malformed)?;
        let mut read = TeamsAnswer {
            favorites: favorites(&answer),
            ..TeamsAnswer::default()
        };
        for entry in entries {
            match named(entry) {
                Named::Deleted => {}
                Named::Unreadable => read.skipped += 1,
                Named::Kept(id, name) => {
                    let team = team(entry, id, name, &mut read.skipped);
                    read.teams.push(team);
                }
            }
        }
        Ok(read)
    }
}

impl CsaTeams {
    /// The meeting that runs in the channel `channel` now, as CSA's answer `body` names it under the channel's
    /// `activeMeetups` (seen 2026-11-03, `meeting-steps`: a channel's Meet now, the web client reading it to show the
    /// channel's Join): `{"messageId", "conversationUrl", "organizerId", "tenantId", "status": "Active", …}`. `None` when
    /// the answer doesn't read, the channel isn't in it, or no meeting in it is active with a conversation link on the
    /// calling service, its meeting's post and its organizer and tenant named.
    #[must_use]
    pub fn meetup_in(body: &[u8], channel: &str) -> Option<ChannelMeetup> {
        Self::meetup_for_cloud(&crate::Cloud::Microsoft, body, channel)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn meetup_for_cloud(cloud: &crate::Cloud, body: &[u8], channel: &str) -> Option<ChannelMeetup> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let channels = answer
            .get("teams")?
            .as_array()?
            .iter()
            .filter_map(|team| team.get("channels").and_then(Value::as_array))
            .flatten();
        let entry = channels
            .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(channel))
            .find(|entry| !flag(entry, "isDeleted"))?;
        entry
            .get("activeMeetups")?
            .as_array()?
            .iter()
            .filter(|meetup| meetup.get("status").and_then(Value::as_str) == Some("Active"))
            .find_map(|meetup| ChannelMeetup::of(cloud, meetup))
    }
}

/// A meeting running in a channel ([`CsaTeams::meetup_in`]): its post, the conversation it is joined at, and its
/// organizer and tenant, as a join names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelMeetup {
    /// The meeting's post in the channel (`messageId`): the join names it beside the channel.
    pub message_id: String,
    /// The meeting's conversation (`conversationUrl`), where the join goes.
    pub conversation: crate::CallLink,
    pub meeting: crate::MeetingInfo,
}

impl ChannelMeetup {
    fn of(cloud: &crate::Cloud, meetup: &Value) -> Option<Self> {
        let text = |key: &str| {
            meetup
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        Some(Self {
            message_id: text("messageId")?.to_owned(),
            conversation: crate::CallLink::parse_in(cloud, text("conversationUrl")?)?,
            meeting: crate::MeetingInfo {
                organizer_id: text("organizerId")?.to_owned(),
                tenant_id: text("tenantId")?.to_owned(),
            },
        })
    }

    /// The meeting that runs in a channel as its posts tell it, newest first (`messages`, a page of the channel's own
    /// messages): its newest call start (`Event/Call`, `<callEventType>callStarted`) whose call (`<callId>`) no newer
    /// post ends, the post's id its message, its `<conversationUrl>` where the join goes, and its organizer and tenant
    /// from the post's `properties.meeting` (seen 2026-11-03: a Meet now in a channel other than General, whose
    /// `activeMeetups` CSA gave the meeting's organizer but not b, who had never opened the channel; b's web client
    /// joins by the post's card). `None` when no such post reads.
    #[must_use]
    pub fn of_posts(messages: &[crate::chat::Message]) -> Option<Self> {
        Self::of_posts_in(&crate::Cloud::Microsoft, messages)
    }

    /// Reads with the account's service origin.
    #[must_use]
    pub fn of_posts_in(cloud: &crate::Cloud, messages: &[crate::chat::Message]) -> Option<Self> {
        let mut ended: Vec<&str> = Vec::new();
        for message in messages {
            if message.message_type.as_str() != "Event/Call" {
                continue;
            }
            let content = message.content.as_str();
            let call = xml_text(content, "callId");
            match xml_text(content, "callEventType") {
                Some("callEnded") => ended.extend(call),
                Some("callStarted") if call.is_none_or(|call| !ended.contains(&call)) => {
                    return Self::of_post(cloud, message);
                }
                _ => {}
            }
        }
        None
    }

    fn of_post(cloud: &crate::Cloud, message: &crate::chat::Message) -> Option<Self> {
        let link = xml_text(&message.content, "conversationUrl")?.replace("&amp;", "&");
        let meeting = match message.properties.get("meeting")? {
            Value::String(text) => serde_json::from_str::<Value>(text).ok()?,
            object @ Value::Object(_) => object.clone(),
            _ => return None,
        };
        let text = |key: &str| {
            meeting
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        (!message.id.is_empty()).then_some(())?;
        Some(Self {
            message_id: message.id.clone(),
            conversation: crate::CallLink::parse_in(cloud, &link)?,
            meeting: crate::MeetingInfo {
                organizer_id: text("organizerId")?,
                tenant_id: text("tenantId")?,
            },
        })
    }
}

/// The conversation a group chat's call runs at, as the chat's own call events tell it, newest first (`events`: each
/// message's `messagetype` and content): the newest start (`Event/Call`, `<callEventType>callStarted`) whose call
/// (`<callId>`) no newer event ends, at its `<conversationUrl>`. A group chat's meeting (Meet now) rings nobody, so its
/// members' join goes where its start says it runs, as a channel's meeting's does ([`ChannelMeetup::of_posts`]).
/// `None` when no start reads so, or an end naming no call comes before any unended start.
#[must_use]
pub fn running_call<'m>(events: impl IntoIterator<Item = (&'m str, &'m str)>) -> Option<crate::CallLink> {
    running_call_in(&crate::Cloud::Microsoft, events)
}

/// The newest running call whose conversation is on this account's calling service.
#[must_use]
pub fn running_call_in<'m>(
    cloud: &crate::Cloud,
    events: impl IntoIterator<Item = (&'m str, &'m str)>,
) -> Option<crate::CallLink> {
    let mut ended: Vec<&str> = Vec::new();
    for (message_type, content) in events {
        if message_type != "Event/Call" {
            continue;
        }
        let call = xml_text(content, "callId");
        match xml_text(content, "callEventType") {
            Some("callEnded") => ended.push(call?),
            Some("callStarted") if call.is_none_or(|call| !ended.contains(&call)) => {
                let link = xml_text(content, "conversationUrl")?.replace("&amp;", "&");
                return crate::CallLink::parse_in(cloud, &link);
            }
            _ => {}
        }
    }
    None
}

/// Whether the call that a chat's call event `start` (its message id) started has ended, as the chat's own call events
/// tell it, newest first (`events`: each message's id, `messagetype` and content): an end (`callEnded`) newer than the
/// start names its call (`<callId>`) or names none; for a start naming no call, any newer end does. `false` when
/// `start` isn't among them.
#[must_use]
pub fn start_ended<'m>(events: impl IntoIterator<Item = (&'m str, &'m str, &'m str)>, start: &str) -> bool {
    let mut ends: Vec<Option<&str>> = Vec::new();
    for (id, message_type, content) in events {
        if message_type != "Event/Call" {
            continue;
        }
        if id == start {
            let call = xml_text(content, "callId");
            return ends.iter().any(|end| end.is_none() || call.is_none() || *end == call);
        }
        if xml_text(content, "callEventType") == Some("callEnded") {
            ends.push(xml_text(content, "callId"));
        }
    }
    false
}

/// The text of the first `<tag>…</tag>` in `content`, trimmed; `None` when there is none or it is empty.
fn xml_text<'c>(content: &'c str, tag: &str) -> Option<&'c str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = start + content[start..].find(&close)?;
    Some(content[start..end].trim()).filter(|text| !text.is_empty())
}

impl Read for CsaTeams {
    fn service(&self) -> Service {
        Service::Csa
    }

    fn endpoint(&self) -> Endpoint {
        CSA_TEAMS_QUERY
            .iter()
            .fold(Endpoint::new(CSA_TEAMS_PATH), |endpoint, (name, value)| {
                endpoint.with_query(*name, *value)
            })
    }
}

/// Whether a CSA answer's `metadata` says its teams are whole (see [`CsaTeams::read_answer`]): `isPartialData`
/// `false`, or `true` only for the chats it leaves out (`hasMoreChats` `true`).
fn teams_whole(metadata: Option<&Value>) -> bool {
    let flag = |key| metadata.and_then(|metadata| metadata.get(key)).and_then(Value::as_bool);
    match flag("isPartialData") {
        Some(false) => true,
        Some(true) => flag("hasMoreChats") == Some(true),
        None => false,
    }
}

/// What CSA's teams read gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamsAnswer {
    pub teams: Vec<Team>,
    /// Teams and channels left out because they had no string `id` or `displayName`.
    pub skipped: usize,
    /// The account's favourites (chats and channels), the ones Teams lists under "Favorites" above its chats, in the
    /// folder's order: the conversation ids of the `Favorites` folder of `conversationFolders` (see [`favorites`]).
    /// `None` when the answer's folders aren't in the captured shape: not known, never read as none.
    pub favorites: Option<Vec<String>>,
}

/// The `Favorites` folder's conversations of a CSA answer, in the folder's order (its `sortType` was
/// `UserDefinedCustomOrder` in every capture: the user's own order, as listed).
///
/// Read only in the captured shape: `conversationFolders` an object that says it is whole (`isPartialResponse`
/// exactly `false`) with a `conversationFolders` array. Without a folder whose `folderType` is `Favorites` (or with
/// it marked deleted) there are none. An item is a conversation id (`conversationId`, a string) unless it is another
/// kind of item (`itemType` present: the quick views' slices); a conversation listed twice keeps its first place.
/// Anything else (no folders, a partial or unsaid answer, a folder or an item of another shape) is `None`.
fn favorites(answer: &Value) -> Option<Vec<String>> {
    let folders = answer.get("conversationFolders")?;
    if folders.get("isPartialResponse").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let listed = folders.get("conversationFolders")?.as_array()?;
    let mut found = None;
    for folder in listed {
        let folder = folder.as_object()?;
        if folder.get("folderType").and_then(Value::as_str) == Some("Favorites") && !flag_of(folder, "isDeleted") {
            if found.is_some() {
                // two favourites folders: no capture showed which one Teams shows
                return None;
            }
            found = Some(folder);
        }
    }
    let Some(folder) = found else {
        return Some(Vec::new());
    };
    let mut ids: Vec<String> = Vec::new();
    for item in folder.get("conversationFolderItems")?.as_array()? {
        let item = item.as_object()?;
        if item.contains_key("itemType") {
            continue;
        }
        let id = item.get("conversationId")?.as_str()?;
        if id.is_empty() {
            return None;
        }
        if !ids.iter().any(|kept| kept == id) {
            ids.push(id.to_owned());
        }
    }
    Some(ids)
}

/// Whether an object's boolean `key` is `true`.
fn flag_of(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool) == Some(true)
}

/// A team the account is a member of. `Debug` leaves the names out.
#[derive(Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the service says of a team and of the account in it: each stands alone, no state machine"
)]
pub struct Team {
    /// Its id: its general channel's (`19:…@thread.tacv2`).
    pub id: String,
    pub name: String,
    /// Archived: read-only for everyone.
    pub archived: bool,
    /// Its Microsoft 365 group (`teamSiteInformation.groupId`), when the answer names one.
    pub group_id: Option<String>,
    /// Its mail address (`smtpAddress`), when the answer names one.
    pub mailbox: Option<String>,
    /// Whether the account owns it (`memberRoleString` `admin`).
    pub owner: bool,
    /// Whether the account may add a channel ([`can_add_channel`]).
    pub can_add_channel: bool,
    /// Whether the account may add a private channel ([`can_add_private_channel`]).
    pub can_add_private_channel: bool,
    /// What the team pages show of it.
    pub about: TeamAbout,
    /// Its channels, the general one first.
    pub channels: Vec<Channel>,
}

/// What "Your teams and channels" and Manage team show of a team, as CSA's answer has it. `Debug` leaves it all out with
/// the rest of the team's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the service says of a team and of the account in it: each stands alone, no state machine"
)]
pub struct TeamAbout {
    /// The tenant it belongs to: a link to it names it.
    pub tenant: String,
    pub description: String,
    /// Anyone in the organization can join it (`accessType` other than 1).
    pub public: bool,
    /// Its people, bots left out (`membershipSummary`).
    pub people: u32,
    /// The account is a guest on it (`memberRoleString` `guest`).
    pub guest: bool,
    /// Its people come from a rule, not from hands (`dynamicMembership`).
    pub dynamic_membership: bool,
    /// It holds everyone in the organization (`isTenantWide`).
    pub tenant_wide: bool,
    /// The account is only on some of its channels (`channelOnlyMember`).
    pub channel_only: bool,
    /// Its channels are hidden for the account (`isFavorite` `false`).
    pub hidden: bool,
    /// Its picture's version (`pictureETag`).
    pub picture_etag: Option<String>,
    /// Its SharePoint site (`teamSiteInformation.sharepointSiteUrl`), where its files and lists live.
    pub site: Option<String>,
}

impl fmt::Debug for Team {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Team")
            .field("id", &self.id)
            .field("archived", &self.archived)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// A team's channel: a conversation of the chat service, read like a chat. `Debug` leaves the name out.
#[derive(Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent channel metadata and permissions supplied by CSA"
)]
pub struct Channel {
    pub id: String,
    pub name: String,
    /// The team's general channel.
    pub general: bool,
    /// Who it is for (`channelType`: 0 standard, 1 private, 2 shared); `Unknown` for any other value.
    pub kind: ChannelType,
    /// Whether it is in the account's channel list: a general channel by the team's `isGeneralChannelFavorite`, another
    /// by its `isFavorite`; an answer that doesn't say shows it.
    pub shown: bool,
    pub description: String,
    pub archived: bool,
    pub owner: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub group: Option<String>,
}

/// A channel's `channelType` in CSA's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelType {
    Standard,
    Private,
    Shared,
    /// A value no capture showed (or none): kept unknown, not guessed.
    Unknown,
}

impl ChannelType {
    /// What an entry's `channelType` says.
    fn of(entry: &Value) -> Self {
        match entry.get("channelType").and_then(Value::as_u64) {
            Some(0) => Self::Standard,
            Some(1) => Self::Private,
            Some(2) => Self::Shared,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("id", &self.id)
            .field("general", &self.general)
            .finish_non_exhaustive()
    }
}

/// Why a CSA answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CsaAnswerError {
    /// Not the probed answer's shape, or an answer that doesn't say its teams are whole ([`CsaTeams::read_answer`]).
    #[error("not a whole CSA teams answer of the probed shape")]
    Malformed,
}

/// How a team or a channel entry reads.
enum Named<'a> {
    Kept(&'a str, &'a str),
    Deleted,
    Unreadable,
}

/// An entry's id and name, unless it is marked deleted (`isDeleted` `true`) or has no string `id` and `displayName`.
fn named(entry: &Value) -> Named<'_> {
    if flag(entry, "isDeleted") {
        return Named::Deleted;
    }
    let text = |key| entry.get(key).and_then(Value::as_str);
    match (text("id"), text("displayName")) {
        (Some(id), Some(name)) if !id.is_empty() => Named::Kept(id, name),
        _ => Named::Unreadable,
    }
}

/// Whether an entry's boolean `key` is `true`; anything else (absent, another type) is `false`.
fn flag(entry: &Value, key: &str) -> bool {
    entry.get(key).and_then(Value::as_bool) == Some(true)
}

fn channel_permission(entry: &Value, key: &str) -> bool {
    let settings = match entry.get("memberRoleString").and_then(Value::as_str) {
        Some("admin" | "user" | "member") => "teamSettings",
        Some("guest") => "teamGuestSettings",
        _ => return false,
    };
    entry
        .get(settings)
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        == Some(true)
}

/// A team entry read, its unreadable channels counted in `skipped`.
fn team(entry: &Value, id: &str, name: &str, skipped: &mut usize) -> Team {
    let mut channels = Vec::new();
    let listed = entry.get("channels").and_then(Value::as_array);
    for channel in listed.into_iter().flatten() {
        match named(channel) {
            Named::Deleted => {}
            Named::Unreadable => *skipped += 1,
            Named::Kept(channel_id, channel_name) => channels.push(Channel {
                id: channel_id.to_owned(),
                name: channel_name.to_owned(),
                general: flag(channel, "isGeneral") || channel_id == id,
                kind: ChannelType::of(channel),
                shown: shown(entry, channel, flag(channel, "isGeneral") || channel_id == id),
                description: channel
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                archived: flag(channel, "isArchived"),
                owner: channel.get("memberRole").and_then(Value::as_u64) == Some(2)
                    || channel
                        .get("memberRole")
                        .and_then(Value::as_str)
                        .is_some_and(|role| matches!(role, "Admin" | "admin")),
                can_edit: ChannelType::of(channel) == ChannelType::Standard
                    && channel_id != id
                    && !flag(channel, "isGeneral")
                    && channel_permission(entry, "updateTopic"),
                can_delete: ChannelType::of(channel) == ChannelType::Standard
                    && channel_permission(entry, "deleteTopic"),
                group: channel
                    .get("substrateGroupId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
            }),
        }
    }
    // the general channel first, the others in the answer's order
    channels.sort_by_key(|channel| !channel.general);
    Team {
        id: id.to_owned(),
        name: name.to_owned(),
        archived: flag(entry, "isArchived"),
        group_id: entry
            .pointer("/teamSiteInformation/groupId")
            .and_then(Value::as_str)
            .filter(|group| !group.is_empty())
            .map(str::to_owned),
        mailbox: entry
            .get("smtpAddress")
            .and_then(Value::as_str)
            .filter(|mailbox| !mailbox.is_empty())
            .map(str::to_owned),
        owner: role(entry) == "admin",
        can_add_channel: can_add_channel(entry),
        can_add_private_channel: can_add_private_channel(entry),
        about: about(entry),
        channels,
    }
}

/// What the team pages show of a team entry ([`TeamAbout`]).
fn about(entry: &Value) -> TeamAbout {
    let count = |key: &str| {
        entry
            .pointer(&format!("/membershipSummary/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    TeamAbout {
        tenant: entry
            .get("tenantId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        description: entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        public: entry
            .get("accessType")
            .and_then(Value::as_u64)
            .is_some_and(|access| access != 1),
        people: u32::try_from(count("totalMemberCount").saturating_sub(count("botCount"))).unwrap_or(u32::MAX),
        guest: role(entry) == "guest",
        dynamic_membership: flag(entry, "dynamicMembership"),
        tenant_wide: flag(entry, "isTenantWide"),
        channel_only: flag(entry, "channelOnlyMember"),
        hidden: entry.get("isFavorite").and_then(Value::as_bool) == Some(false),
        picture_etag: entry
            .get("pictureETag")
            .and_then(Value::as_str)
            .filter(|etag| !etag.is_empty())
            .map(str::to_owned),
        site: entry
            .pointer("/teamSiteInformation/sharepointSiteUrl")
            .and_then(Value::as_str)
            .filter(|site| site.starts_with("https://"))
            .map(str::to_owned),
    }
}

/// Whether a channel is in the account's channel list ([`Channel::shown`]).
fn shown(team: &Value, channel: &Value, general: bool) -> bool {
    let (holder, key) = if general {
        (team, "isGeneralChannelFavorite")
    } else {
        (channel, "isFavorite")
    };
    holder.get(key).and_then(Value::as_bool) != Some(false)
}

/// The account's role in a team entry, in lower case (`memberRoleString`: `admin`, `user`, `guest`).
fn role(entry: &Value) -> String {
    entry
        .get("memberRoleString")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Whether a team entry lets the account add a channel, as the web client decides: not in an archived team; an owner
/// always, a member by `teamSettings.createTopic`, a guest by `teamGuestSettings.createTopic`.
fn can_add_channel(entry: &Value) -> bool {
    let setting = |settings: &str| {
        entry
            .get(settings)
            .is_some_and(|settings| flag(settings, "createTopic"))
    };
    !flag(entry, "isArchived")
        && match role(entry).as_str() {
            "admin" => true,
            "user" => setting("teamSettings"),
            "guest" => setting("teamGuestSettings"),
            _ => false,
        }
}

/// Whether a team entry lets the account add a private channel, as the web client decides: not in an archived team;
/// an owner always, a member when members may add channels and `teamSettings.isPrivateChannelCreationEnabled`.
fn can_add_private_channel(entry: &Value) -> bool {
    let settings = entry.get("teamSettings");
    let setting = |key| settings.is_some_and(|settings| flag(settings, key));
    !flag(entry, "isArchived")
        && match role(entry).as_str() {
            "admin" => true,
            "user" => setting("createTopic") && setting("isPrivateChannelCreationEnabled"),
            _ => false,
        }
}
