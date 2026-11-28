//! The desktop's account files: `teams_app::accounts` in the directories this platform keeps them in.

pub use paths::{PERSONAL_DIR, PersonalLocations, overlapping};
pub use teams_app::accounts::{OwnServer, PersonalAccount, PersonalError, PersonalPaths};
mod paths;

/// The desktop's default directories for the account files.
pub trait FromEnv: Sized {
    /// The default ones, from the environment ([`PersonalLocations::from_env`]).
    ///
    /// # Errors
    /// [`PersonalError::NoHome`] when they can't be found.
    fn from_env() -> Result<Self, PersonalError>;
}

impl FromEnv for PersonalPaths {
    fn from_env() -> Result<Self, PersonalError> {
        let locations = PersonalLocations::from_env().ok_or(PersonalError::NoHome)?;
        Ok(Self::new(locations.config_dir, locations.data_dir))
    }
}
