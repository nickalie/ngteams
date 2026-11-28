//! `ngteams-server app …`: the registry from the command line, as the operator (`cli` in the audit).

use std::io::Write;
use std::path::PathBuf;

use clap::Subcommand;

use super::{AuthMode, BotForm, Changes, Registry};
use crate::admin::Actor;
use crate::cli::CliError;
use crate::config::Config;

#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// Registers a bot (`--name`, `--endpoint`) or a Teams app package (`--package`); prints the app's id and its
    /// bot's secret, which is shown only this once.
    Add {
        #[arg(long, required_unless_present = "package", conflicts_with = "package")]
        name: Option<String>,
        #[arg(long, conflicts_with = "package")]
        endpoint: Option<String>,
        #[arg(long, conflicts_with = "package")]
        description: Option<String>,
        /// Neither side authenticates (needs `bots.allow_unsigned`).
        #[arg(long, conflicts_with = "package")]
        unsigned: bool,
        #[arg(long)]
        package: Option<PathBuf>,
    },
    /// Lists the apps: id, state, name and the bot's MRI, tab-separated.
    List,
    /// Turns an app off: its bot gets no token.
    Disable { id: String },
    /// Turns an app on again.
    Enable { id: String },
    /// Gives an app's bot a new secret and prints it; the old one stops working.
    RotateSecret { id: String },
    /// Deletes an app.
    Remove { id: String },
}

fn line(output: &mut dyn Write, text: &str) -> Result<(), CliError> {
    writeln!(output, "{text}").map_err(CliError::Output)
}

/// What `app add` prints: the app, its secret once, and its bot's id, which a package's manifest may set apart.
fn added(output: &mut dyn Write, record: &super::repo::Record, secret: Option<&str>) -> Result<(), CliError> {
    line(output, &format!("added {} ({})", record.app.id, record.app.name))?;
    if let Some(secret) = secret {
        line(output, &format!("secret {secret}"))?;
    }
    if let Some(bot) = &record.bot {
        line(output, &format!("bot {}", bot.id))?;
    }
    Ok(())
}

/// Runs an app command on the configured database and file store.
///
/// # Errors
/// What the registry refuses, or the database, store or output failing.
pub async fn run(config: &Config, command: AppCommand, output: &mut dyn Write) -> Result<(), CliError> {
    let db = crate::db::Db::connect(&config.database_url, 1).await?;
    let storage =
        crate::storage::ObjectStorage::open_with(&config.storage_url, &config.storage_options, &config.storage)?;
    let registry = Registry {
        db: &db,
        storage: &storage,
        config,
    };
    let state = |enabled| Changes {
        enabled: Some(enabled),
        ..Changes::default()
    };
    match command {
        AppCommand::Add {
            name,
            endpoint,
            description,
            unsigned,
            package,
        } => {
            let (record, secret) = if let Some(file) = package {
                let bytes = std::fs::read(&file).map_err(CliError::File)?;
                super::add_package(registry, Actor::CLI, bytes.into()).await?
            } else {
                let form = BotForm {
                    name: name.unwrap_or_default(),
                    description: description.unwrap_or_default(),
                    endpoint,
                    auth_mode: if unsigned { AuthMode::Unsigned } else { AuthMode::Signed },
                };
                let (record, secret) = super::register(registry, Actor::CLI, &form).await?;
                (record, Some(secret))
            };
            added(output, &record, secret.as_deref())
        }
        AppCommand::List => {
            for record in super::repo::list(&db).await? {
                let state = if record.app.enabled { "enabled" } else { "disabled" };
                let bot = record
                    .bot
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |bot| format!("28:{}", bot.id));
                line(
                    output,
                    &format!("{}\t{state}\t{}\t{bot}", record.app.id, record.app.name),
                )?;
            }
            Ok(())
        }
        AppCommand::Disable { id } => {
            super::update(registry, Actor::CLI, &id, state(false)).await?;
            line(output, &format!("disabled {id}"))
        }
        AppCommand::Enable { id } => {
            super::update(registry, Actor::CLI, &id, state(true)).await?;
            line(output, &format!("enabled {id}"))
        }
        AppCommand::RotateSecret { id } => {
            let secret = super::rotate_secret(&db, Actor::CLI, &id).await?;
            line(output, &format!("secret {secret}"))
        }
        AppCommand::Remove { id } => {
            super::delete(registry, Actor::CLI, &id).await?;
            line(output, &format!("removed {id}"))
        }
    }
}
