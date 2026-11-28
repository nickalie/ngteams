//! The AI providers the user can pick for each AI work: the command-line tools on this computer (Claude Code, the
//! Codex CLI) and the APIs that the app asks with a key (Anthropic, OpenAI, and any OpenAI-compatible server).
//!
//! - **The list** is the settings' ([`crate::settings::Settings::ai_providers`]), in the user's order, each with a
//!   name and an on/off switch. A command-line tool found on the `PATH` that the list does not hold is added to the
//!   end of it, turned on ([`effective`]): Claude Code before Codex. Thus a settings file of an earlier build, which
//!   has no list, gets the order that its Automatic had.
//! - **Automatic** is the first provider in that list that is on and can work: a tool that is installed, or an API
//!   with its key (an OpenAI-compatible server can take no key).
//! - **The keys** are only in the system keyring ([`AiKeys`]), under the provider's id. The settings file keeps only
//!   that a key is there ([`Provider::has_key`]). The key is read on the thread that asks the API, never in a view.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{App, AsyncApp, BorrowAppContext as _, Global};
use teams_auth::secrets::Secret;

use super::catalog::Listed;
use super::{Backend, Failure, Model, Target, ToolChoice, Tools, Work, api};

pub use teams_app::ai::providers::{
    FILE_KEY, MAX_NAME, Provider, base_url_shaped, default_base_url, from_value, id_shaped, to_value,
};

/// Whether `provider` can do a work now, when it is on: a tool installed (in `tools`), an API with a key (an
/// OpenAI-compatible server with a base URL is enough).
#[must_use]
pub fn ready(provider: &Provider, tools: &Tools) -> bool {
    match provider.kind {
        Backend::Claude | Backend::Codex => tools.program(provider.kind).is_some(),
        Backend::Anthropic | Backend::OpenAi => provider.has_key,
        Backend::Compatible => provider.base_url.is_some(),
    }
}

/// The list `configured` (the settings'), with each command-line tool in `tools` that it does not hold added to its
/// end, turned on (see the module docs).
#[must_use]
pub fn effective(configured: &[Provider], tools: &Tools) -> Vec<Provider> {
    let mut list = configured.to_vec();
    for backend in tools.installed() {
        if !list.iter().any(|provider| provider.kind == backend) {
            list.push(Provider::cli(backend));
        }
    }
    list
}

/// The app's list of providers, as the settings last held it ([`crate::settings::SettingsModel`] keeps it current).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Providers(pub Vec<Provider>);

impl Global for Providers {}

impl Providers {
    /// The app's list with the tools found added ([`effective`]).
    pub fn of(cx: &mut App) -> Vec<Provider> {
        let tools = Tools::of(cx);
        let configured = cx.try_global::<Self>().map(|held| held.0.clone()).unwrap_or_default();
        effective(&configured, &tools)
    }

    /// Makes `list` the app's, when it changed.
    pub fn publish(list: &[Provider], cx: &mut App) {
        if cx.try_global::<Self>().is_some_and(|held| held.0 == list) {
            return;
        }
        cx.set_global(Self(list.to_vec()));
    }
}

/// What a pick came to: the provider's kind, its name, and where its work goes.
#[derive(Clone, Debug)]
pub struct Picked {
    pub backend: Backend,
    pub name: String,
    pub target: Target,
}

impl Picked {
    /// What a view says the work runs with, `work` asking `model`.
    #[must_use]
    pub fn label(&self, work: Work, model: &Model) -> String {
        let Target::Api(target) = &self.target else {
            return self.backend.label_of(work, model);
        };
        match api::model_for(self.backend, work, model, target.model.as_deref()) {
            Ok(asked) if self.backend.recommended(work) == Some(asked.as_str()) => {
                format!("{} ({})", self.name, self.backend.label(work))
            }
            Ok(asked) => format!("{} ({asked})", self.name),
            Err(_) => self.name.clone(),
        }
    }
}

/// Where an API's requests go and what reads its key, for the thread that asks it.
#[derive(Clone, Debug)]
pub struct ApiTarget {
    /// The provider's id: its key's name in the keyring.
    pub provider: String,
    /// The name the user gave the provider, for the words of its failures.
    pub name: String,
    pub base_url: String,
    /// The provider's own default model.
    pub model: Option<String>,
    pub has_key: bool,
    pub keys: AiKeys,
}

impl ApiTarget {
    /// The key of the provider, read from the keyring (this blocks: call it off the UI thread).
    ///
    /// # Errors
    /// The kind must have a key and none is stored, or the keyring cannot be read.
    pub fn key(&self, backend: Backend) -> Result<Option<Secret>, Failure> {
        if !self.has_key {
            return if backend == Backend::Compatible {
                Ok(None)
            } else {
                Err(Failure::NoKey(backend))
            };
        }
        match self.keys.get(&self.provider) {
            Ok(Some(key)) => Ok(Some(key)),
            Ok(None) => Err(Failure::NoKey(backend)),
            Err(why) => Err(Failure::Stopped(
                backend,
                Some(format!("the keyring could not be read: {why}")),
            )),
        }
    }
}

/// The provider `choice` comes to, with `list` the providers ([`effective`]).
///
/// # Errors
/// No provider can do the work (Automatic), or the one chosen is gone, turned off, not installed or without a key.
pub(crate) fn pick_from(
    list: &[Provider],
    tools: &Tools,
    keys: &AiKeys,
    choice: &ToolChoice,
) -> Result<Picked, Failure> {
    let provider = match choice {
        ToolChoice::Automatic => list
            .iter()
            .find(|provider| provider.enabled && ready(provider, tools))
            .ok_or(Failure::NotInstalled(None))?,
        ToolChoice::Only(id) => match list.iter().find(|provider| provider.id == *id) {
            Some(provider) => provider,
            None => {
                return Err(Backend::from_key(id)
                    .filter(|backend| backend.is_cli())
                    .map_or(Failure::NoProvider, |backend| Failure::NotInstalled(Some(backend))));
            }
        },
    };
    let backend = provider.kind;
    if !provider.enabled {
        return Err(Failure::TurnedOff(backend).of(&provider.name));
    }
    let target = if backend.is_cli() {
        Target::Program(tools.program(backend).ok_or(Failure::NotInstalled(Some(backend)))?)
    } else {
        if backend != Backend::Compatible && !provider.has_key {
            return Err(Failure::NoKey(backend).of(&provider.name));
        }
        Target::Api(ApiTarget {
            provider: provider.id.clone(),
            name: provider.name.clone(),
            base_url: provider.base_url(),
            model: provider.model.clone(),
            has_key: provider.has_key,
            keys: keys.clone(),
        })
    };
    Ok(Picked {
        backend,
        name: provider.name.clone(),
        target,
    })
}

/// The provider `choice` comes to in the app (as `pick_from` picks): the gate of all AI work.
///
/// # Errors
/// The AI features are off ([`Failure::Off`]); else as `pick_from`.
pub fn pick(choice: &ToolChoice, cx: &mut App) -> Result<Picked, Failure> {
    if !super::enabled(cx) {
        return Err(Failure::Off);
    }
    let list = Providers::of(cx);
    let tools = Tools::of(cx);
    let keys = AiKeys::of(cx);
    pick_from(&list, &tools, &keys, choice)
}

/// What keeps the providers' keys: the system keyring in the app, the memory in tests and the sandbox.
pub trait KeyVault: Send + Sync {
    /// The key of `provider`.
    ///
    /// # Errors
    /// The keyring cannot be read; the error never holds the key.
    fn get(&self, provider: &str) -> Result<Option<Secret>, String>;
    /// Keeps `key` for `provider`, in place of one there.
    ///
    /// # Errors
    /// The keyring cannot be written.
    fn put(&self, provider: &str, key: &Secret) -> Result<(), String>;
    /// Forgets the key of `provider` (none there is not an error).
    ///
    /// # Errors
    /// The keyring cannot be written.
    fn delete(&self, provider: &str) -> Result<(), String>;
}

/// Keys kept in this process only.
#[derive(Default)]
pub struct MemoryKeys(Mutex<HashMap<String, Secret>>);

impl KeyVault for MemoryKeys {
    fn get(&self, provider: &str) -> Result<Option<Secret>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(provider)
            .cloned())
    }

    fn put(&self, provider: &str, key: &Secret) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(provider.to_owned(), key.clone());
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(provider);
        Ok(())
    }
}

/// The app's keys of the AI providers: an app global, the memory's until the app gives its keyring ([`AiKeys::new`]).
#[derive(Clone)]
pub struct AiKeys(Arc<dyn KeyVault>);

impl Global for AiKeys {}

impl fmt::Debug for AiKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AiKeys(<vault>)")
    }
}

impl Default for AiKeys {
    fn default() -> Self {
        Self::new(Arc::new(MemoryKeys::default()))
    }
}

impl AiKeys {
    #[must_use]
    pub fn new(vault: Arc<dyn KeyVault>) -> Self {
        Self(vault)
    }

    /// The app's.
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    /// The key of `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::get`].
    pub fn get(&self, provider: &str) -> Result<Option<Secret>, String> {
        self.0.get(provider)
    }

    /// Keeps `key` for `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::put`].
    pub fn put(&self, provider: &str, key: &Secret) -> Result<(), String> {
        self.0.put(provider, key)
    }

    /// Forgets the key of `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::delete`].
    pub fn delete(&self, provider: &str) -> Result<(), String> {
        self.0.delete(provider)
    }
}

/// What an API listed of its models, or why it could not.
pub type Listing = Result<Vec<Listed>, Failure>;

/// The models each API provider lists (`/v1/models`), by the provider's id: an app global, asked in the background on
/// first use ([`ApiModels::of`]) and again on [`ApiModels::relist`] (its key, base URL or kind changed).
#[derive(Clone, Debug, Default)]
pub struct ApiModels {
    listed: HashMap<String, Listing>,
    /// The providers asked now, with what was asked: a listing of a provider changed since is stale.
    asking: HashMap<String, Provider>,
    /// Set when the AI features go off: a listing not sent yet is not sent.
    stop: Arc<AtomicBool>,
}

impl Global for ApiModels {}

impl ApiModels {
    /// What `provider` listed; `None` while it is asked, which starts on first use.
    pub fn of(provider: &Provider, cx: &mut App) -> Option<Listing> {
        if !super::enabled(cx) {
            return Some(Err(Failure::Off));
        }
        // only read: a mutable access tells the observers, and a view that draws this observes it
        if let Some(held) = cx.try_global::<Self>() {
            if let Some(listing) = held.listed.get(&provider.id) {
                return Some(listing.clone());
            }
            if held.asking.contains_key(&provider.id) {
                return None;
            }
        }
        Self::relist(provider, cx);
        None
    }

    /// Asks `provider` for its models again, in the background.
    pub fn relist(provider: &Provider, cx: &mut App) {
        if !super::enabled(cx) {
            return;
        }
        let held = cx.default_global::<Self>();
        held.listed.remove(&provider.id);
        held.asking.insert(provider.id.clone(), provider.clone());
        let stop = Arc::clone(&held.stop);
        let (asked, stopped) = (provider.clone(), Arc::clone(&stop));
        let keys = AiKeys::of(cx);
        let listing = cx
            .background_executor()
            .spawn(async move { Self::list_unless(&stopped, &asked, &keys) });
        let asked = provider.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let listing = listing.await;
            // a listing asked before AI went off must not take the place of the one asked after it came on again
            if !stop.load(Ordering::SeqCst) {
                cx.update(|cx| Self::listed(asked, listing, cx));
            }
        })
        .detach();
    }

    /// Keeps what `asked` listed, unless it was asked again since.
    fn listed(asked: Provider, listing: Listing, cx: &mut App) {
        cx.default_global::<Self>();
        cx.update_global::<Self, _>(|held, _| {
            if held.asking.get(&asked.id) == Some(&asked) {
                held.asking.remove(&asked.id);
                held.listed.insert(asked.id, listing);
            }
        });
    }

    /// What is set when the AI features go off (see [`ApiModels::list_unless`]).
    pub(crate) fn stop_flag(cx: &mut App) -> Arc<AtomicBool> {
        Arc::clone(&cx.default_global::<Self>().stop)
    }

    /// What `provider` lists (blocking: it is asked), unless `stop` was set before the request is sent.
    pub(crate) fn list_unless(stop: &AtomicBool, provider: &Provider, keys: &AiKeys) -> Listing {
        if stop.load(Ordering::SeqCst) {
            return Err(Failure::Off);
        }
        api::list_models(provider, keys, stop)
    }

    /// The AI features went off: a listing not sent yet is not sent, and one that answers later is dropped (also after
    /// AI is on again), so that the next [`ApiModels::of`] lists again. A request already sent cannot be called back.
    pub(super) fn stop_listing(cx: &mut App) {
        if cx.has_global::<Self>() {
            let held = cx.global_mut::<Self>();
            held.stop.store(true, Ordering::SeqCst);
            held.stop = Arc::default();
            held.asking.clear();
        }
    }

    /// Forgets what `provider` listed (it was removed).
    pub fn forget(provider: &str, cx: &mut App) {
        let held = cx.default_global::<Self>();
        held.listed.remove(provider);
        held.asking.remove(provider);
    }
}

#[cfg(test)]
#[path = "../../tests/support/local_ai_providers_units.rs"]
mod tests;
