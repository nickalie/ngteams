//! CSA's shapes of a team and a channel, as `docs/protocol/channels.md` and `teams.md` describe them.

use serde_json::{Map, Value, json};

use super::repo::{Channel, Role, Team, TeamMember};

/// A new team's settings: what its members and guests may do (the `spaceAdminSettings` thread property).
pub(super) fn default_settings() -> String {
    json!({
        "userSettings": {
            "createTopic": true, "updateTopic": true, "deleteTopic": true, "createPrivateSpace": true,
            "installApp": true, "uninstallApp": true, "uploadCustomApp": true, "createTab": true, "deleteTab": true,
            "createIntegration": true, "updateIntegration": true, "deleteIntegration": true,
            "deleteEnabled": true, "editEnabled": true, "teamMention": true, "channelMention": true,
            "giphyEnabled": true, "giphyRating": 1, "stickersEnabled": true, "customMemesEnabled": true,
        },
        "guestSettings": { "createTopic": false, "updateTopic": false, "deleteTopic": false },
    })
    .to_string()
}

pub(super) const DEFAULT_TAG_SETTINGS: &str =
    r#"{"onlyOwnersCanUpdate":true,"disallowedSources":[],"enableAutomaticTags":false}"#;

fn settings(team: &Team, of: &str) -> Value {
    serde_json::from_str::<Value>(&team.settings)
        .ok()
        .and_then(|mut settings| settings.get_mut(of).map(Value::take))
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// What the team lets its members do.
pub(super) fn member_settings(team: &Team) -> Value {
    settings(team, "userSettings")
}

/// Whether the team's `userSettings` key `setting` lets its members do it (Teams' "Allow members to …" boxes);
/// `default` when the key isn't there.
pub(crate) fn members_may(team: &Team, setting: &str, default: bool) -> bool {
    member_settings(team)[setting].as_bool().unwrap_or(default)
}

pub(super) fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// What an account sees of a team.
pub(super) struct View<'a> {
    pub tenant: &'a str,
    pub host: &'a str,
    pub team: &'a Team,
    pub member: &'a TeamMember,
    /// The team's channels the account may see.
    pub channels: Vec<&'a Channel>,
    /// The account's own properties, by thread.
    pub own: &'a [(String, Map<String, Value>)],
    pub roles: &'a [Role],
    /// Who made each thread.
    pub creators: &'a [(String, String)],
    pub user: &'a str,
    /// Everyone on the team, and its owners among them.
    pub people: i64,
    pub owners: i64,
}

impl View<'_> {
    fn own(&self, thread: &str) -> Option<&Map<String, Value>> {
        self.own.iter().find(|(id, _)| id == thread).map(|(_, own)| own)
    }

    fn creator(&self, thread: &str) -> &str {
        self.creators
            .iter()
            .find(|(id, _)| id == thread)
            .map_or("", |(_, creator)| creator.as_str())
    }

    fn role(&self, channel: &Channel) -> Option<&Role> {
        self.roles
            .iter()
            .find(|role| role.channel_id == channel.id && role.user_id == self.user)
    }

    fn channel(&self, channel: &Channel) -> Value {
        let owner = if channel.private {
            self.role(channel).is_some_and(|role| role.owner)
        } else {
            self.member.owner
        };
        let mut entry = json!({
            "id": channel.id, "displayName": channel.name, "description": channel.description,
            "parentTeamId": self.team.id, "groupId": self.team.group_id,
            "threadType": if channel.general() || channel.private { "space" } else { "topic" },
            "threadSubType": "", "channelType": u8::from(channel.private),
            "modality": if channel.layout == "PostReply" { "postReply" } else { "conversational" },
            "creator": format!("8:orgid:{}", self.creator(&channel.id)),
            "creationTime": iso(channel.created_at), "lastJoinAt": iso(channel.created_at),
            "lastMessage": {"messageType": null, "content": null, "id": null, "sequenceId": 0, "version": 0},
        });
        let flags = json!({
            "lastL2MessageIdNFS": 0, "isMessageRead": true, "isImportantMessageRead": true,
            "isEmptyConversation": false, "isGapDetectionEnabled": true, "csaV1": false, "interestCountMap": "",
            "version": channel.version, "threadVersion": channel.version, "membershipVersion": channel.version,
            "membershipExpiry": 0, "memberRole": if owner { 2 } else { 0 },
            "memberRoleString": if owner { "Admin" } else { "User" },
            "isGeneral": channel.general(), "isDeleted": false, "isArchived": channel.archived,
            "isFavorite": super::shown(channel, self.own(&channel.id)),
            "isFavoriteByDefault": channel.general() || channel.shown_by_default,
            "isFollowed": false, "isMember": true, "isMuted": false, "isPinned": false, "isShared": false,
            "isModerator": self.role(channel).is_some_and(|role| role.moderator),
            "explicitlyAdded": channel.private, "channelOnlyMember": false, "archivalLevel": "none",
            "tabs": [], "connectorProfiles": [], "defaultFileSettings": {}, "extensionDefinition": {},
        });
        merge(&mut entry, flags);
        if let Some(group) = &channel.group_id {
            entry["substrateGroupId"] = json!(group);
        }
        entry
    }

    pub(super) fn entry(&self) -> Value {
        let team = self.team;
        let mut team_settings = member_settings(team);
        team_settings["isPrivateChannelCreationEnabled"] =
            json!(team_settings["createPrivateSpace"].as_bool() != Some(false));
        team_settings["isSharedChannelCreationEnabled"] = json!(false);
        let general = self.channels.iter().find(|channel| channel.general());
        let mut entry = json!({
            "id": team.id, "displayName": team.name, "description": team.description,
            "channels": self.channels.iter().map(|channel| self.channel(channel)).collect::<Vec<_>>(),
            "creator": format!("8:orgid:{}", self.creator(&team.id)),
            "isCreator": self.creator(&team.id) == self.user,
            "classification": null, "conversationVersion": null, "guestUsersCategory": null,
            "teamStatus": {"exchangeTeamCreationStatus": 0, "sharePointSiteCreationStatus": 0,
                "teamNotebookCreationStatus": 0},
            "extensionDefinition": {"tabsEtag": null, "updatedTime": iso(team.created_at)},
            "interestCountMap": "", "isEmptyConversation": false, "isUnlockMembershipSyncRequired": false,
        });
        let about = json!({
            "tenantId": self.tenant, "smtpAddress": format!("{}@{}", team.group_id, self.host),
            "pictureETag": team.picture_etag, "joiningenabled": "True", "spaceTypes": "standard", "teamType": 0,
            "accessType": if team.public { 3 } else { 1 },
            "memberRole": if self.member.owner { 2 } else { 0 },
            "memberRoleString": if self.member.owner { "admin" } else { "user" },
            "membershipVersion": team.version, "membershipExpiry": 0, "rosterVersion": team.version,
            "threadVersion": team.version.to_string(), "threadSchemaVersion": "v8",
            "dynamicMembership": false, "maximumMemberLimitExceeded": false,
            "membershipSummary": {"totalMemberCount": self.people, "botCount": 0,
                "adminRoleCount": self.owners, "userRoleCount": self.people - self.owners},
            "teamSiteInformation": {"groupId": team.group_id, "isOneNoteProvisioned": false, "notebookId": "",
                "sharepointSiteUrl": ""},
            "lastJoinAt": iso(self.member.joined_at), "archivalLevel": "none",
            "isArchived": team.archived, "isCollapsed": false, "isDeleted": false,
            "isFavorite": !self.member.hidden, "isFollowed": false,
            "isGeneralChannelFavorite": general.is_some_and(|general| super::shown(general, self.own(&general.id))),
            "isTeamLocked": false, "isTenantWide": false, "isUserMuted": false, "channelOnlyMember": false,
            "teamSettings": team_settings, "teamGuestSettings": settings(team, "guestSettings"),
        });
        merge(&mut entry, about);
        entry
    }
}

/// Puts the keys of `more` into `entry`: one `json!` object of every key is past the macro's recursion limit.
fn merge(entry: &mut Value, more: Value) {
    if let (Some(entry), Value::Object(more)) = (entry.as_object_mut(), more) {
        entry.extend(more);
    }
}
