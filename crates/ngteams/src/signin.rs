//! The desktop's sign-in of one of its accounts (`docs/personal-build.md`, "Its sign-in" and "The binary and its
//! data"): the steps are `teams_app::signin`'s (re-exported here), and the window is a window of this app
//! ([`window_for`]: `teams_signin::window`, its page's engine the sibling `ngteams-web` worker), bound to Entra's
//! sign-in hosts and the account's identity provider hosts. Its refresh token goes to the desktop's Secret Service
//! under [`crate::KEYRING_APPLICATION`].

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::accounts_file::PersonalAccount;
use gpui::{App, Global};
use teams_auth::redirect::RedirectUri;
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_signin::hosts::SignInHosts;
use teams_signin::window::{self, Bound, WindowConfig, Windows};

use crate::setup::Setup;

pub use teams_app::signin::{
    ACCOUNT_DISABLED_CODE, ACCOUNT_TURNED_OFF, CA_FILE_EXAMPLE, PASSWORD_MUST_CHANGE, SignInClients, SignInError,
    SignInWindow, add, add_own, failure, own_failure, ready, refused_host, region_again, sign_in, sign_in_own,
    sign_in_own_again, untrusted_certificate,
};

/// A sign-in window for one request: shows the request's page and gives its answer (blocking: it waits for the person
/// signing in). The app's own is [`window_for`]'s; a test's answers by itself.
pub type Window = Box<dyn FnOnce(SignInRequest) -> SignInAnswer + Send>;

/// The app's sign-in windows, opened on GPUI's foreground for the sign-ins running off it.
struct SignInWindows(Windows);

impl Global for SignInWindows {}

/// The current account setup's shell identity also groups its sign-in worker. In particular a lab sign-in must
/// never share the installed client's taskbar identity, even before its first signed-in main window exists.
pub fn windows_for(setup: &Setup, cx: &mut App) -> Windows {
    let app_id = crate::app::shell_id(setup, false, cx);
    windows_under(app_id, cx)
}

fn windows_under(app_id: &str, cx: &mut App) -> Windows {
    if let Some(windows) = cx.try_global::<SignInWindows>() {
        return windows.0.clone();
    }
    let windows = Windows::serve(app_id, cx);
    cx.set_global(SignInWindows(windows.clone()));
    windows
}

/// The sign-in window for `account`, opened through `windows`: bound to Entra's sign-in hosts and the identity
/// provider's hosts its `accounts.toml` entry names ([`SignInHosts::entra_with`]), its decisions in the app's
/// diagnostic log, its web process extension the one beside this binary. A request whose authorize URL isn't on that
/// bound, or whose redirect URI isn't usable, opens no window and starts no engine.
///
/// # Errors
/// [`SignInError::Hosts`] for an `idp_hosts` entry the bound refuses, [`SignInError::NoWindow`] when the running
/// executable or the extension beside it can't be found.
pub fn window_for(setup: &Setup, account: &PersonalAccount, windows: &Windows) -> Result<Window, SignInError> {
    let hosts = Arc::new(SignInHosts::entra_with(&account.idp_hosts)?);
    let exe = std::env::current_exe().map_err(SignInError::NoWindow)?;
    let extension = window::extension_beside(&exe).ok_or_else(|| {
        SignInError::NoWindow(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {} beside {}", window::EXTENSION_FILE, exe.display()),
        ))
    })?;
    Ok(bound_window(setup, hosts, windows, extension))
}

fn bound_window(setup: &Setup, hosts: Arc<SignInHosts>, windows: &Windows, extension: PathBuf) -> Window {
    let paths = setup.paths.clone();
    let windows = windows.clone();
    Box::new(move |request: SignInRequest| {
        if !hosts.admits_url(&request.authorize) {
            return SignInAnswer::Failed("the authorize URL isn't on the sign-in window's hosts".into());
        }
        if let Err(error) = paths.make_dirs() {
            return SignInAnswer::Failed(format!("the personal data dir: {error}"));
        }
        // before the engine: a request no page could answer never starts, or downloads, one
        if RedirectUri::parse(&request.redirect_uri).is_err() {
            return SignInAnswer::Failed("the redirect URI isn't usable".into());
        }
        let engine = match windows.engine_source(paths.data_dir()) {
            Ok(engine) => engine,
            Err(answer) => return answer,
        };
        windows.ask(WindowConfig {
            request,
            bound: Bound::Hosts(hosts),
            extension,
            engine,
            setup: Box::new(|guard| guard),
        })
    })
}

#[cfg(test)]
#[path = "../tests/support/signin_window.rs"]
mod tests;
