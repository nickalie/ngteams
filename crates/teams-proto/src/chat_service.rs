//! Recognition of the HTTPS chat-service base and decoded request path.

use percent_encoding::percent_decode_str;
use url::Url;

use crate::AFD_HOSTS;
use crate::ids::REGIONAL_SUFFIX;

/// The request path below a recognized chat-service base and `v1`, percent-decoded.
/// Requires HTTPS, no user info or explicit non-default port, and an AFD or regional service host.
#[must_use]
pub fn chat_service_path(url: &Url) -> Option<Vec<String>> {
    chat_service_segments(url)?
        .into_iter()
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect()
}

fn chat_service_segments(url: &Url) -> Option<Vec<&str>> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let mut segments = url.path_segments()?;
    if AFD_HOSTS.contains(&host.as_str()) {
        let (api, chatsvc, region) = (segments.next()?, segments.next()?, segments.next()?);
        if api != "api" || chatsvc != "chatsvc" || !is_region(region) {
            return None;
        }
    } else {
        let name = host.strip_suffix(REGIONAL_SUFFIX)?;
        let valid = !name.is_empty()
            && name.split('.').all(|label| {
                !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        if !valid {
            return None;
        }
    }
    (segments.next()? == "v1").then(|| segments.collect())
}

/// A chat service region in a base path: a short lowercase name (`emea`, `amer`, `apac`, …).
fn is_region(region: &str) -> bool {
    (1..=32).contains(&region.len())
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The decoded chat-service path within the account's cloud.
#[must_use]
pub fn chat_service_path_in(cloud: &crate::Cloud, url: &Url) -> Option<Vec<String>> {
    if cloud.is_microsoft() {
        return chat_service_path(url);
    }
    let projected = cloud.project(url, "", "https://teams.microsoft.com")?;
    chat_service_path(&projected)
}
