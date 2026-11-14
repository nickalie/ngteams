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
    BTreeMap::from([(AUTH_SERVICE_GTM, format!("{origin}/api/authsvc"))])
}
