//! The settings a bot built with each SDK takes to talk to this server (`docs/server.md`, "Apps and bots"): its id and
//! secret, where it gets its token and where it finds the keys that check the server's. Only the Bot Framework SDKs
//! for .NET and Python get all of it from settings; the JavaScript, Teams and Agents SDKs need a few lines of code for
//! their own token, or the bot registered as unsigned.

use teams_auth::admin::BotAuthMode;

use super::BotConnection;

/// What stands for the secret when it isn't shown any more.
pub const SECRET_PLACEHOLDER: &str = "<the bot's secret>";
/// The scope a bot asks its token for, unless the server's operator chose another (`bots.scope`).
pub const BOT_SCOPE: &str = "https://api.botframework.com";
/// The caller id of the public cloud, which the Bot Framework SDKs stamp on what they receive.
const CALLER_ID: &str = "urn:botframework:azure";

/// An SDK a bot is built with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sdk {
    DotNet,
    Python,
    JavaScript,
    Teams,
    Agents,
}

impl Sdk {
    pub const ALL: [Self; 5] = [Self::DotNet, Self::Python, Self::JavaScript, Self::Teams, Self::Agents];

    /// Its name in its segment's test selector.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::DotNet => "dotnet",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Teams => "teams",
            Self::Agents => "agents",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::DotNet => ".NET",
            Self::Python => "Python",
            Self::JavaScript => "JavaScript",
            Self::Teams => "Teams SDK",
            Self::Agents => "Agents SDK",
        }
    }
}

/// The settings of one SDK: where they go, what to know about them, and the text to paste.
#[derive(Clone, PartialEq, Eq)]
pub struct SdkSettings {
    /// Where the text goes (a file, or the bot's code).
    pub place: &'static str,
    /// What the SDK needs beyond the text, if anything.
    pub note: Option<&'static str>,
    /// Can hold the bot's secret: never in `Debug`.
    pub text: String,
}

impl std::fmt::Debug for SdkSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkSettings")
            .field("place", &self.place)
            .field("note", &self.note)
            .field("text", &"<redacted>")
            .finish()
    }
}

/// The bot's own values in its settings.
#[derive(Clone, Copy)]
pub struct BotValues<'a> {
    pub app_id: &'a str,
    /// Shown once, when the bot just got it; [`SECRET_PLACEHOLDER`] stands for it after.
    pub secret: Option<&'a str>,
    pub mode: BotAuthMode,
}

impl std::fmt::Debug for BotValues<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotValues")
            .field("app_id", &self.app_id)
            .field("secret", &self.secret.map(|_| "<redacted>"))
            .field("mode", &self.mode)
            .finish()
    }
}

/// The server's addresses a signed bot's settings name.
struct Addresses {
    origin: String,
    issuer: String,
    metadata: String,
    /// A Bot Framework SDK's sign-in address: the server's tenant path that MSAL takes as its authority.
    login: String,
    /// Python's and .NET's: an `/adfs` authority, which msal-python and MSAL.NET take without asking Microsoft.
    adfs: String,
}

impl Addresses {
    fn of(connection: &BotConnection) -> Self {
        let origin = connection.origin.trim_end_matches('/').to_owned();
        Self {
            metadata: format!("{origin}/bot/v1/.well-known/openidconfiguration"),
            login: format!("{origin}/botframework.com"),
            adfs: format!("{origin}/adfs"),
            issuer: connection.issuer.clone(),
            origin,
        }
    }
}

/// What a JSON string holds, escaped.
fn quoted(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

/// The settings `sdk` takes for the bot `values` on the server `connection` names.
#[must_use]
pub fn settings(sdk: Sdk, connection: &BotConnection, values: BotValues<'_>) -> SdkSettings {
    match values.mode {
        BotAuthMode::Signed => signed(sdk, &Addresses::of(connection), values),
        BotAuthMode::Unsigned => unsigned(sdk),
    }
}

fn signed(sdk: Sdk, at: &Addresses, values: BotValues<'_>) -> SdkSettings {
    let id = values.app_id;
    let secret = values.secret.unwrap_or(SECRET_PLACEHOLDER);
    match sdk {
        Sdk::DotNet => SdkSettings {
            place: "appsettings.json",
            note: None,
            text: dotnet(at, id, secret),
        },
        Sdk::Python => SdkSettings {
            place: "config.py",
            note: None,
            text: python(at, id, secret),
        },
        Sdk::JavaScript => SdkSettings {
            place: ".env",
            note: Some(
                "With these, the bot checks the server's messages. To answer, it needs a few lines of code that get \
                 its token from this server, because the SDK takes no sign-in address but Microsoft's. Without that \
                 code, register the bot as unsigned.",
            ),
            text: javascript(at, id, secret),
        },
        Sdk::Teams => SdkSettings {
            place: "the app's cloud options, in code",
            note: Some(
                "The Teams SDK takes these in code: its app's cloud options (openIdMetadataUrl, tokenIssuer, \
                 loginEndpoint, botScope) and its credentials. Without that code, register the bot as unsigned.",
            ),
            text: values_list(at, id, secret),
        },
        Sdk::Agents => SdkSettings {
            place: "the agent's connection settings and code",
            note: Some(
                "The Agents SDK for JavaScript checks the server's messages with these settings, and needs a few \
                 lines of code to get its own token. The Agents SDK for Python works with this server only as an \
                 unsigned bot.",
            ),
            text: values_list(at, id, secret),
        },
    }
}

fn dotnet(at: &Addresses, id: &str, secret: &str) -> String {
    let pairs = [
        ("MicrosoftAppType", quoted("MultiTenant")),
        ("MicrosoftAppId", quoted(id)),
        ("MicrosoftAppPassword", quoted(secret)),
        ("MicrosoftAppTenantId", quoted("")),
        ("ToChannelFromBotLoginUrl", quoted(&at.adfs)),
        ("ToChannelFromBotOAuthScope", quoted(BOT_SCOPE)),
        ("ToBotFromChannelTokenIssuer", quoted(&at.issuer)),
        ("ToBotFromChannelOpenIdMetadataUrl", quoted(&at.metadata)),
        ("ToBotFromEmulatorOpenIdMetadataUrl", quoted(&at.metadata)),
        ("OAuthUrl", quoted(&at.origin)),
        ("CallerId", quoted(CALLER_ID)),
        ("ValidateAuthority", "false".to_owned()),
    ];
    let lines: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("  \"{key}\": {value}"))
        .collect();
    format!("{{\n{}\n}}", lines.join(",\n"))
}

fn python(at: &Addresses, id: &str, secret: &str) -> String {
    [
        ("APP_ID", quoted(id)),
        ("APP_PASSWORD", quoted(secret)),
        ("APP_TYPE", quoted("MultiTenant")),
        ("APP_TENANTID", quoted("")),
        ("TO_CHANNEL_FROM_BOT_LOGIN_URL", quoted(&at.adfs)),
        ("TO_CHANNEL_FROM_BOT_OAUTH_SCOPE", quoted(BOT_SCOPE)),
        ("TO_BOT_FROM_CHANNEL_TOKEN_ISSUER", quoted(&at.issuer)),
        ("TO_BOT_FROM_CHANNEL_OPENID_METADATA_URL", quoted(&at.metadata)),
        ("TO_BOT_FROM_EMULATOR_OPENID_METADATA_URL", quoted(&at.metadata)),
        ("OAUTH_URL", quoted(&at.origin)),
        ("CALLER_ID", quoted(CALLER_ID)),
        ("VALIDATE_AUTHORITY", "False".to_owned()),
    ]
    .iter()
    .map(|(key, value)| format!("{key} = {value}"))
    .collect::<Vec<_>>()
    .join("\n")
}

fn javascript(at: &Addresses, id: &str, secret: &str) -> String {
    [
        ("MicrosoftAppType", "MultiTenant"),
        ("MicrosoftAppId", id),
        ("MicrosoftAppPassword", secret),
        ("ToBotFromChannelTokenIssuer", &at.issuer),
        ("ToBotFromChannelOpenIdMetadataUrl", &at.metadata),
        ("ToChannelFromBotLoginUrl", &at.login),
        ("ToChannelFromBotOAuthScope", BOT_SCOPE),
        ("OAuthUrl", &at.origin),
        ("ValidateAuthority", "false"),
    ]
    .iter()
    .map(|(key, value)| format!("{key}={value}"))
    .collect::<Vec<_>>()
    .join("\n")
}

/// The values an SDK set up in code takes, one per line.
fn values_list(at: &Addresses, id: &str, secret: &str) -> String {
    [
        ("App (client) id", id),
        ("App (client) secret", secret),
        ("Tenant", "botframework.com"),
        ("Token issuer", &at.issuer),
        ("OpenID metadata URL", &at.metadata),
        ("Sign-in authority", &at.login),
        ("Bot scope", BOT_SCOPE),
    ]
    .iter()
    .map(|(name, value)| format!("{name}: {value}"))
    .collect::<Vec<_>>()
    .join("\n")
}

/// What an unsigned bot's settings say: no id, no secret.
const UNSIGNED_NOTE: &str = "An unsigned bot sends no token and checks none. Anyone who can reach its endpoint can \
                             post to it: use it only for development or on a private network.";

fn unsigned(sdk: Sdk) -> SdkSettings {
    let (place, text) = match sdk {
        Sdk::DotNet => (
            "appsettings.json",
            "{\n  \"MicrosoftAppType\": \"MultiTenant\",\n  \"MicrosoftAppId\": \"\",\n  \"MicrosoftAppPassword\": \"\"\n}",
        ),
        Sdk::Python => ("config.py", "APP_ID = \"\"\nAPP_PASSWORD = \"\""),
        Sdk::JavaScript => (".env", "MicrosoftAppId=\nMicrosoftAppPassword="),
        Sdk::Teams | Sdk::Agents => (
            "the bot's settings",
            "App (client) id: (none)\nApp (client) secret: (none)",
        ),
    };
    SdkSettings {
        place,
        note: Some(UNSIGNED_NOTE),
        text: text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00000000-0000-4000-8000-0000000000b1";

    fn connection() -> BotConnection {
        BotConnection {
            origin: "https://teams.example.test/".to_owned(),
            issuer: "https://teams.example.test/bot".to_owned(),
        }
    }

    fn signed_values(secret: Option<&str>) -> BotValues<'_> {
        BotValues {
            app_id: ID,
            secret,
            mode: BotAuthMode::Signed,
        }
    }

    #[test]
    fn debug_output_never_holds_the_secret() {
        let values = signed_values(Some("made-up-secret"));
        assert!(!format!("{values:?}").contains("made-up-secret"));
        for sdk in Sdk::ALL {
            let shown = settings(sdk, &connection(), values);
            assert!(!format!("{shown:?}").contains("made-up-secret"), "{}", sdk.key());
        }
    }

    #[test]
    fn dotnet_settings_are_json_with_every_parameterized_key() {
        let shown = settings(Sdk::DotNet, &connection(), signed_values(Some("made-up-secret")));
        assert_eq!(shown.place, "appsettings.json");
        let json: serde_json::Value = serde_json::from_str(&shown.text).unwrap();
        assert_eq!(json["MicrosoftAppId"], ID);
        assert_eq!(json["MicrosoftAppPassword"], "made-up-secret");
        assert_eq!(json["ToChannelFromBotLoginUrl"], "https://teams.example.test/adfs");
        assert_eq!(json["ToChannelFromBotOAuthScope"], BOT_SCOPE);
        assert_eq!(json["ToBotFromChannelTokenIssuer"], "https://teams.example.test/bot");
        assert_eq!(
            json["ToBotFromChannelOpenIdMetadataUrl"],
            "https://teams.example.test/bot/v1/.well-known/openidconfiguration"
        );
        assert_eq!(json["OAuthUrl"], "https://teams.example.test");
        assert_eq!(json["ValidateAuthority"], false);
        assert!(shown.note.is_none());
    }

    #[test]
    fn python_settings_use_the_adfs_authority() {
        let shown = settings(Sdk::Python, &connection(), signed_values(None));
        assert!(
            shown
                .text
                .contains("TO_CHANNEL_FROM_BOT_LOGIN_URL = \"https://teams.example.test/adfs\"")
        );
        assert!(shown.text.contains(&format!("APP_PASSWORD = \"{SECRET_PLACEHOLDER}\"")));
        assert!(shown.text.contains("VALIDATE_AUTHORITY = False"));
    }

    #[test]
    fn the_sdks_that_need_code_say_so() {
        for sdk in [Sdk::JavaScript, Sdk::Teams, Sdk::Agents] {
            let shown = settings(sdk, &connection(), signed_values(Some("s")));
            assert!(shown.note.is_some_and(|note| note.contains("unsigned")), "{sdk:?}");
            assert!(shown.text.contains(ID), "{sdk:?}");
            assert!(shown.text.contains("https://teams.example.test/bot"), "{sdk:?}");
        }
        let js = settings(Sdk::JavaScript, &connection(), signed_values(Some("s")));
        assert!(js.text.contains("MicrosoftAppPassword=s"));
    }

    #[test]
    fn an_unsigned_bot_has_no_id_or_secret_in_any_sdk() {
        for sdk in Sdk::ALL {
            let shown = settings(
                sdk,
                &connection(),
                BotValues {
                    app_id: ID,
                    secret: Some("made-up-secret"),
                    mode: BotAuthMode::Unsigned,
                },
            );
            assert!(
                !shown.text.contains(ID) && !shown.text.contains("made-up-secret"),
                "{sdk:?}"
            );
            assert_eq!(shown.note, Some(UNSIGNED_NOTE));
            assert!(!sdk.key().is_empty() && !sdk.label().is_empty());
        }
    }

    #[test]
    fn a_value_with_quotes_stays_valid_json() {
        let odd = BotConnection {
            origin: "https://teams.example.test".to_owned(),
            issuer: "https://teams.example.test/\"bot\"".to_owned(),
        };
        let shown = settings(Sdk::DotNet, &odd, signed_values(None));
        let json: serde_json::Value = serde_json::from_str(&shown.text).unwrap();
        assert_eq!(
            json["ToBotFromChannelTokenIssuer"],
            "https://teams.example.test/\"bot\""
        );
    }
}
