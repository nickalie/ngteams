//! The AI providers' keys and the Telegram bot token in the app's secret store (its in-memory stand-in here), each under
//! its own account name, and kept apart from it in the read-only mode.

use teams_auth::secrets::{Secret, SecretKey};

use super::*;

#[test]
fn a_key_goes_to_the_store_under_the_providers_id_and_back() {
    let secrets = AppSecrets::in_memory();
    let keys = vault(&secrets, false, AI_ACCOUNT);
    assert!(keys.get("api-1").unwrap().is_none());
    keys.put("api-1", &Secret::new("sk-synthetic-1")).unwrap();
    assert_eq!(keys.get("api-1").unwrap().unwrap().expose(), "sk-synthetic-1");
    let stored = crate::accounts::runtime()
        .unwrap()
        .block_on(secrets.get(&SecretKey::new(AI_ACCOUNT, "api-1")))
        .unwrap();
    assert_eq!(stored.unwrap().expose(), "sk-synthetic-1");
    keys.delete("api-1").unwrap();
    assert!(keys.get("api-1").unwrap().is_none());
    keys.delete("api-1").unwrap();
}

#[test]
fn the_read_only_mode_keeps_its_keys_apart_from_the_store() {
    let secrets = AppSecrets::in_memory();
    vault(&secrets, false, AI_ACCOUNT)
        .put("api-1", &Secret::new("sk-synthetic-2"))
        .unwrap();
    let apart = vault(&secrets, true, AI_ACCOUNT);
    assert!(apart.get("api-1").unwrap().is_none());
    apart.put("api-2", &Secret::new("sk-synthetic-3")).unwrap();
    assert!(vault(&secrets, false, AI_ACCOUNT).get("api-2").unwrap().is_none());
}

#[test]
fn the_telegram_bot_token_goes_to_the_store_under_its_own_name() {
    let secrets = AppSecrets::in_memory();
    let token = BotToken::new(vault(&secrets, false, TELEGRAM_ACCOUNT));
    assert!(token.get().unwrap().is_none());
    token
        .put(&Secret::new("123456789:SYNTHETIC_bot_token_made_up_000000"))
        .unwrap();
    let stored = crate::accounts::runtime()
        .unwrap()
        .block_on(secrets.get(&SecretKey::new(
            TELEGRAM_ACCOUNT,
            teams_ui::stickers::telegram::TOKEN_NAME,
        )))
        .unwrap();
    assert_eq!(stored.unwrap().expose(), "123456789:SYNTHETIC_bot_token_made_up_000000");
    assert!(
        vault(&secrets, false, AI_ACCOUNT)
            .get("telegram-bot")
            .unwrap()
            .is_none()
    );
    token.delete().unwrap();
    assert!(token.get().unwrap().is_none());
}
