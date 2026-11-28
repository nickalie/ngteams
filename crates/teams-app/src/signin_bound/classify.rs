//! What a request of the sign-in window is, as far as its guard is concerned: the sign-in rules (`docs/personal-build.md`
//! keeps them for the personal sign-in window too).
//!
//! [`Classifier::classify`] sorts a request into:
//! - [`Classification::Read`]: `GET`, `HEAD` or `OPTIONS`, which change nothing anyone else sees;
//! - [`Classification::Own`]: a mutating request on the own-state list, which changes only the signed-in account's own
//!   session: the Entra token endpoint, Teams' authz exchange and Entra's sign-in form ([`Classifier::sign_in`]), and
//!   in the personal sign-in window (`Classifier::personal`) also Entra's multi-factor and
//!   federation return paths and a `POST` to the identity provider's hosts the user named;
//! - [`Classification::Unknown`]: everything else, denied.
//!
//! An own-state rule matches the method, the exact origin (scheme, host and port), the path segment by segment (a `*`
//! segment stands for exactly one non-empty segment, an Entra tenant) and the query ([`QueryRule`]): the token
//! endpoint takes one of its two captured parameter sets and authz none, and any other query, a repeated parameter
//! among them, makes the request [`Classification::Unknown`]. Entra's sign-in form still takes any query
//! ([`QueryRule::Any`]): no capture has shown its queries yet. The personal window's rules are wider still: its
//! multi-factor and federation paths take any query too, and a `POST` to an identity provider host is own-state on any
//! path and any query.

use url::Url;

/// A kind of own-state request: it changes only the signed-in account's own session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnKind {
    /// The Entra token endpoint: code exchange and refresh.
    EntraToken,
    /// Teams' authz exchange: an access token for a skypetoken and the regional endpoints.
    Authz,
    /// Entra's sign-in form: the username lookup, the password and "Stay signed in?".
    EntraSignIn,
    /// Entra's multi-factor and federation return paths (the sign-in window): no capture has
    /// shown them; they change only the account's own sign-in.
    EntraMultiFactor,
    /// A `POST` to the federated identity provider's hosts the user named (the personal sign-in window only).
    IdentityProvider,
}

impl OwnKind {
    /// The name used in diagnostic decision records.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntraToken => "entra_token",
            Self::Authz => "authz",
            Self::EntraSignIn => "entra_sign_in",
            Self::EntraMultiFactor => "entra_multi_factor",
            Self::IdentityProvider => "identity_provider",
        }
    }
}

/// What the guard makes of one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    /// A reading method: let through.
    Read,
    /// A mutating request on the own-state list: let through.
    Own(OwnKind),
    /// A mutating request nothing classifies: denied.
    Unknown,
}

/// An exact origin and a path matched segment by segment; the query plays no part here (an [`OwnRule`] checks it
/// with its [`QueryRule`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlPattern {
    origin: String,
    path: Vec<String>,
}

impl UrlPattern {
    /// `origin` is `scheme://host[:port]` as a browser serializes it (no default port, lowercase host); `path` starts
    /// with `/`, and a `*` segment stands for one non-empty segment.
    #[must_use]
    pub fn new(origin: &str, path: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            path: segments(path).map(str::to_owned).collect(),
        }
    }

    /// Whether `url` has exactly this origin and a path this pattern matches.
    #[must_use]
    pub fn matches(&self, url: &Url) -> bool {
        if self.origin != url.origin().ascii_serialization() {
            return false;
        }
        let mut path = segments(url.path());
        let mut pattern = self.path.iter();
        loop {
            match (pattern.next(), path.next()) {
                (None, None) => return true,
                (Some(wanted), Some(segment)) if !segment.is_empty() && (wanted == "*" || wanted == segment) => {}
                _ => return false,
            }
        }
    }
}

/// The query an own-state rule takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryRule {
    /// No query at all (not even a bare `?`).
    None,
    /// Exactly one of these parameter sets: every parameter of the set once and no other, each value as its
    /// [`ValueRule`] says. Parameters are read as `key=value` pairs separated by `&`, percent-decoded (`+` as a space);
    /// an empty pair or one without `=` is a query no set takes.
    OneOf(&'static [&'static [(&'static str, ValueRule)]]),
    /// Any query, or none: only Entra's sign-in form ([`Classifier::sign_in`]), whose queries no capture has shown yet.
    Any,
}

/// What a parameter's value may be ([`QueryRule::OneOf`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueRule {
    /// One of these values exactly, after percent-decoding.
    Exact(&'static [&'static str]),
    /// A request id as the web client makes one: a GUID in lower case after one of these prefixes (`""` for none).
    Guid(&'static [&'static str]),
}

impl ValueRule {
    fn admits(self, value: &str) -> bool {
        match self {
            Self::Exact(values) => values.contains(&value),
            Self::Guid(prefixes) => prefixes
                .iter()
                .any(|prefix| value.strip_prefix(prefix).is_some_and(is_lower_guid)),
        }
    }
}

impl QueryRule {
    /// Whether `url`'s query is one this rule takes.
    #[must_use]
    pub fn matches(self, url: &Url) -> bool {
        match self {
            Self::Any => true,
            Self::None => url.query().is_none(),
            Self::OneOf(sets) => url
                .query()
                .and_then(query_pairs)
                .is_some_and(|pairs| sets.iter().any(|set| set_matches(set, &pairs))),
        }
    }
}

/// A query's parameters in order, percent-decoded (`+` as a space); `None` when a pair is empty or has no `=`.
fn query_pairs(query: &str) -> Option<Vec<(String, String)>> {
    query
        .split('&')
        .map(|pair| {
            if !pair.contains('=') {
                return None;
            }
            url::form_urlencoded::parse(pair.as_bytes())
                .next()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
        })
        .collect()
}

/// Whether `pairs` are exactly `set`: as many, each key of the set there once, each value admitted.
fn set_matches(set: &[(&str, ValueRule)], pairs: &[(String, String)]) -> bool {
    pairs.len() == set.len()
        && set.iter().all(|(key, rule)| {
            let mut found = pairs.iter().filter(|(name, _)| name == key);
            matches!((found.next(), found.next()), (Some((_, value)), None) if rule.admits(value))
        })
}

/// A GUID as `8-4-4-4-12` lower-case hex digits.
fn is_lower_guid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len && group.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// One entry of the own-state list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnRule {
    kind: OwnKind,
    method: String,
    url: UrlPattern,
    query: QueryRule,
}

impl OwnRule {
    /// `origin` and `path` as [`UrlPattern::new`] takes them. The rule takes no query ([`QueryRule::None`]) unless
    /// [`OwnRule::with_query`] gives it one.
    #[must_use]
    pub fn new(kind: OwnKind, method: &str, origin: &str, path: &str) -> Self {
        Self {
            kind,
            method: method.to_ascii_uppercase(),
            url: UrlPattern::new(origin, path),
            query: QueryRule::None,
        }
    }

    /// The same rule, taking the queries `query` takes.
    #[must_use]
    pub fn with_query(mut self, query: QueryRule) -> Self {
        self.query = query;
        self
    }

    fn matches(&self, method: &str, url: &Url) -> bool {
        self.method == method && self.url.matches(url) && self.query.matches(url)
    }
}

/// The path's segments after the leading `/`.
fn segments(path: &str) -> std::str::Split<'_, char> {
    path.strip_prefix('/').unwrap_or(path).split('/')
}

/// The authz endpoints of the commercial cloud, as the web client names them.
pub const AUTHZ: [(&str, &str); 4] = [
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authz"),
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authzcore"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authz"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authzcore"),
];

/// The token endpoint's queries, in the two forms every capture shows: the request id alone (a GUID, or `Core-` and a
/// GUID), or after the parameters the web client's nested app adds for the broker (its own client id, the Teams web
/// client's id, and the redirect URI of one of the two Teams origins).
pub const TOKEN_QUERY: QueryRule = QueryRule::OneOf(&[
    &[("client-request-id", ValueRule::Guid(&["", "Core-"]))],
    &[
        ("client_id", ValueRule::Exact(&["f4060917-6abe-40d7-baa6-f634c0eda4ac"])),
        (
            "brk_client_id",
            ValueRule::Exact(&["5e3ce6c0-2b1f-4285-8d4b-75ee78787346"]),
        ),
        (
            "brk_redirect_uri",
            ValueRule::Exact(&[
                "https://teams.cloud.microsoft/v2/authv2",
                "https://teams.microsoft.com/v2/authv2",
            ]),
        ),
        ("client-request-id", ValueRule::Guid(&[""])),
    ],
]);

/// Entra's sign-in form on `login.microsoftonline.com`, as the sign-in page posts it (`*` is `common`,
/// `organizations` or a tenant): the username lookup, the password, and the answer to "Stay signed in?". Nothing
/// of multi-factor sign-in (`SAS/*`) or of the account's security info is on it.
const SIGN_IN: [&str; 4] = ["/*/GetCredentialType", "/*/login", "/kmsi", "/*/kmsi"];

/// Entra's multi-factor sign-in (`SAS/*`: the method's start, its end and the answer's processing) and the federation
/// return paths, on `login.microsoftonline.com` (`*` a tenant or `common`): the personal sign-in window's, which the
/// lab never showed (`docs/personal-build.md`, "Its sign-in").
const MULTI_FACTOR: [&str; 5] = [
    "/*/SAS/BeginAuth",
    "/*/SAS/EndAuth",
    "/*/SAS/ProcessAuth",
    "/*/reprocess",
    "/login.srf",
];

/// The classifier: the own-state list, and the identity provider's origins whose `POST`s are own-state too (the
/// personal sign-in window's; none in the lab's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classifier {
    own: Vec<OwnRule>,
    /// `https://<host>` of each identity provider host: a mutating request there is [`OwnKind::IdentityProvider`].
    idp: Vec<String>,
}

impl Classifier {
    /// The token endpoint and authz: `POST` to the Entra v2 token endpoint of any tenant on `login.microsoftonline.com`
    /// with one of its captured queries ([`TOKEN_QUERY`]), and `POST` to Teams' authz endpoints with no query.
    #[must_use]
    pub fn seeded() -> Self {
        let mut own = vec![
            OwnRule::new(
                OwnKind::EntraToken,
                "POST",
                "https://login.microsoftonline.com",
                "/*/oauth2/v2.0/token",
            )
            .with_query(TOKEN_QUERY),
        ];
        own.extend(
            AUTHZ
                .iter()
                .map(|(origin, path)| OwnRule::new(OwnKind::Authz, "POST", origin, path)),
        );
        Self { own, idp: Vec::new() }
    }

    /// The seeded list plus Entra's sign-in form ([`OwnKind::EntraSignIn`]): what the sign-in window needs to show
    /// Microsoft's sign-in page. The form changes only that account's own sign-in session. Its paths still take any
    /// query ([`QueryRule::Any`]) until captures show which ones the form sends.
    #[must_use]
    pub fn sign_in() -> Self {
        let mut classifier = Self::seeded();
        classifier.own.extend(SIGN_IN.iter().map(|path| {
            OwnRule::new(OwnKind::EntraSignIn, "POST", "https://login.microsoftonline.com", path)
                .with_query(QueryRule::Any)
        }));
        classifier
    }

    /// The personal sign-in window's rules: [`Classifier::sign_in`], Entra's multi-factor and federation
    /// return paths ([`OwnKind::EntraMultiFactor`], any query: no capture showed them), and a `POST` to any path of the
    /// identity provider's hosts `hosts` names ([`OwnKind::IdentityProvider`]).
    #[must_use]
    pub fn personal(hosts: &super::hosts::SignInHosts) -> Self {
        let mut classifier = Self::sign_in();
        classifier.own.extend(MULTI_FACTOR.iter().map(|path| {
            OwnRule::new(
                OwnKind::EntraMultiFactor,
                "POST",
                "https://login.microsoftonline.com",
                path,
            )
            .with_query(QueryRule::Any)
        }));
        classifier.idp = hosts.idp_hosts().iter().map(|host| format!("https://{host}")).collect();
        classifier
    }

    /// The same list plus one more own-state rule. Tests use it to stand a local endpoint in for a Microsoft one.
    #[must_use]
    pub fn with_own(mut self, rule: OwnRule) -> Self {
        self.own.push(rule);
        self
    }

    /// Classifies one request: a read, an own-state request, or unknown. Any method other than the three reading ones
    /// counts as mutating, including ones the classifier has never heard of.
    #[must_use]
    pub fn classify(&self, method: &str, url: &str) -> Classification {
        let method = method.to_ascii_uppercase();
        if ["GET", "HEAD", "OPTIONS"].contains(&method.as_str()) {
            return Classification::Read;
        }
        let Ok(url) = Url::parse(url) else {
            return Classification::Unknown;
        };
        if method == "POST" && self.idp.contains(&url.origin().ascii_serialization()) {
            return Classification::Own(OwnKind::IdentityProvider);
        }
        self.own
            .iter()
            .find(|rule| rule.matches(&method, &url))
            .map_or(Classification::Unknown, |rule| Classification::Own(rule.kind))
    }
}
