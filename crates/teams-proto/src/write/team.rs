//! Typed team creation and management requests, with raw uploads carried separately from JSON bodies.

use crate::Mri;
use serde_json::{Value, json};

use super::Prepared;
use super::{RawBody, Wire};
use crate::endpoint::{Endpoint, Method, Service};

/// A new team, as the "Create a team" dialog makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTeam {
    pub name: String,
    pub description: String,
    /// The name of its first channel.
    pub first_channel: String,
    /// Anyone in the organization can join it; a private team's people need permission.
    pub public: bool,
}

/// `accessType` of a private team, as the web client's creation sends it.
pub const TEAM_PRIVATE: u64 = 1;

/// `accessType` of a public team (the bundle's list of access types: `None, Private, Secret, Public, …`).
pub const TEAM_PUBLIC: u64 = 3;

/// A person's role in a team.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TeamRole {
    #[default]
    Member,
    Owner,
}

impl TeamRole {
    /// Its `role` in a body adding people.
    #[must_use]
    pub fn wire(self) -> u64 {
        match self {
            Self::Member => 0,
            Self::Owner => 1,
        }
    }
}

/// What happens to someone already on a team: a body's `updateType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemberUpdate {
    /// A member made an owner.
    Promoted,
    /// An owner made a member.
    Demoted,
    /// Someone taken off the team by an owner.
    Removed,
    /// The account leaving, as a member.
    Left,
    /// The account leaving, as an owner.
    DemotedAndLeft,
}

impl MemberUpdate {
    /// Its `updateType`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Promoted => "Promoted",
            Self::Demoted => "Demoted",
            Self::Removed => "Removed",
            Self::Left => "Left",
            Self::DemotedAndLeft => "DemotedAndLeft",
        }
    }
}

/// A write that manages a team (see the module docs). `team` is its thread (its general channel's id), `group` its
/// Microsoft 365 group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamWrite {
    /// An existing channel managed through the same one-attempt engine path.
    Channel(crate::ChannelWrite),
    /// A new team.
    Create(NewTeam),
    /// A team's name, description and privacy; a name or a privacy left out stays as it is.
    Edit {
        team: String,
        name: Option<String>,
        description: String,
        public: Option<bool>,
    },
    /// A team's settings, the whole `spaceAdminSettings` thread property as text.
    Settings { team: String, settings: String },
    /// Who may change a team's tags, and whether apps apply them.
    TagSettings {
        team: String,
        owners_only: bool,
        automatic: bool,
    },
    /// A new tag, with the people who have it.
    CreateTag {
        team: String,
        name: String,
        description: String,
        members: Vec<Mri>,
    },
    /// A tag's name and description; `version` is the version it was read at.
    EditTag {
        team: String,
        tag: String,
        version: u64,
        name: String,
        description: String,
    },
    /// A tag deleted.
    DeleteTag { team: String, tag: String },
    /// A team archived, its site read-only for its members when asked.
    Archive { group: String, site_read_only: bool },
    /// An archived team restored.
    Restore { group: String },
    /// A team deleted.
    Delete { team: String },
    /// People added to a team, each with a role.
    AddMembers {
        team: String,
        group: String,
        members: Vec<(Mri, TeamRole)>,
    },
    /// A member of a private team asking its owners to let people in.
    RequestMembers { group: String, members: Vec<Mri> },
    /// Someone on a team changed, or gone.
    UpdateMember {
        team: String,
        group: String,
        member: Mri,
        update: MemberUpdate,
    },
    /// A team's code made, or made anew.
    ResetCode { team: String },
    /// A team's code taken away.
    RemoveCode { team: String },
    /// A public team joined, or a private one asked to join.
    Join { group: String },
    /// A team joined by its code.
    JoinByCode { code: String },
    /// Someone's request to join let in or turned away.
    AnswerJoinRequest { group: String, member: Mri, accept: bool },
    /// A team's picture changed to an image (PNG or JPEG bytes).
    SetPicture {
        team: String,
        group: String,
        picture: RawBody,
    },
    /// An app taken off a team, by its app id.
    RemoveApp { team: String, app: String },
    /// An app added to a team: its definition as the app catalogue gave it.
    AddApp { team: String, app: AppDefinition },
    /// A new team from one of Teams' templates.
    FromTemplate(TemplateTeam),
    /// A new team copied from another.
    Copy(TeamCopy),
}

/// An app's definition as the app catalogue gives it (`beta/users/apps/batchedDefinitions`), sent whole to add the app
/// to a team. Its bots (`bots[].id`, `inputExtensions[].botId`) join the team, so they are named on the write as
/// `28:<bot id>`, as decoded from the captured fields. `Debug` shows its id alone.
#[derive(Clone, PartialEq, Eq)]
pub struct AppDefinition(serde_json::Map<String, Value>);

impl AppDefinition {
    /// A definition with an app id (`id`), as the catalogue answered it; `None` when it has none, or names a bot in a
    /// unsupported shape (`bots` or `inputExtensions` not a list of objects, a bot id not text).
    #[must_use]
    pub fn new(definition: serde_json::Map<String, Value>) -> Option<Self> {
        definition
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())?;
        for (list, key) in [("bots", "id"), ("inputExtensions", "botId")] {
            let Some(entries) = definition.get(list) else {
                continue;
            };
            for entry in entries.as_array()? {
                if let Some(bot) = entry.as_object()?.get(key) {
                    bot.as_str()?;
                }
            }
        }
        Some(Self(definition))
    }

    /// Its app id.
    #[must_use]
    pub fn id(&self) -> &str {
        self.0.get("id").and_then(Value::as_str).unwrap_or_default()
    }

    /// The name shown by the app catalogue.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.0
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    }

    /// The app's icon URL, for the caller's picture transport.
    #[must_use]
    pub fn large_image_url(&self) -> Option<&str> {
        self.0
            .get("largeImageUrl")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
    }

    /// Every activity type declared by the app; unknown shapes cannot establish the full set.
    #[must_use]
    pub fn activity_types(&self) -> Option<Vec<String>> {
        self.0
            .get("activities")?
            .get("activityTypes")?
            .as_array()?
            .iter()
            .map(|entry| {
                entry
                    .get("type")?
                    .as_str()
                    .filter(|kind| !kind.is_empty())
                    .map(str::to_owned)
            })
            .collect()
    }

    /// The bots it brings onto a team, as MRIs.
    #[must_use]
    pub fn bots(&self) -> Vec<Mri> {
        let ids = |list: &str, key: &str| -> Vec<Mri> {
            self.0
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get(key)?.as_str())
                .map(|bot| Mri::new(format!("28:{bot}")))
                .collect()
        };
        let mut bots = ids("bots", "id");
        bots.extend(ids("inputExtensions", "botId"));
        bots
    }
}

impl std::fmt::Debug for AppDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AppDefinition").field(&self.id()).finish()
    }
}

/// A new team from one of Teams' templates, as "Create a team from a template" makes it: the template's channels (each
/// by its id and the name it was given) and apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateTeam {
    /// The template's id (`com.microsoft.teams.template.ManageAProject`).
    pub template: String,
    pub name: String,
    pub description: String,
    pub public: bool,
    /// The template's locale (`en-US`).
    pub locale: String,
    /// Each channel's id in the template and its name.
    pub channels: Vec<(String, String)>,
    /// The template's apps, by id.
    pub apps: Vec<String>,
}

/// A new team copied from the team of `group`, as "From another team" makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the copy takes along, each ticked on its own in the dialog, as the body sends them"
)]
pub struct TeamCopy {
    pub group: String,
    pub name: String,
    pub description: String,
    pub public: bool,
    /// The account's own object id: the new team's owner.
    pub owner: String,
    /// Its people come along (`includeRoster`).
    pub roster: bool,
    pub channels: bool,
    pub apps: bool,
    pub settings: bool,
    pub tabs: bool,
}

impl TeamWrite {
    /// The wire of a write on the team itself.
    fn details_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::Create(team) => create_wire(team),
            Self::Edit {
                team,
                name,
                description,
                public,
            } => {
                let mut body = json!({
                    "description": description,
                    "displayNamePrefix": "",
                    "displayNameSuffix": "",
                    "validationRequired": false,
                });
                if let Some(name) = name {
                    body["displayName"] = json!(name);
                }
                if let Some(public) = public {
                    body["accessType"] = json!(access_type(*public));
                }
                middle_tier(Method::Patch, ["beta", "teams", team.as_str()], Some(body))
            }
            Self::Settings { team, settings } => Wire {
                raw: None,
                service: Service::ChatService,
                method: Method::Put,
                endpoint: Endpoint::new(["v1", "threads", team.as_str(), "properties"])
                    .with_query("name", "spaceAdminSettings"),
                headers: Vec::new(),
                body: Some(json!({ "spaceAdminSettings": settings })),
            },
            Self::Archive { group, site_read_only } => {
                let endpoint = Endpoint::new(["groups", group.as_str(), "archive"]);
                let endpoint = if *site_read_only {
                    endpoint.with_query("setSpoSiteReadOnlyForMembers", "true")
                } else {
                    endpoint
                };
                provisioning(endpoint)
            }
            Self::Restore { group } => provisioning(Endpoint::new(["groups", group.as_str(), "unarchive"])),
            Self::Delete { team } => middle_tier(Method::Delete, ["beta", "teams", team.as_str(), "delete"], None),
            Self::ResetCode { team } => middle_tier(Method::Put, ["beta", "teams", team.as_str(), "joinCode"], None),
            Self::RemoveCode { team } => {
                middle_tier(Method::Delete, ["beta", "teams", team.as_str(), "joinCode"], None)
            }
            _ => return None,
        })
    }

    /// The wire of a team's picture and apps, and of a team made from a template or another team.
    fn more_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::SetPicture { team, group, picture } => Wire {
                raw: Some(picture.clone()),
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", group.as_str(), "profilepicture"])
                    .with_query("threadId", team),
                headers: Vec::new(),
                body: None,
            },
            Self::RemoveApp { team, app } => middle_tier(
                Method::Delete,
                ["beta", "teams", team.as_str(), "apps", "entitlements", app.as_str()],
                None,
            ),
            Self::AddApp { team, app } => middle_tier(
                Method::Post,
                ["beta", "teams", team.as_str(), "apps", "entitlements"],
                Some(Value::Object(app.0.clone())),
            ),
            Self::FromTemplate(team) => provisioning_with(Endpoint::new(["team"]), template_body(team)),
            Self::Copy(copy) => provisioning_with(
                Endpoint::new(["groups", copy.group.as_str(), "clone"]),
                json!({
                    "displayName": copy.name,
                    "description": copy.description,
                    "ownerObjectId": copy.owner,
                    "teamGroupId": copy.group,
                    "visibility": visibility(copy.public),
                    "sensitivityLabelId": "",
                    "includeRoster": copy.roster,
                    "includeChannels": copy.channels,
                    "includeAppEntitlements": copy.apps,
                    "includeSettings": copy.settings,
                    "includeTabs": copy.tabs,
                }),
            ),
            _ => return None,
        })
    }

    /// The wire of a write on its tags.
    fn tags_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::TagSettings {
                team,
                owners_only,
                automatic,
            } => csa(
                Method::Put,
                [team.as_str(), "tagSettings"].to_vec(),
                Some(json!({
                    "onlyOwnersCanUpdate": owners_only,
                    "disallowedSources": [],
                    "enableAutomaticTags": automatic,
                })),
                None,
            ),
            Self::CreateTag {
                team,
                name,
                description,
                members,
            } => csa(
                Method::Post,
                [team.as_str(), "tags"].to_vec(),
                Some(json!({
                    "tagName": name,
                    "tagDescription": description,
                    "memberIds": members.iter().map(Mri::as_str).collect::<Vec<_>>(),
                })),
                None,
            ),
            Self::EditTag {
                team,
                tag,
                version,
                name,
                description,
            } => csa(
                Method::Patch,
                [team.as_str(), "tags", tag.as_str()].to_vec(),
                Some(json!({ "tagName": name, "tagDescription": description })),
                Some(*version),
            ),
            Self::DeleteTag { team, tag } => csa(
                Method::Delete,
                [team.as_str(), "tags", tag.as_str()].to_vec(),
                None,
                None,
            ),
            _ => return None,
        })
    }

    /// The wire of a write on its people.
    fn people_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::AddMembers { team, group, members } => {
                let users: Vec<Value> = members
                    .iter()
                    .map(|(member, role)| json!({ "mri": member.as_str(), "role": role.wire() }))
                    .collect();
                Wire {
                    raw: None,
                    service: Service::MiddleTier,
                    method: Method::Put,
                    endpoint: Endpoint::new(["beta", "teams", team.as_str(), "bulkUpdateRoledMembers"])
                        .with_query("allowAsyncAddition", "true"),
                    headers: Vec::new(),
                    body: Some(json!({ "users": users, "groupId": group })),
                }
            }
            Self::RequestMembers { group, members } => middle_tier(
                Method::Post,
                ["beta", "teams", "sponsorJoinRequest", group.as_str()],
                Some(json!(members.iter().map(Mri::as_str).collect::<Vec<_>>())),
            ),
            Self::UpdateMember {
                team,
                group,
                member,
                update,
            } => middle_tier(
                Method::Put,
                ["beta", "teams", team.as_str(), "members"],
                Some(json!({ "userMri": member.as_str(), "updateType": update.wire(), "groupId": group })),
            ),
            Self::Join { group } => middle_tier(Method::Post, ["beta", "teams", "joinRequest", group.as_str()], None),
            Self::JoinByCode { code } => Wire {
                raw: None,
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", "joinByCode"])
                    .with_query("restrictJoinToPublicTeam", "false"),
                headers: Vec::new(),
                body: Some(json!(code)),
            },
            Self::AnswerJoinRequest { group, member, accept } => Wire {
                raw: None,
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", "joinRequestAdminAction"])
                    .with_query("silentAction", "false"),
                headers: Vec::new(),
                body: Some(json!({
                    "GroupId": group,
                    "UserMri": member.as_str(),
                    "ActionType": if *accept { "Accept" } else { "Reject" },
                })),
            },
            _ => return None,
        })
    }
}

impl Prepared for TeamWrite {
    fn wire(&self) -> Option<Wire> {
        if let Self::Channel(write) = self {
            return write.wire();
        }
        self.details_wire()
            .or_else(|| self.tags_wire())
            .or_else(|| self.people_wire())
            .or_else(|| self.more_wire())
    }
}

/// `visibility` of a team made from a template or another team.
fn visibility(public: bool) -> &'static str {
    if public { "Public" } else { "Private" }
}

/// A team from a template, as the web client sends it.
fn template_body(team: &TemplateTeam) -> Value {
    json!({
        "apps": team.apps.iter().map(|app| json!({ "id": app })).collect::<Vec<_>>(),
        "channels": team
            .channels
            .iter()
            .map(|(id, name)| json!({ "displayName": name, "id": id }))
            .collect::<Vec<_>>(),
        "description": team.description,
        "displayName": team.name,
        "isTenantWide": false,
        "locale": team.locale,
        "sensitivityLabelId": null,
        "templateId": team.template,
        "visibility": visibility(team.public),
    })
}

/// A `POST` on the provisioning service with a JSON body.
fn provisioning_with(endpoint: Endpoint, body: Value) -> Wire {
    Wire {
        body: Some(body),
        ..provisioning(endpoint)
    }
}

fn access_type(public: bool) -> u64 {
    if public { TEAM_PUBLIC } else { TEAM_PRIVATE }
}

fn create_wire(team: &NewTeam) -> Wire {
    Wire {
        raw: None,
        service: Service::MiddleTier,
        method: Method::Post,
        endpoint: Endpoint::new(["beta", "teams", "create"]),
        headers: Vec::new(),
        body: Some(json!({
            "accessType": access_type(team.public),
            "description": team.description,
            "displayName": team.name,
            "displayNamePrefix": "",
            "displayNameSuffix": "",
            "isTenantWide": false,
            "sensitivityLabelId": null,
            "topicThreadTopic": team.first_channel,
            "validationRequired": false,
        })),
    }
}

fn middle_tier<const N: usize>(method: Method, path: [&str; N], body: Option<Value>) -> Wire {
    Wire {
        raw: None,
        service: Service::MiddleTier,
        method,
        endpoint: Endpoint::new(path),
        headers: Vec::new(),
        body,
    }
}

fn provisioning(endpoint: Endpoint) -> Wire {
    Wire {
        raw: None,
        service: Service::Provisioning,
        method: Method::Post,
        endpoint,
        headers: Vec::new(),
        body: None,
    }
}

/// On CSA, under `api/v1/teams/`; `version` goes as the `if-match` a tag's change needs.
fn csa(method: Method, path: Vec<&str>, body: Option<Value>, version: Option<u64>) -> Wire {
    let mut segments = vec!["api", "v1", "teams"];
    segments.extend(path);
    Wire {
        raw: None,
        service: Service::Csa,
        method,
        endpoint: Endpoint::new(segments),
        headers: version
            .into_iter()
            .map(|version| ("if-match", version.to_string()))
            .collect(),
        body,
    }
}

/// What a new team's answer says: its thread and its group, `{"value": {"objectId", "siteInfo": {"groupId"}}}`.
#[must_use]
pub fn team_created(body: &[u8]) -> Option<(String, Option<String>)> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    let value = answer.get("value")?;
    let thread = value
        .get("objectId")
        .or_else(|| value.get("skypeThreadId"))?
        .as_str()
        .filter(|thread| !thread.is_empty())?
        .to_owned();
    let group = value
        .pointer("/siteInfo/groupId")
        .and_then(Value::as_str)
        .filter(|group| !group.is_empty())
        .map(str::to_owned);
    Some((thread, group))
}

/// The error code a team's write answer names, as [`crate::channel_error_code`] reads a channel's.
#[must_use]
pub fn team_error_code(body: &[u8]) -> Option<String> {
    super::channel::channel_error_code(body)
}

#[cfg(test)]
mod tests {

    use super::*;

    const TEAM: &str = "19:team0001@thread.tacv2";
    const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

    fn definition(value: &Value) -> AppDefinition {
        AppDefinition::new(value.as_object().cloned().expect("an object")).expect("a definition")
    }

    fn copy(roster: bool, public: bool) -> TeamWrite {
        TeamWrite::Copy(TeamCopy {
            group: GROUP.to_owned(),
            name: "Copy".to_owned(),
            description: String::new(),
            public,
            owner: "00000000-0000-4000-8000-00000000000a".to_owned(),
            roster,
            channels: true,
            apps: true,
            settings: true,
            tabs: false,
        })
    }

    #[test]
    fn each_new_request_has_a_wire_form() {
        let template = TeamWrite::FromTemplate(TemplateTeam {
            template: "com.microsoft.teams.template.ManageAProject".to_owned(),
            name: "Project".to_owned(),
            description: String::new(),
            public: false,
            locale: "en-US".to_owned(),
            channels: vec![("General".to_owned(), "General".to_owned())],
            apps: vec!["com.microsoft.teamspace.tab.planner".to_owned()],
        });
        let writes = [
            TeamWrite::SetPicture {
                team: TEAM.to_owned(),
                group: GROUP.to_owned(),
                picture: RawBody::from(b"\x89PNG\r\n".to_vec()),
            },
            TeamWrite::RemoveApp {
                team: TEAM.to_owned(),
                app: "99e46335-5250-4a68-8499-3e4f55e6f3f6".to_owned(),
            },
            TeamWrite::AddApp {
                team: TEAM.to_owned(),
                app: definition(&json!({"id": "a", "bots": [{"id": "b1"}], "inputExtensions": [{"botId": "b2"}]})),
            },
            TeamWrite::AddApp {
                team: TEAM.to_owned(),
                app: definition(&json!({"id": "tab-only"})),
            },
            template,
            copy(true, false),
            copy(false, false),
            copy(false, true),
        ];
        for write in writes {
            assert!(write.wire().is_some());
        }
    }

    #[test]
    fn an_apps_definition_names_its_bots_and_shows_only_its_id() {
        let app = definition(&json!({"id": "a", "name": "Secret name", "bots": [{"id": "b1"}]}));
        assert_eq!(app.id(), "a");
        assert_eq!(app.bots(), [Mri::new("28:b1")]);
        assert!(!format!("{app:?}").contains("Secret"));
        for unreadable in [
            json!({"name": "no id"}),
            json!({"id": "a", "bots": {"id": "b"}}),
            json!({"id": "a", "bots": [{"id": 1}]}),
            json!({"id": "a", "inputExtensions": ["x"]}),
        ] {
            assert!(AppDefinition::new(unreadable.as_object().cloned().unwrap_or_default()).is_none());
        }
    }

    #[test]
    fn a_picture_goes_as_its_bytes_and_shows_only_its_length() {
        let picture = RawBody::from(vec![1, 2, 3]);
        assert_eq!(format!("{picture:?}"), "RawBody(3 bytes)");
        let write = TeamWrite::SetPicture {
            team: TEAM.to_owned(),
            group: GROUP.to_owned(),
            picture,
        };
        let wire = write.wire().expect("a wire form");
        assert_eq!(wire.body_bytes(), Some(vec![1, 2, 3]));
        assert_eq!(wire.endpoint().query(), [("threadId".to_owned(), TEAM.to_owned())]);
    }

    fn mri(text: &str) -> Mri {
        Mri::new(text)
    }

    /// One of each write on a team itself, its tags and its people.
    fn managing_writes() -> Vec<TeamWrite> {
        let team = || TEAM.to_owned();
        let group = || GROUP.to_owned();
        let mut writes = vec![
            TeamWrite::Create(NewTeam {
                name: "Team".to_owned(),
                description: "About".to_owned(),
                first_channel: "General".to_owned(),
                public: true,
            }),
            TeamWrite::Create(NewTeam {
                name: "Team".to_owned(),
                description: String::new(),
                first_channel: "General".to_owned(),
                public: false,
            }),
            TeamWrite::Edit {
                team: team(),
                name: Some("Renamed".to_owned()),
                description: "About".to_owned(),
                public: Some(false),
            },
            TeamWrite::Edit {
                team: team(),
                name: None,
                description: String::new(),
                public: None,
            },
            TeamWrite::Settings {
                team: team(),
                settings: "{}".to_owned(),
            },
            TeamWrite::TagSettings {
                team: team(),
                owners_only: true,
                automatic: false,
            },
            TeamWrite::CreateTag {
                team: team(),
                name: "Tag".to_owned(),
                description: String::new(),
                members: vec![mri("8:orgid:tagged")],
            },
            TeamWrite::EditTag {
                team: team(),
                tag: "tag-1".to_owned(),
                version: 4,
                name: "Tag".to_owned(),
                description: "About".to_owned(),
            },
            TeamWrite::DeleteTag {
                team: team(),
                tag: "tag-1".to_owned(),
            },
            TeamWrite::Archive {
                group: group(),
                site_read_only: true,
            },
            TeamWrite::Archive {
                group: group(),
                site_read_only: false,
            },
            TeamWrite::Restore { group: group() },
            TeamWrite::Delete { team: team() },
        ];
        writes.extend(people_writes());
        writes
    }

    /// One of each write on a team's people and its code, after [`managing_writes`]' own.
    fn people_writes() -> Vec<TeamWrite> {
        let team = || TEAM.to_owned();
        let group = || GROUP.to_owned();
        vec![
            TeamWrite::AddMembers {
                team: team(),
                group: group(),
                members: vec![
                    (mri("8:orgid:member"), TeamRole::Member),
                    (mri("8:orgid:owner"), TeamRole::Owner),
                ],
            },
            TeamWrite::RequestMembers {
                group: group(),
                members: vec![mri("8:orgid:asked")],
            },
            TeamWrite::UpdateMember {
                team: team(),
                group: group(),
                member: mri("8:orgid:member"),
                update: MemberUpdate::Promoted,
            },
            TeamWrite::ResetCode { team: team() },
            TeamWrite::RemoveCode { team: team() },
            TeamWrite::Join { group: group() },
            TeamWrite::JoinByCode {
                code: "abc1234".to_owned(),
            },
            TeamWrite::AnswerJoinRequest {
                group: group(),
                member: mri("8:orgid:asker"),
                accept: true,
            },
            TeamWrite::AnswerJoinRequest {
                group: group(),
                member: mri("8:orgid:asker"),
                accept: false,
            },
        ]
    }

    #[test]
    fn each_managing_request_has_a_wire_form() {
        for write in managing_writes() {
            assert!(write.wire().is_some());
        }
    }

    /// A write's method, its path and query as `path?query`, and its body.
    fn sent(write: &TeamWrite) -> (Service, &'static str, String, Option<Value>) {
        let wire = write.wire().expect("a wire form");
        let mut shown = wire.endpoint().segments().join("/");
        let query: Vec<String> = wire
            .endpoint()
            .query()
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        if !query.is_empty() {
            shown = format!("{shown}?{}", query.join("&"));
        }
        (wire.service(), wire.method().as_str(), shown, wire.body().cloned())
    }

    #[test]
    fn a_new_team_and_its_details_go_in_the_web_clients_shape() {
        let writes = managing_writes();
        let (service, method, path, body) = sent(&writes[0]);
        assert_eq!(
            (service, method, path.as_str()),
            (Service::MiddleTier, "POST", "beta/teams/create")
        );
        let body = body.expect("a body");
        assert_eq!(body["accessType"], json!(TEAM_PUBLIC));
        assert_eq!(body["topicThreadTopic"], json!("General"));
        assert_eq!(body["displayName"], json!("Team"));
        assert_eq!(sent(&writes[1]).3.expect("a body")["accessType"], json!(TEAM_PRIVATE));
        let (service, method, path, body) = sent(&writes[2]);
        assert_eq!(
            (service, method, path),
            (Service::MiddleTier, "PATCH", format!("beta/teams/{TEAM}"))
        );
        let body = body.expect("a body");
        assert_eq!(body["displayName"], json!("Renamed"));
        assert_eq!(body["accessType"], json!(TEAM_PRIVATE));
        let unnamed = sent(&writes[3]).3.expect("a body");
        assert!(unnamed.get("displayName").is_none() && unnamed.get("accessType").is_none());
        let (service, method, path, body) = sent(&writes[4]);
        assert_eq!(
            (service, method, path),
            (
                Service::ChatService,
                "PUT",
                format!("v1/threads/{TEAM}/properties?name=spaceAdminSettings")
            )
        );
        assert_eq!(body, Some(json!({"spaceAdminSettings": "{}"})));
    }

    #[test]
    fn a_teams_tags_go_to_csa_with_the_version_a_change_names() {
        let writes = managing_writes();
        let (service, method, path, body) = sent(&writes[5]);
        assert_eq!(
            (service, method, path),
            (Service::Csa, "PUT", format!("api/v1/teams/{TEAM}/tagSettings"))
        );
        assert_eq!(
            body,
            Some(json!({"onlyOwnersCanUpdate": true, "disallowedSources": [], "enableAutomaticTags": false}))
        );
        let (_, method, path, body) = sent(&writes[6]);
        assert_eq!((method, path), ("POST", format!("api/v1/teams/{TEAM}/tags")));
        assert_eq!(body.expect("a body")["memberIds"], json!(["8:orgid:tagged"]));
        let edit = writes[7].wire().expect("a wire form");
        assert_eq!(edit.method(), Method::Patch);
        assert_eq!(edit.headers(), [("if-match", "4".to_owned())]);
        let (_, method, path, body) = sent(&writes[8]);
        assert_eq!(
            (method, path, body),
            ("DELETE", format!("api/v1/teams/{TEAM}/tags/tag-1"), None)
        );
        assert!(writes[8].wire().expect("a wire form").headers().is_empty());
    }

    #[test]
    fn a_team_is_archived_restored_deleted_and_its_code_kept_where_the_web_client_does() {
        let writes = managing_writes();
        let shown: Vec<_> = writes[9..=17]
            .iter()
            .filter(|write| !matches!(write, TeamWrite::AddMembers { .. } | TeamWrite::RequestMembers { .. }))
            .filter(|write| !matches!(write, TeamWrite::UpdateMember { .. }))
            .map(|write| {
                let (service, method, path, body) = sent(write);
                assert_eq!(body, None, "{write:?}");
                (service, method, path)
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    Service::Provisioning,
                    "POST",
                    format!("groups/{GROUP}/archive?setSpoSiteReadOnlyForMembers=true")
                ),
                (Service::Provisioning, "POST", format!("groups/{GROUP}/archive")),
                (Service::Provisioning, "POST", format!("groups/{GROUP}/unarchive")),
                (Service::MiddleTier, "DELETE", format!("beta/teams/{TEAM}/delete")),
                (Service::MiddleTier, "PUT", format!("beta/teams/{TEAM}/joinCode")),
                (Service::MiddleTier, "DELETE", format!("beta/teams/{TEAM}/joinCode")),
            ]
        );
    }

    #[test]
    fn a_teams_people_are_added_asked_for_changed_and_let_in_on_the_middle_tier() {
        let writes = managing_writes();
        let (_, method, path, body) = sent(&writes[13]);
        assert_eq!(
            (method, path),
            (
                "PUT",
                format!("beta/teams/{TEAM}/bulkUpdateRoledMembers?allowAsyncAddition=true")
            )
        );
        assert_eq!(
            body,
            Some(json!({"users": [
                {"mri": "8:orgid:member", "role": 0},
                {"mri": "8:orgid:owner", "role": 1},
            ], "groupId": GROUP}))
        );
        let (_, method, path, body) = sent(&writes[14]);
        assert_eq!(
            (method, path),
            ("POST", format!("beta/teams/sponsorJoinRequest/{GROUP}"))
        );
        assert_eq!(body, Some(json!(["8:orgid:asked"])));
        let (_, method, path, body) = sent(&writes[15]);
        assert_eq!((method, path), ("PUT", format!("beta/teams/{TEAM}/members")));
        assert_eq!(
            body,
            Some(json!({"userMri": "8:orgid:member", "updateType": "Promoted", "groupId": GROUP}))
        );
        let (_, method, path, body) = sent(&writes[18]);
        assert_eq!(
            (method, path, body),
            ("POST", format!("beta/teams/joinRequest/{GROUP}"), None)
        );
        let (_, method, path, body) = sent(&writes[19]);
        assert_eq!(
            (method, path.as_str(), body),
            (
                "POST",
                "beta/teams/joinByCode?restrictJoinToPublicTeam=false",
                Some(json!("abc1234"))
            )
        );
        for (write, action) in [(&writes[20], "Accept"), (&writes[21], "Reject")] {
            let (_, method, path, body) = sent(write);
            assert_eq!(
                (method, path.as_str()),
                ("POST", "beta/teams/joinRequestAdminAction?silentAction=false")
            );
            assert_eq!(
                body,
                Some(json!({"GroupId": GROUP, "UserMri": "8:orgid:asker", "ActionType": action}))
            );
        }
    }

    #[test]
    fn roles_and_member_updates_have_their_wire_names() {
        assert_eq!(TeamRole::default(), TeamRole::Member);
        assert_eq!((TeamRole::Member.wire(), TeamRole::Owner.wire()), (0, 1));
        let updates = [
            MemberUpdate::Promoted,
            MemberUpdate::Demoted,
            MemberUpdate::Removed,
            MemberUpdate::Left,
            MemberUpdate::DemotedAndLeft,
        ];
        let wires: Vec<_> = updates.iter().map(|update| update.wire()).collect();
        assert_eq!(wires, ["Promoted", "Demoted", "Removed", "Left", "DemotedAndLeft"]);
    }

    #[test]
    fn a_new_teams_answer_names_its_thread_and_group() {
        let created = |body: Value| team_created(body.to_string().as_bytes());
        assert_eq!(
            created(json!({"value": {"objectId": TEAM, "siteInfo": {"groupId": GROUP}}})),
            Some((TEAM.to_owned(), Some(GROUP.to_owned())))
        );
        assert_eq!(
            created(json!({"value": {"skypeThreadId": TEAM, "siteInfo": {"groupId": ""}}})),
            Some((TEAM.to_owned(), None))
        );
        assert_eq!(created(json!({"value": {"objectId": ""}})), None);
        assert_eq!(created(json!({"value": {}})), None);
        assert_eq!(created(json!({})), None);
        assert_eq!(team_created(b"<"), None);
        assert_eq!(
            team_error_code(b"{\"errorCode\": \"NameTaken\"}").as_deref(),
            Some("NameTaken")
        );
    }

    #[test]
    fn a_template_and_a_copy_go_to_the_provisioning_service_with_what_they_take_along() {
        let template = TeamWrite::FromTemplate(TemplateTeam {
            template: "com.microsoft.teams.template.ManageAProject".to_owned(),
            name: "Project".to_owned(),
            description: "About".to_owned(),
            public: true,
            locale: "en-US".to_owned(),
            channels: vec![("Planning".to_owned(), "Plans".to_owned())],
            apps: vec!["app-1".to_owned()],
        });
        let (service, method, path, body) = sent(&template);
        assert_eq!(
            (service, method, path.as_str()),
            (Service::Provisioning, "POST", "team")
        );
        let body = body.expect("a body");
        assert_eq!(body["visibility"], json!("Public"));
        assert_eq!(body["channels"], json!([{"displayName": "Plans", "id": "Planning"}]));
        assert_eq!(body["apps"], json!([{"id": "app-1"}]));
        assert_eq!(body["templateId"], json!("com.microsoft.teams.template.ManageAProject"));
        let (service, method, path, body) = sent(&copy(true, false));
        assert_eq!(
            (service, method, path),
            (Service::Provisioning, "POST", format!("groups/{GROUP}/clone"))
        );
        let body = body.expect("a body");
        assert_eq!(body["visibility"], json!("Private"));
        assert_eq!(
            (&body["includeRoster"], &body["includeTabs"]),
            (&json!(true), &json!(false))
        );
        let (_, method, path, body) = sent(&TeamWrite::RemoveApp {
            team: TEAM.to_owned(),
            app: "app-1".to_owned(),
        });
        assert_eq!(
            (method, path, body),
            ("DELETE", format!("beta/teams/{TEAM}/apps/entitlements/app-1"), None)
        );
        let (_, method, path, body) = sent(&TeamWrite::AddApp {
            team: TEAM.to_owned(),
            app: definition(&json!({"id": "app-2", "name": "App"})),
        });
        assert_eq!((method, path), ("POST", format!("beta/teams/{TEAM}/apps/entitlements")));
        assert_eq!(body, Some(json!({"id": "app-2", "name": "App"})));
    }
}
