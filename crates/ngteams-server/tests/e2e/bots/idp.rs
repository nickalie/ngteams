//! A test identity provider on loopback: the authorization code grant with PKCE (S256) as an OAuth 2.0 provider
//! serves it, consenting at once, and a refresh grant. It checks the client's id, secret, redirect and verifier, and
//! counts what it issued. Its tokens are synthetic; a browser for the tests follows a sign-in link through it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine as _;
use serde_json::json;
use sha2::{Digest as _, Sha256};

pub const CLIENT_ID: &str = "synthetic-client";
pub const CLIENT_SECRET: &str = "synthetic-client-secret";

struct Shared {
    /// Each issued code: its PKCE challenge and redirect.
    codes: std::sync::Mutex<HashMap<String, (String, String)>>,
    issued: AtomicUsize,
    refreshed: AtomicUsize,
    /// Seconds each access token is good for.
    lifetime: AtomicU64,
    scopes: std::sync::Mutex<Vec<String>>,
    /// The answer every refresh gets instead of a token, while set.
    refresh_failure: std::sync::Mutex<Option<RefreshFailure>>,
}

/// How the provider fails a refresh, when a test asks it to.
#[derive(Clone, Copy, Debug)]
pub enum RefreshFailure {
    /// 503: it isn't there now.
    Unavailable,
    /// 400 `invalid_grant`: the refresh token is gone.
    InvalidGrant,
}

/// The provider, stopped when dropped.
pub struct Idp {
    pub base: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Idp {
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0").await
    }

    /// A provider listening on `address`.
    pub async fn start_on(address: &str) -> Self {
        let shared = Arc::new(Shared {
            codes: std::sync::Mutex::new(HashMap::new()),
            issued: AtomicUsize::new(0),
            refreshed: AtomicUsize::new(0),
            lifetime: AtomicU64::new(3600),
            scopes: std::sync::Mutex::new(Vec::new()),
            refresh_failure: std::sync::Mutex::new(None),
        });
        let router = Router::new()
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { base, shared, task }
    }

    pub fn authorize_url(&self) -> String {
        format!("{}/authorize", self.base)
    }

    pub fn token_url(&self) -> String {
        format!("{}/token", self.base)
    }

    /// From now on, access tokens are good for `seconds`.
    pub fn lifetime(&self, seconds: u64) {
        self.shared.lifetime.store(seconds, Ordering::SeqCst);
    }

    /// From now on, every refresh fails so, or, with none, succeeds again.
    pub fn fail_refreshes(&self, failure: Option<RefreshFailure>) {
        *self.shared.refresh_failure.lock().unwrap() = failure;
    }

    /// How many tokens it issued for a code, and for a refresh token.
    pub fn issued(&self) -> (usize, usize) {
        (
            self.shared.issued.load(Ordering::SeqCst),
            self.shared.refreshed.load(Ordering::SeqCst),
        )
    }

    /// The scopes the last sign-in asked for.
    pub fn scopes(&self) -> Vec<String> {
        self.shared.scopes.lock().unwrap().clone()
    }
}

impl Drop for Idp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn authorize(State(shared): State<Arc<Shared>>, Query(query): Query<HashMap<String, String>>) -> Response {
    let wanted = |key: &str| query.get(key).cloned().unwrap_or_default();
    if wanted("response_type") != "code"
        || wanted("client_id") != CLIENT_ID
        || wanted("code_challenge_method") != "S256"
        || wanted("code_challenge").is_empty()
        || wanted("state").is_empty()
    {
        return (StatusCode::BAD_REQUEST, "bad authorize request").into_response();
    }
    *shared.scopes.lock().unwrap() = wanted("scope").split(' ').map(str::to_owned).collect();
    let code = format!("code-{}", uuid::Uuid::new_v4().simple());
    let redirect = wanted("redirect_uri");
    shared
        .codes
        .lock()
        .unwrap()
        .insert(code.clone(), (wanted("code_challenge"), redirect.clone()));
    let mut target = url::Url::parse(&redirect).unwrap();
    target
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &wanted("state"));
    Redirect::to(target.as_str()).into_response()
}

/// The client's id and secret: HTTP Basic, or in the form.
fn client(headers: &HeaderMap, form: &HashMap<String, String>) -> (String, String) {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());
    if let Some((id, secret)) = basic.as_deref().and_then(|pair| pair.split_once(':')) {
        let decode = |text: &str| {
            url::form_urlencoded::parse(format!("x={text}").as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        };
        return (decode(id), decode(secret));
    }
    let field = |key: &str| form.get(key).cloned().unwrap_or_default();
    (field("client_id"), field("client_secret"))
}

async fn token(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let refused = |error: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    if client(&headers, &form) != (CLIENT_ID.to_owned(), CLIENT_SECRET.to_owned()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid_client"}))).into_response();
    }
    let field = |key: &str| form.get(key).cloned().unwrap_or_default();
    let number = match field("grant_type").as_str() {
        "authorization_code" => {
            let Some((challenge, redirect)) = shared.codes.lock().unwrap().remove(&field("code")) else {
                return refused("invalid_grant");
            };
            let digest = Sha256::digest(field("code_verifier").as_bytes());
            let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            if computed != challenge || field("redirect_uri") != redirect {
                return refused("invalid_grant");
            }
            shared.issued.fetch_add(1, Ordering::SeqCst) + 1
        }
        "refresh_token" if field("refresh_token").starts_with("idp-refresh-") => {
            match *shared.refresh_failure.lock().unwrap() {
                Some(RefreshFailure::Unavailable) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                Some(RefreshFailure::InvalidGrant) => return refused("invalid_grant"),
                None => {}
            }
            shared.refreshed.fetch_add(1, Ordering::SeqCst) + 101
        }
        _ => return refused("unsupported_grant_type"),
    };
    Json(json!({
        "access_token": format!("idp-access-{number}"),
        "token_type": "Bearer",
        "expires_in": shared.lifetime.load(Ordering::SeqCst),
        "refresh_token": format!("idp-refresh-{number}"),
    }))
    .into_response()
}

/// A person's browser: follows a sign-in link, through the provider's consent, to the page that shows their code.
/// `origin` is the server's public origin, reached at `loopback`. The code, and the page.
pub async fn sign_in_in_browser(link: &str, origin: &str, loopback: &str) -> (String, String) {
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let mut at = link.replacen(origin, loopback, 1);
    for _ in 0..5 {
        let response = browser.get(&at).send().await.unwrap();
        let status = response.status();
        if status.is_redirection() {
            let next = response.headers()[header::LOCATION].to_str().unwrap().to_owned();
            at = next.replacen(origin, loopback, 1);
            continue;
        }
        let page = response.text().await.unwrap();
        assert_eq!(status, 200, "{page}");
        return (code_on(&page), page);
    }
    panic!("the sign-in never reached its page");
}

/// The code a sign-in page shows (`<output id="code">123456</output>`).
pub fn code_on(page: &str) -> String {
    let start = page.find("id=\"code\">").map(|at| at + "id=\"code\">".len());
    let Some(start) = start else {
        return String::new();
    };
    page[start..].chars().take_while(char::is_ascii_digit).collect()
}
