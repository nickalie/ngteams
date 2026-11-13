//! An app's Activity notification preference: preserve the whole account setting before changing one app.

use serde_json::{Map, Value};

use crate::{Endpoint, Read, Service};

/// The account's own properties, including `userGraphSettings` as JSON text.
#[derive(Clone, Copy, Debug)]
pub struct UserGraphSettingsRead;

impl UserGraphSettingsRead {
    /// The preferences sync's known empty-account error: HTTP 404 with error subcode 1.
    #[must_use]
    pub fn is_absent_answer(body: &[u8]) -> bool {
        serde_json::from_slice::<Value>(body).is_ok_and(|error| {
            error["errorCode"] == 404
                && error["standardizedError"]["errorCode"] == 404
                && error["standardizedError"]["errorSubCode"] == 1
        })
    }

    /// The setting an answer of `status` carries: the known empty-account 404 is empty; any other failure is unread.
    ///
    /// # Errors
    /// Returns [`UserGraphSettingsError`] for a failed read or an unknown answer.
    pub fn settings_of(status: u16, body: &[u8]) -> Result<UserGraphSettings, UserGraphSettingsError> {
        match status {
            404 if Self::is_absent_answer(body) => Ok(UserGraphSettings::default()),
            200..=299 => UserGraphSettings::read_answer(body),
            _ => Err(UserGraphSettingsError),
        }
    }
}

impl Read for UserGraphSettingsRead {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "properties"])
    }
}

/// The whole app notification setting. Unknown keys are retained; `Debug` omits its contents.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct UserGraphSettings(Map<String, Value>);

impl std::fmt::Debug for UserGraphSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserGraphSettings").finish_non_exhaustive()
    }
}

/// One app's preference before Turn off, including absence (the service default).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppNotificationPreference(Option<Map<String, Value>>);

impl std::fmt::Debug for AppNotificationPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppNotificationPreference").finish_non_exhaustive()
    }
}

/// An unfamiliar setting cannot safely be overwritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the account's app notification settings could not be read")]
pub struct UserGraphSettingsError;

impl UserGraphSettings {
    /// Reads the own-properties answer. An absent setting is empty; malformed values are refused.
    ///
    /// # Errors
    /// Returns [`UserGraphSettingsError`] for an unknown answer or setting shape.
    pub fn read_answer(body: &[u8]) -> Result<Self, UserGraphSettingsError> {
        let answer: Map<String, Value> = serde_json::from_slice(body).map_err(|_| UserGraphSettingsError)?;
        let Some(setting) = answer.get("userGraphSettings") else {
            return Ok(Self::default());
        };
        let text = setting.as_str().ok_or(UserGraphSettingsError)?;
        serde_json::from_str(text).map(Self).map_err(|_| UserGraphSettingsError)
    }

    /// Turns off the app's declared activity types and default, preserving every unrelated preference.
    ///
    /// # Errors
    /// Returns [`UserGraphSettingsError`] if an existing app or category has an unknown shape.
    pub fn turn_off(&mut self, app: &str, types: &[String]) -> Result<(), UserGraphSettingsError> {
        if app.is_empty() || types.iter().any(String::is_empty) {
            return Err(UserGraphSettingsError);
        }
        let mut setting = self.0.get(app).cloned().unwrap_or_else(|| Value::Object(Map::new()));
        let settings = setting.as_object_mut().ok_or(UserGraphSettingsError)?;
        for key in ["activityGenerationSettings", "toastSettings"] {
            let category = settings.entry(key).or_insert_with(|| Value::Object(Map::new()));
            let category = category.as_object_mut().ok_or(UserGraphSettingsError)?;
            for kind in types.iter().map(String::as_str).chain(std::iter::once("systemDefault")) {
                category.insert(kind.to_owned(), Value::Bool(false));
            }
        }
        settings
            .entry("emailSettings")
            .or_insert_with(|| Value::Object(Map::new()));
        self.0.insert(app.to_owned(), setting);
        Ok(())
    }

    /// Whether the app's activity of `activity_type` shows a banner. As Teams reads it, a type the app saved no choice
    /// for shows one ("Show in Activity and banner"); "Show in Activity" saves its toast `false`, "Off" its generation
    /// too, and an unfamiliar value shows none.
    #[must_use]
    pub fn banner(&self, app: &str, activity_type: &str) -> bool {
        let Some(setting) = self.0.get(app) else {
            return true;
        };
        ["activityGenerationSettings", "toastSettings"].iter().all(|category| {
            match setting.get(*category).and_then(|category| category.get(activity_type)) {
                None => true,
                Some(Value::Bool(on)) => *on,
                Some(_) => false,
            }
        })
    }

    /// The app's current preference, for Undo.
    ///
    /// # Errors
    /// Returns [`UserGraphSettingsError`] for an app preference in an unknown shape.
    pub fn app_preference(&self, app: &str) -> Result<AppNotificationPreference, UserGraphSettingsError> {
        match self.0.get(app) {
            None => Ok(AppNotificationPreference(None)),
            Some(Value::Object(value)) => Ok(AppNotificationPreference(Some(value.clone()))),
            _ => Err(UserGraphSettingsError),
        }
    }

    /// Restores only this app; preferences changed for other apps since Turn off are kept.
    pub fn restore_app(&mut self, app: &str, preference: &AppNotificationPreference) {
        if let Some(value) = &preference.0 {
            self.0.insert(app.to_owned(), Value::Object(value.clone()));
        } else {
            self.0.remove(app);
        }
    }

    /// The complete setting, as the write's JSON string value.
    #[must_use]
    pub fn json_text(&self) -> String {
        Value::Object(self.0.clone()).to_string()
    }
}
