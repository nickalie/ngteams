//! The keys of the AI providers ([`teams_ui::local_ai::AiKeys`]) in the app's own keyring namespace, under the
//! account-independent name [`AI_ACCOUNT`] and the provider's id, and the Telegram bot token of the sticker import
//! ([`teams_ui::stickers::telegram::BotToken`]) under [`TELEGRAM_ACCOUNT`]. A lab alias and the read-only mode keep
//! them in the memory only: a lab broker serves only its one account, and the read-only mode touches no keyring item.

use std::sync::Arc;

use gpui::App;
use teams_auth::secrets::{Secret, SecretKey, SecretStore as _};
use teams_ui::local_ai::providers::MemoryKeys;
use teams_ui::local_ai::{AiKeys, KeyVault};
use teams_ui::stickers::telegram::BotToken;

use crate::secrets::AppSecrets;

/// The keyring's account name of every AI provider's key: no Teams account has it (it is not an address).
pub const AI_ACCOUNT: &str = "ngteams-ai-providers";

/// The keyring's account name of the Telegram bot token: no Teams account has it (it is not an address).
pub const TELEGRAM_ACCOUNT: &str = "ngteams-telegram-bot";

/// The app's keyring, asked on a runtime of its own for each operation, under the keyring's account name `account`.
struct KeyringKeys {
    secrets: AppSecrets,
    account: &'static str,
}

impl KeyringKeys {
    fn run<T>(
        &self,
        operation: impl AsyncFnOnce(&AppSecrets) -> Result<T, teams_auth::secrets::SecretError>,
    ) -> Result<T, String> {
        let runtime = crate::accounts::runtime()?;
        // a clone connects on this runtime, not on the one of an earlier operation
        let secrets = self.secrets.clone();
        runtime.block_on(operation(&secrets)).map_err(|error| error.to_string())
    }
}

impl KeyVault for KeyringKeys {
    fn get(&self, provider: &str) -> Result<Option<Secret>, String> {
        let key = SecretKey::new(self.account, provider);
        self.run(async |secrets| secrets.get(&key).await)
    }

    fn put(&self, provider: &str, secret: &Secret) -> Result<(), String> {
        let key = SecretKey::new(self.account, provider);
        self.run(async |secrets| secrets.put(&key, secret).await)
    }

    fn delete(&self, provider: &str) -> Result<(), String> {
        let key = SecretKey::new(self.account, provider);
        self.run(async |secrets| secrets.delete(&key).await)
    }
}

/// Makes the app's AI keys and Telegram bot token those of `secrets`, or of the memory for a lab alias and the
/// read-only mode.
pub fn install(secrets: &AppSecrets, read_only: bool, cx: &mut App) {
    cx.set_global(AiKeys::new(vault(secrets, read_only, AI_ACCOUNT)));
    cx.set_global(BotToken::new(vault(secrets, read_only, TELEGRAM_ACCOUNT)));
}

/// What keeps the keys under `account`: `secrets`, or the memory for a lab alias and the read-only mode.
fn vault(secrets: &AppSecrets, read_only: bool, account: &'static str) -> Arc<dyn KeyVault> {
    let lab = secrets.application() == crate::lab::LAB_KEYRING_APPLICATION;
    if read_only || lab {
        Arc::new(MemoryKeys::default())
    } else {
        Arc::new(KeyringKeys {
            secrets: secrets.clone(),
            account,
        })
    }
}

#[cfg(test)]
#[path = "../tests/support/ai_keys.rs"]
mod tests;
