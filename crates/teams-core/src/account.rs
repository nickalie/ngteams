//! Builds an account's session, validated service transports, optional push/calling stacks and engine.
//! The app supplies the shared call floor; credentials are requested only when a transport sends a request.

use std::path::PathBuf;
use std::sync::Arc;

use crate::CallFloor;
use teams_auth::authz::{AuthzClient, AuthzError, CHAT_BASE_GTM, Region};
use teams_auth::secrets::SecretStore;
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_call::Identity;
use teams_call::Tracker;
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;
use teams_proto::Mri;
use teams_proto::{Author, BuildError, Client, ClientInfo, Cloud, Credentials, HttpTransport};
use teams_store::{Store, StoreError};
use teams_trouter::{CallStack, Trouter};

use crate::Outbox;
use crate::calling::{CallMedia, CallParts};
use crate::engine::{self, EngineError, EngineHandle, EngineOptions, Parts, Push};

/// How an account describes its client in its calls: as the web client's calling stack does in the captures
/// (`docs/protocol/calling-echo.md`): its endpoint's and its client's capability numbers and its platform's name, in
/// the form the web client sends it. The native client takes the web client's place in a call.
#[must_use]
pub fn web_client_info() -> ClientInfo {
    ClientInfo {
        endpoint_capabilities: Some(WEB_ENDPOINT_CAPABILITIES),
        client_endpoint_capabilities: Some(WEB_CLIENT_ENDPOINT_CAPABILITIES),
        platform_name: Some(WEB_PLATFORM_NAME.to_owned()),
    }
}

/// The web client's `endpointCapabilities` in the captured starts.
pub const WEB_ENDPOINT_CAPABILITIES: u64 = 73_463;
/// The web client's `clientEndpointCapabilities` in the captured starts.
pub const WEB_CLIENT_ENDPOINT_CAPABILITIES: u64 = 114_259_690;
/// The web client's platform name (`pstnContent.platformName`), in the captured form.
pub const WEB_PLATFORM_NAME: &str = "SkypeSpaces/1415/26100212018/os=linux; osVer=undefined; deviceType=computer; \
                                     browser=chrome; browserVer=155.0.0.0/TsCallingVersion=2026.37.01.11";
/// The language an account's calls name, as the captured starts do.
pub const CALL_LANGUAGE: &str = "en-us";

/// A call's video shown on `output`: the camera encoded at up to 1.5 Mb/s, 30 frames a second, no bigger than 1280 by
/// 720.
#[must_use]
pub fn call_video(output: Arc<dyn crate::VideoOutput>) -> crate::CallVideo {
    crate::CallVideo {
        encoder: EncoderSettings::default(),
        max_size: MaxSize::HD,
        output,
    }
}

/// An account's calling on `devices`: the calls' media on those devices,
/// relayed through Teams' relay, the microphone processed as a call's is, their video `video` (none without), and the
/// account describing its client as the web client does ([`web_client_info`], [`CALL_LANGUAGE`]).
#[must_use]
pub fn account_calls(
    devices: Arc<dyn teams_media::devices::Devices + Send + Sync>,
    video: Option<crate::CallVideo>,
) -> AccountCalls {
    AccountCalls {
        media: CallMedia {
            devices,
            relay: crate::Relay::Teams,
            session: teams_media::session::SessionConfig::default(),
            video,
        },
        client: web_client_info(),
        language_id: Some(CALL_LANGUAGE.to_owned()),
    }
}

/// Why an account's engine couldn't be built or started.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("the region has neither a chatServiceAfd nor a chatService GTM")]
    NoChatBase,
    #[error("the chat transport can't be built: {0}")]
    Transport(#[from] BuildError),
    #[error("the push channel can't be built: {0}")]
    Push(#[from] teams_trouter::BuildError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Authz(#[from] AuthzError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// The account an engine is for, as the binary knows it.
#[derive(Debug)]
pub struct Account<S> {
    /// Its UPN: what its tokens are stored under.
    pub upn: String,
    /// Its MRI (`8:orgid:<oid>`).
    pub mri: String,
    /// The name its posts carry (`imdisplayname`).
    pub display_name: String,
    /// Its store's file (`teams-store`).
    pub store_file: PathBuf,
    /// Its region, as authz gave it.
    pub region: Region,
    /// Where its session takes its tokens from.
    pub secrets: S,
    /// Whether the push channel runs.
    pub push: bool,
    /// Names the binary knows already, by MRI ([`Parts::known_names`]).
    pub known_names: Vec<(String, String)>,
    /// Whether, and how, the account calls; `None` runs the engine without calling.
    pub calls: Option<AccountCalls>,
    /// Read-only (the personal build's `ngteams --read-only`, [`READ_ONLY_POLL`]): see [`start`].
    pub read_only: bool,
}

/// How often a read-only engine catches up by reads, having no push channel ([`EngineOptions::poll`]).
pub const READ_ONLY_POLL: std::time::Duration = std::time::Duration::from_secs(60);

/// What an engine opens of what the binary asked for: its push channel and its calling, neither of them when it is
/// read-only (no second Trouter connection on the account, no endpoint registered, no calling socket).
#[must_use]
pub fn listening(read_only: bool, push: bool, calls: Option<AccountCalls>) -> (bool, Option<AccountCalls>) {
    if read_only { (false, None) } else { (push, calls) }
}

/// An account's calling as the binary gives it: what its calls' media is made of, and how the account describes its
/// client in them. The rest of the account in its calls comes from the account (its MRI, its name, its region and
/// partition), the calling endpoint the socket registers, and its token (its tenant, read before its first test call).
#[derive(Clone, Debug)]
pub struct AccountCalls {
    pub media: CallMedia,
    pub client: ClientInfo,
    /// The language its calls name (`languageId`), as the web client's do.
    pub language_id: Option<String>,
}

/// An account's running engine and the session its requests take their tokens from (the app watches it for a
/// sign-out, [`Session::watch_signed_out`]).
#[derive(Debug)]
pub struct AccountEngine<S> {
    pub handle: EngineHandle,
    pub session: Arc<Session<S>>,
}

/// Builds `account`'s engine as the module docs say, with the app's shared call floor, and starts it
/// ([`engine::start`]: refused while another engine runs on the account).
///
/// A read-only account ([`Account::read_only`]) gets transports that send reads only
/// (`teams_proto::HttpTransport::read_only`: every write refused below the views and logged as `refused_read_only`),
/// no push channel and no calling whatever was asked ([`listening`]), and catches up by reads every
/// [`READ_ONLY_POLL`] instead.
///
/// # Errors
/// See [`AccountError`]; [`AccountError::Engine`] with [`EngineError::AlreadyRunning`] when another engine holds the
/// account.
pub fn start<S: SecretStore + 'static>(
    account: Account<S>,
    floor: CallFloor,
) -> Result<AccountEngine<S>, AccountError> {
    start_for_cloud(account, floor, &Cloud::Microsoft)
}

/// Builds an account engine whose authentication, transports and push stacks belong to `cloud`.
/// Credentials and region must belong to that cloud. Existing callers use [`start`]'s Microsoft default.
///
/// # Errors
/// See [`start`].
pub fn start_for_cloud<S: SecretStore + 'static>(
    account: Account<S>,
    floor: CallFloor,
    cloud: &Cloud,
) -> Result<AccountEngine<S>, AccountError> {
    let Account {
        upn,
        mri,
        display_name,
        store_file,
        region,
        secrets,
        push,
        known_names,
        calls,
        read_only,
    } = account;
    let (push, calls) = listening(read_only, push, calls);
    let tracker = account_tracker(&mri, cloud.clone());
    let writable = |transport: HttpTransport<Session<S>>| {
        if read_only { transport.read_only() } else { transport }
    };
    let store = Store::open(&store_file)?;
    let session = Arc::new(new_session(upn, secrets, region, cloud)?);
    let base = session.service_base(CHAT_BASE_GTM).ok_or(AccountError::NoChatBase)?;
    let presence = session.service_base(PRESENCE_GTM);
    // the writes' transport (the outbox's, the push channel's registration): UPS's base too, for the account's own
    // availability (M2 Task 19), and the middle tier's, for the calendar's writes (the scheduling form's events and
    // its event reads).
    let writes = with_middle_tier(
        || {
            HttpTransport::for_cloud(Arc::clone(&session), &base, cloud.clone())
                .map(|transport| writable(transport.for_account(Mri::new(&mri)).with_call_state(tracker.clone())))
        },
        session.service_base(MIDDLE_TIER_GTM).as_deref(),
    )?;
    let writes = with_team_writes(writes, |gtm| session.service_base(gtm))?;
    let transport = Arc::new(with_presence(writes, presence.as_deref())?);
    let reads = with_middle_tier(
        || {
            HttpTransport::for_cloud(Arc::clone(&session), &base, cloud.clone())
                .map(|transport| writable(transport.for_account(Mri::new(&mri)).with_call_state(tracker.clone())))
        },
        session.service_base(MIDDLE_TIER_GTM).as_deref(),
    )?;
    let reads = with_csa(reads, session.service_base(CSA_GTM).as_deref())?;
    let reads = with_channel_services(reads, |gtm| session.service_base(gtm))?;
    let reads = with_presence(reads, presence.as_deref())?;
    let reads = with_content_services(reads, |gtm| session.service_base(gtm))?;
    // Loki (a person's profile card) on the reads' transport alone: its global base is fixed, and the regional one its
    // configuration names is handed over by the engine at run time (`crate::profile_card`)
    let client = Arc::new(Client::new(reads.with_loki()));
    let push = if push {
        Some(Push::new(Trouter::new(Arc::clone(&transport), Arc::clone(&session))?))
    } else {
        None
    };
    let calls = match calls {
        Some(calls) => {
            let identity = Identity {
                mri: Mri::new(&mri),
                display_name: Some(display_name.clone()),
                endpoint_id: String::new(),
                language_id: calls.language_id,
                tenant_id: String::new(),
                client: calls.client,
                region: session.region().name,
                partition: session.region().partition,
            };
            let socket =
                Trouter::new(Arc::clone(&transport), Arc::clone(&session))?.for_calls(call_stack(&tracker, &floor));
            Some(CallParts::new(socket, identity, calls.media))
        }
        None => None,
    };
    let author = Author::new(Mri::new(&mri), display_name);
    let outbox = Outbox::new(transport, author).with_calls(tracker, floor);
    let handle = engine::start(Parts {
        calls,
        store,
        client,
        outbox,
        push,
        options: EngineOptions {
            poll: read_only.then_some(READ_ONLY_POLL),
            ..EngineOptions::default()
        },
        known_names,
    })?;
    Ok(AccountEngine { handle, session })
}

fn new_session<S: SecretStore>(
    upn: String,
    secrets: S,
    region: Region,
    cloud: &Cloud,
) -> Result<Session<S>, AccountError> {
    Ok(Session::new(
        upn,
        secrets,
        TokenClient::new(EntraConfig::teams_native().for_cloud(cloud))?,
        AuthzClient::for_cloud(cloud)?,
        region,
    ))
}

fn account_tracker(mri: &str, cloud: teams_proto::Cloud) -> Arc<Tracker> {
    Arc::new(Tracker::for_cloud(Mri::new(mri), cloud))
}

fn call_stack(tracker: &Arc<Tracker>, floor: &CallFloor) -> CallStack {
    let observed = Arc::clone(tracker);
    let floor = floor.clone();
    CallStack::new(Arc::clone(tracker)).observing(move |delivery| floor.observe(&observed, delivery))
}

/// The reads' transport `build` makes, given the middle tier's base (the profile lookup's) when the region has one
/// the transport accepts. Without one, or with one of another form (logged, without its value), the lookup is
/// refused and names come from Graph and the messages: an optional source of names never stops the engine.
fn with_middle_tier<C: Credentials>(
    build: impl Fn() -> Result<HttpTransport<C>, BuildError>,
    middle_tier: Option<&str>,
) -> Result<HttpTransport<C>, BuildError> {
    let reads = build()?;
    let Some(middle_tier) = middle_tier else {
        return Ok(reads);
    };
    match reads.with_middle_tier(middle_tier) {
        Err(BuildError::MiddleTierBase) => {
            log::warn!("the region's middleTier base isn't of the form the transport accepts: no profile lookup");
            build()
        }
        other => other,
    }
}

/// The GTM the middle tier's base comes from (the profile lookup's).
pub const MIDDLE_TIER_GTM: &str = "middleTier";

/// The GTM CSA's base comes from (the teams read's).
pub const CSA_GTM: &str = "chatSvcAggAfd";

/// The reads' transport with CSA's base (the teams read's) when the region has one the transport accepts. Without
/// one, or with one of another form (logged, without its value), every teams read is refused (told as a failed read
/// of the teams, and the Teams list says the teams can't be read): an optional read never stops the engine.
fn with_csa<C: Credentials>(reads: HttpTransport<C>, csa: Option<&str>) -> Result<HttpTransport<C>, BuildError> {
    match csa {
        Some(base) if reads.accepts_csa_base_in(base) => reads.with_csa(base),
        Some(_) => {
            log::warn!("the region's chatSvcAggAfd base isn't of the form the transport accepts: no teams read");
            Ok(reads)
        }
        None => Ok(reads),
    }
}

/// The GTM the teams' provisioning service's base comes from (a private channel's creation).
pub const PROVISIONING_GTM: &str = "teamsAndChannelsProvisioningService";

/// A transport with the provisioning service's base when the region has one the transport accepts. Without one, or
/// with one of another form (logged, without its value), a private channel can't be made: it never stops the engine.
fn with_provisioning<C: Credentials>(
    transport: HttpTransport<C>,
    base: Option<&str>,
) -> Result<HttpTransport<C>, BuildError> {
    match base {
        Some(base) if transport.accepts_provisioning_base_in(base) => transport.with_provisioning(base),
        Some(_) => {
            log::warn!("the region's provisioning base isn't of the form the transport accepts: no private channels");
            Ok(transport)
        }
        None => Ok(transport),
    }
}

/// The writes' transport with what a team's writes need: the provisioning service's base (a private channel, an archived
/// team) and CSA's (its tags), each by its GTM from `base`; and AMS's, which a pasted picture is sent to.
fn with_team_writes<C: Credentials>(
    writes: HttpTransport<C>,
    base: impl Fn(&str) -> Option<String>,
) -> Result<HttpTransport<C>, BuildError> {
    let writes = with_provisioning(writes, base(PROVISIONING_GTM).as_deref())?;
    let writes = with_ams(writes, base(AMS_GTM).as_deref())?;
    with_csa(writes, base(CSA_GTM).as_deref())
}

/// The GTM the user profile service's base comes from (the people picker's search in a team).
pub const USER_PROFILE_GTM: &str = "userProfileService";

/// The reads' transport with the user profile service's base when the region has one the transport accepts. Without
/// one, or with one of another form (logged, without its value), a team's people search finds nobody: it never stops
/// the engine.
fn with_user_profile<C: Credentials>(
    reads: HttpTransport<C>,
    base: Option<&str>,
) -> Result<HttpTransport<C>, BuildError> {
    match base {
        Some(base) if reads.accepts_user_profile_base_in(base) => reads.with_user_profile(base),
        Some(_) => {
            log::warn!("the region's user profile base isn't of the form the transport accepts: no team people search");
            Ok(reads)
        }
        None => Ok(reads),
    }
}

/// The reads' transport with what a new channel's reads need, each base by its GTM from `base`.
fn with_channel_services<C: Credentials>(
    reads: HttpTransport<C>,
    base: impl Fn(&str) -> Option<String>,
) -> Result<HttpTransport<C>, BuildError> {
    let reads = with_provisioning(reads, base(PROVISIONING_GTM).as_deref())?;
    with_user_profile(reads, base(USER_PROFILE_GTM).as_deref())
}

/// The reads' transport with the bases of the services a chat's content is read from: AMS (a message's pictures)
/// and MCPS (a meeting's recap), each when the region has one the transport accepts.
fn with_content_services<C: Credentials>(
    reads: HttpTransport<C>,
    base: impl Fn(&str) -> Option<String>,
) -> Result<HttpTransport<C>, BuildError> {
    let reads = with_ams(reads, base(AMS_GTM).as_deref())?;
    with_mcps(reads, base(MCPS_GTM).as_deref())
}

/// The GTM MCPS's base comes from (a meeting's recap: its recordings and transcripts).
pub const MCPS_GTM: &str = "mcpService";

/// The reads' transport with MCPS's base (a meeting's recap) when the region has one the transport accepts. Without
/// one, or with one of another form (logged, without its value), every recap read is refused and the Recap tab says
/// the recap can't be read: an optional read never stops the engine.
fn with_mcps<C: Credentials>(reads: HttpTransport<C>, mcps: Option<&str>) -> Result<HttpTransport<C>, BuildError> {
    match mcps {
        Some(base) if reads.accepts_mcps_base_in(base) => reads.with_mcps(base),
        Some(_) => {
            log::warn!("the region's mcpService base isn't of the form the transport accepts: no meeting recaps");
            Ok(reads)
        }
        None => Ok(reads),
    }
}

/// The GTM AMS's base comes from (the async gateway: a message's pictures' reads).
pub const AMS_GTM: &str = "amsV2";

/// The reads' transport with AMS's base (a message's pictures' reads) when the region has one the transport accepts.
/// Without one, or with one of another form (logged, without its value), every picture read is refused and a message
/// shows its pictures as placeholders: an optional read never stops the engine.
fn with_ams<C: Credentials>(reads: HttpTransport<C>, ams: Option<&str>) -> Result<HttpTransport<C>, BuildError> {
    match ams {
        Some(base) if reads.accepts_ams_base_in(base) => reads.with_ams(base),
        Some(_) => {
            log::warn!("the region's amsV2 base isn't of the form the transport accepts: no message pictures");
            Ok(reads)
        }
        None => Ok(reads),
    }
}

/// The GTM UPS's base comes from (the presence read's).
pub const PRESENCE_GTM: &str = "presenceUPS";

/// A transport with UPS's base when the region has one the transport accepts: the reads' (the presence read's) and the
/// writes' (the account's own availability, M2 Task 19). Without one, or with one of another form (logged, without
/// its value), UPS is refused, everyone's presence stays unknown and the own availability isn't sent: neither ever
/// stops the engine.
fn with_presence<C: Credentials>(
    reads: HttpTransport<C>,
    presence: Option<&str>,
) -> Result<HttpTransport<C>, BuildError> {
    match presence {
        Some(base) if reads.accepts_presence_base_in(base) => reads.with_presence(base),
        Some(_) => {
            log::warn!("the region's presenceUPS base isn't of the form the transport accepts: no presence read");
            Ok(reads)
        }
        None => Ok(reads),
    }
}

#[cfg(test)]
mod tests {
    use std::future::{self, Future};

    use teams_auth::secrets::Secret;
    use teams_auth::token::TokenError;

    use super::*;

    /// Placeholder tokens; nothing is sent.
    pub(super) struct NoTokens;

    impl Credentials for NoTokens {
        fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
        }

        fn bearer_rejected(
            &self,
            audience: &str,
            _: &Secret,
        ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            self.bearer(audience)
        }
    }

    fn reads(middle_tier: Option<&str>) -> Result<HttpTransport<NoTokens>, BuildError> {
        let tokens = Arc::new(NoTokens);
        with_middle_tier(
            || HttpTransport::new(Arc::clone(&tokens), "https://teams.microsoft.com/api/chatsvc/amer"),
            middle_tier,
        )
    }

    #[test]
    fn the_account_tracker_uses_the_account_cloud() {
        use teams_proto::CallState as _;
        let cloud = teams_proto::Cloud::own("https://calls.example:8443").unwrap();
        let tracker = account_tracker("8:orgid:00000000-0000-4000-8000-00000000000a", cloud);
        assert!(tracker.connected("own", "https://calls.example:8443/trouter/v4/f/Endpoint00001/"));
        assert!(!tracker.connected("other", "https://calls.example:9443/trouter/v4/f/Endpoint00001/"));
        assert!(!tracker.connected(
            "microsoft",
            "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/Endpoint00001/"
        ));
    }

    #[test]
    fn a_middle_tier_base_of_another_form_leaves_the_reads_without_the_lookup_and_the_engine_starting() {
        let given = reads(Some("https://teams.microsoft.com/api/mt/part/amer-03")).unwrap();
        assert!(given.has_middle_tier());
        assert!(!reads(None).unwrap().has_middle_tier());
        for other in [
            "http://teams.microsoft.com/api/mt/part/amer-03",
            "https://elsewhere.example/api/mt/part/amer-03",
            "https://teams.microsoft.com/api/mt/part/AMER-03",
            "not a url",
        ] {
            assert!(!reads(Some(other)).unwrap().has_middle_tier(), "{other}");
        }
    }

    #[test]
    fn a_presence_base_of_another_form_leaves_the_reads_without_the_presence_read_and_the_rest_kept() {
        let middle_tier = Some("https://teams.microsoft.com/api/mt/part/amer-03");
        let given = with_presence(
            reads(middle_tier).unwrap(),
            Some("https://teams.microsoft.com/ups/noam"),
        )
        .unwrap();
        assert!(given.has_presence() && given.has_middle_tier());
        assert!(!with_presence(reads(None).unwrap(), None).unwrap().has_presence());
        // the writes' transport, which has no middle tier, takes it too (the account's own availability)
        let writes = with_presence(reads(None).unwrap(), Some("https://teams.microsoft.com/ups/noam")).unwrap();
        assert!(writes.has_presence() && !writes.has_middle_tier());
        for other in [
            "https://presence.teams.microsoft.com",
            "https://teams.microsoft.com/ups/NOAM",
            "not a url",
        ] {
            let kept = with_presence(reads(middle_tier).unwrap(), Some(other)).unwrap();
            assert!(!kept.has_presence() && kept.has_middle_tier(), "{other}");
        }
    }

    #[test]
    fn a_csa_base_of_another_form_leaves_the_reads_without_the_teams_read_and_the_middle_tier_kept() {
        let middle_tier = Some("https://teams.microsoft.com/api/mt/part/amer-03");
        let given = with_csa(
            reads(middle_tier).unwrap(),
            Some("https://teams.microsoft.com/api/csa/amer"),
        )
        .unwrap();
        assert!(given.has_csa() && given.has_middle_tier());
        assert!(!with_csa(reads(None).unwrap(), None).unwrap().has_csa());
        for other in [
            "https://chatsvcagg.teams.microsoft.com/api/csa/amer",
            "https://teams.microsoft.com/api/csa/AMER",
            "not a url",
        ] {
            let kept = with_csa(reads(middle_tier).unwrap(), Some(other)).unwrap();
            assert!(!kept.has_csa() && kept.has_middle_tier(), "{other}");
        }
    }
}

#[cfg(test)]
mod cloud_tests {
    use super::tests::NoTokens;
    use super::*;

    #[test]
    fn optional_services_accept_only_the_accounts_cloud() {
        let cloud = Cloud::own("https://chat.example:8443").unwrap();
        let tokens = Arc::new(NoTokens);
        let build = || {
            HttpTransport::for_cloud(
                Arc::clone(&tokens),
                "https://chat.example:8443/api/chatsvc/amer",
                cloud.clone(),
            )
        };
        for origin in [
            "https://chat.example:8443",
            "https://other.example:8443",
            "https://teams.microsoft.com",
        ] {
            let presence = with_presence(build().unwrap(), Some(&format!("{origin}/ups/noam"))).unwrap();
            assert_eq!(presence.has_presence(), origin == "https://chat.example:8443");
            let csa = with_csa(build().unwrap(), Some(&format!("{origin}/api/csa/amer"))).unwrap();
            assert_eq!(csa.has_csa(), origin == "https://chat.example:8443");
            let mt = with_middle_tier(build, Some(&format!("{origin}/api/mt/part/amer-03"))).unwrap();
            assert_eq!(mt.has_middle_tier(), origin == "https://chat.example:8443");
        }
    }
}
