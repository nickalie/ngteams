//! What every class of request names: the [`Service`] it goes to and its [`Endpoint`] under that service's base; a
//! request that isn't a read also names its [`Method`]. Never a host or an absolute URL: the transport builds the URL
//! from the service's base, which it alone knows, and checks the result against that base.

/// A service the native transport reaches, and so the only hosts it sends to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Service {
    /// Microsoft Power Automate, fixed `https://api.flow.microsoft.com/`, with its own Flow audience.
    Workflows,
    /// Microsoft Power Apps connection service, with its separate Power Apps token.
    WorkflowConnections,
    /// Only a typed workflow input run may use its freshly discovered Microsoft API Hub runtime.
    WorkflowRuntime,
    /// The chat service at the account's chat base, the region's GTM `chatServiceAfd`
    /// (`https://<AFD host>/api/chatsvc/<region>`, or a regional `https://<name>.msg.teams.microsoft.com`): the bases
    /// [`crate::chat_service_path`] recognizes. Its endpoints start with `v1`.
    ChatService,
    /// The registrar, `https://<AFD host>/registrar/…`, on the chat base's AFD host (`teams.microsoft.com` when the
    /// chat base is a regional host). It registers an endpoint's Trouter URL ([`crate::EndpointRegistration`]).
    Registrar,
    /// Trouter's HTTP side, `https://go.trouter.teams.microsoft.com/`.
    Trouter,
    /// The middle tier at the account's middle-tier base, the region's GTM `middleTier`
    /// (`https://<AFD host>/api/mt/part/<partition>`, the partition a region and a number: `amer-03`), which a
    /// transport reaches only once it was given one (`HttpTransport::with_middle_tier`). It takes the account's token
    /// for `https://api.spaces.skype.com`. The profile lookups go here (`beta/users/…`, [`crate::ShortProfileLookup`]),
    /// and the calendar's read (`v2.0/me/calendars/default/calendarView`, [`crate::CalendarView`]).
    MiddleTier,
    /// Microsoft Graph, `https://graph.microsoft.com/v1.0/`, with the account's token for `https://graph.microsoft.com`:
    /// a user's name (`users/<object id>?$select=id,displayName`, [`crate::GraphUser`]) and their 96×96 picture
    /// (`users/<object id>/photos/96x96/$value`, [`crate::GraphPicture`]). Graph is a contacting service: a plain
    /// request to it that isn't a read is refused.
    Graph,
    /// The chat service aggregator (CSA) at the account's CSA base, the region's GTM `chatSvcAggAfd`
    /// (`https://<AFD host>/api/csa/<region>`), which a transport reaches only once it was given one
    /// (`HttpTransport::with_csa`), with the account's token for `https://chatsvcagg.teams.microsoft.com`: the account's
    /// teams and their channels (`api/v3/teams/users/me` with the web client's query, [`crate::CsaTeams`]).
    Csa,
    /// The teams' provisioning service at the account's base for it, the region's GTM
    /// `teamsAndChannelsProvisioningService` (`https://<AFD host>/fabric/<region>/templates/api`), which a transport
    /// reaches only once it was given one (`HttpTransport::with_provisioning`), with the account's token for
    /// `https://templates.teams.microsoft.com`: a private channel made ([`crate::CreateChannel`]), its people added
    /// ([`crate::AddChannelMembers`]) and its creation's status read ([`crate::ChannelCreationStatus`]).
    Provisioning,
    /// The user profile service at the account's base for it, the region's GTM `userProfileService`
    /// (`https://<AFD host>/api/userprofilesvc/<region>`), which a transport reaches only once it was given one
    /// (`HttpTransport::with_user_profile`), with the account's token for `https://api.spaces.skype.com`: the people on a
    /// team a name finds ([`crate::TeamPeopleSearch`], a read sent as a `POST`).
    UserProfile,
    /// UPS, the presence service, at the account's presence base, the region's GTM `presenceUPS`
    /// (`https://<AFD host>/ups/<geo>`, the geo lowercase letters: `noam`), which a transport reaches only once it was
    /// given one (`HttpTransport::with_presence`), with the account's token for
    /// `https://presence.teams.microsoft.com/`: the presence read (`v1/presence/getpresence/`, [`crate::PresenceRead`]),
    /// the subscription of the account's endpoint to others' presence (`v1/pubsub/subscriptions/<endpoint>`,
    /// [`crate::PresenceSubscription`]) and the account's own availability, as an authorized write
    /// (`v1/me/endpoints/`, [`crate::SetEndpointAvailability`]; `v1/me/forceavailability/`,
    /// [`crate::ForceAvailability`]), every path sent with the trailing slash every captured UPS path had but the
    /// subscription's, which never had one.
    Presence,
    /// The calling service: the flight proxy, `https://api.flightproxy.teams.microsoft.com/api/v2/`, with the account's
    /// skypetoken (`X-Skypetoken`, not a bearer token). The call writes go here (`crate::StartCall` and the others):
    /// the call start to `epconv`, every other write to a link the call's services handed out
    /// (`ep/<service host>/…`, a [`crate::CallLink`]), sent exactly as given and checked against the base. Only
    /// authorized call writes are sent here: a read or a plain request on it is refused.
    Calling,
    /// The TURN relay's credentials, `https://teams.microsoft.com/trap/`, with the account's `ic3` token and
    /// `x-ms-migration: True` and `api-version: 2`, as the web client's Entra form of the read
    /// (`docs/protocol/auth.md`): [`crate::RelayTokens`].
    Relay,
    /// Teams' reaction pictures on its public CDN,
    /// `https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/`, with no token
    /// (static files any browser reads): a reaction's or a message's emoticon's animation ([`crate::ReactionAnimation`]).
    Emoticons,
    /// The media service (AMS) through the async gateway at the account's AMS base, the region's GTM `amsV2`
    /// (`https://<name>.asyncgw.teams.microsoft.com`), which a transport reaches only once it was given one
    /// (`HttpTransport::with_ams`), with the account's `ic3` token and `x-ms-migration: True` (the object read's form,
    /// `docs/protocol/chat.md`): a picture pasted into a message, read by its view (`v1/objects/<id>/views/<view>`,
    /// [`crate::MessagePicture`]), and a link preview's picture from the gateway's URL preview service
    /// (`urlp/v1/url/image/Thumbnail?url=<picture>`, [`crate::LinkPreviewPicture`]). Reads only: AMS is a contacting
    /// service, and the transport sends it nothing else.
    Ams,
    /// A file thumbnail's picture, at the pre-authenticated link Graph handed out for it ([`crate::ThumbnailLink`]:
    /// SharePoint's media host `https://<name>.svc.ms`, or a SharePoint host), with no token of the account's (the link
    /// carries its own credential). No base of its own: only [`crate::HttpTransport::file_thumbnail`] reads here, and a
    /// read or a plain request naming this service is refused.
    FileThumbnail,
    /// A shared file's bytes, at the pre-authenticated download link Graph handed out for it
    /// ([`crate::FileContentLink`], a SharePoint host), read part by part with `Range`, with no token of the account's
    /// (the link carries its own credential): a video a message carries, played in its card. No base of its own: only
    /// [`crate::HttpTransport::file_range`] reads here, and a read or a plain request naming this service is refused.
    FileContent,
    /// Giphy's media CDN, `https://media.giphy.com/`, with no token (Giphy's public files, which the web client's
    /// `<img>` loads as any browser does): a GIF a message shows, picked from Teams' GIF picker
    /// (`media/<id>/<file>`, [`crate::GiphyPicture`]). Reads only, nothing of the account's sent.
    Giphy,
    /// App tiles on Microsoft's CDN, `https://res.cdn.office.net/teamsappdata/`, with no token (public files the web
    /// client's `<img>` loads as any browser does): the picture an app definition's `largeImageUrl` names
    /// ([`crate::AppPicture`]). Reads only, nothing of the account's sent.
    AppPictures,
    /// App tiles on Teams' static CDN, `https://statics.teams.cdn.office.net/` (`app-assets/…`,
    /// `evergreen-assets/apps/…`), with no token, as [`Service::AppPictures`] ([`crate::AppPicture`]).
    StaticAppPictures,
    /// The meeting content processing service (MCPS) at the account's base for it, the region's GTM `mcpService`
    /// (`https://<AFD host>/api/mcps/<region>`), which a transport reaches only once it was given one
    /// (`HttpTransport::with_mcps`), with the account's token for the meeting-artifacts app
    /// ([`crate::MCPS_AUDIENCE`]): a meeting's recordings and transcripts, as its recap lists them
    /// ([`crate::MeetingContents`], `contents/?threadId=<chat>`, the path's trailing slash as the web client sends it).
    /// Reads only.
    Mcps,
    /// Substrate's search, `https://substrate.office.com/`, with the account's token for
    /// `https://outlook.office.com/search` ([`crate::SUBSTRATE_AUDIENCE`]): the title bar's search
    /// ([`crate::SubstrateSearch`], `search/api/v1/suggestions` and `searchservice/api/v2/query`), reads by `POST`
    /// sent only by [`crate::HttpTransport::search`]; a read or a plain request naming this service is refused.
    Substrate,
    /// Loki, Microsoft's persona card service (Delve's live persona card, which Teams' profile card reads), on its
    /// fixed global base `https://loki.delve.office.com/`, with the account's token for
    /// `https://loki.delve.office.com` ([`crate::loki::LOKI_AUDIENCE`]): only its configuration read
    /// ([`crate::loki::LokiConfiguration`]), whose answer names the account's regional base ([`Service::Loki`]); the
    /// global host refuses every other path. Reads only, on a transport given Loki (`HttpTransport::with_loki`).
    LokiGlobal,
    /// Loki at the account's regional base (`https://<region>.loki.delve.office.com/`), which the transport takes once
    /// at run time from the configuration's answer (`HttpTransport::set_loki_region`), with the same token: a person's
    /// card ([`crate::loki::LokiPerson`] and the other reads of [`crate::loki`]). Reads only; a read before the region
    /// was set is refused.
    Loki,
}

/// A request's path under its service's base, as path segments (each sent as one encoded segment, so a `/` inside one
/// stays inside it), and its query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    segments: Vec<String>,
    query: Vec<(String, String)>,
}

impl Endpoint {
    /// The path `segments`, with no query. The transport refuses an endpoint with an empty, `.` or `..` segment.
    #[must_use]
    pub fn new<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
            query: Vec::new(),
        }
    }

    /// Adds `name=value` to the query, after any added before.
    #[must_use]
    pub fn with_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    #[must_use]
    pub fn query(&self) -> &[(String, String)] {
        &self.query
    }
}

/// The method of a request that isn't a read: a read is always a `GET` and names no method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    pub(crate) fn to_reqwest(self) -> reqwest::Method {
        match self {
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }
}
