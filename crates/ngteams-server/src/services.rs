//! The services this server serves, each under one path prefix of its origin, and the region GTMs authz names them
//! by. `docs/server.md` has the whole prefix table, served or planned; only the served ones are handed out, so the
//! client keeps every other service off.

use std::collections::BTreeMap;

use crate::config::Config;

/// The authz service's own GTM (`<origin>/api/authsvc`): the token endpoint and authz.
pub const AUTH_SERVICE_GTM: &str = "authService";

/// The region GTMs of the served services: name → base URL under the public origin, as authz answers them.
#[must_use]
pub fn region_gtms(config: &Config) -> BTreeMap<&'static str, String> {
    let origin = config.origin();
    let mut gtms = served_gtms(config, &origin);
    // the client hides an optional service whose GTM authz doesn't name (`teams_core::capabilities`)
    if config.workflows.enabled {
        gtms.insert(WORKFLOWS_GTM, format!("{origin}/workflows"));
    }
    if config.apps.enabled {
        gtms.insert(crate::apps::GTM, format!("{origin}{}", middle_tier_prefix(config)));
    }
    if config.outgoing_webhooks.enabled {
        gtms.insert(crate::outgoing::GTM, format!("{origin}{}", middle_tier_prefix(config)));
    }
    gtms
}

/// The GTM that announces Workflows (the Flow API subset under `/workflows/`) while it is served.
pub const WORKFLOWS_GTM: &str = "workflowsService";

fn served_gtms(config: &Config, origin: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        (AUTH_SERVICE_GTM, format!("{origin}/api/authsvc")),
        ("chatServiceAfd", format!("{origin}/api/chatsvc/{}", config.region)),
        ("chatSvcAggAfd", format!("{origin}{}", csa_prefix(config))),
        ("middleTier", format!("{origin}{}", middle_tier_prefix(config))),
        (
            "teamsAndChannelsProvisioningService",
            format!("{origin}{}", provisioning_prefix(config)),
        ),
        ("userProfileService", format!("{origin}{}", user_profile_prefix(config))),
        ("amsV2", format!("{origin}/ams")),
        ("presenceUPS", format!("{origin}{}", presence_prefix(config))),
    ])
}

/// CSA's path under the origin.
#[must_use]
pub fn csa_prefix(config: &Config) -> String {
    format!("/api/csa/{}", config.region)
}

/// The presence service's path under the origin. The client takes a geo of lowercase letters only.
#[must_use]
pub fn presence_prefix(config: &Config) -> String {
    let geo: String = config.region.chars().filter(char::is_ascii_lowercase).collect();
    format!("/ups/{}", if geo.is_empty() { "own" } else { &geo })
}

/// The user profile service's path under the origin.
#[must_use]
pub fn user_profile_prefix(config: &Config) -> String {
    format!("/api/userprofilesvc/{}", config.region)
}

/// The provisioning service's path under the origin.
#[must_use]
pub fn provisioning_prefix(config: &Config) -> String {
    format!("/fabric/{}/templates/api", config.region)
}

/// The middle tier's path under the origin. The client takes a partition only as `<letters>-<digits>` (`amer-03`),
/// so the configured one (`own01`) is put in that form.
#[must_use]
pub fn middle_tier_prefix(config: &Config) -> String {
    let letters: String = config.partition.chars().filter(char::is_ascii_lowercase).collect();
    let digits: String = config.partition.chars().filter(char::is_ascii_digit).collect();
    format!(
        "/api/mt/part/{}-{}",
        if letters.is_empty() { "own" } else { &letters },
        if digits.is_empty() { "01" } else { &digits }
    )
}
