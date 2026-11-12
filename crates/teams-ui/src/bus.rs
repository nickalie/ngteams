//! The Linux session bus the app is given, for its notifications ([`crate::notify`]) and its tray item ([`crate::tray`]).
//!
//! The app is given one address, the `DBUS_SESSION_BUS_ADDRESS` it was started with (in the lab, the UI sandbox's
//! private bus), and connects there and nowhere else: no `$XDG_RUNTIME_DIR/bus`, no other address in a list, no
//! autolaunch. Without an address it has no notifications and no tray. `zbus`'s own session-bus constructors (which
//! fall back to `$XDG_RUNTIME_DIR/bus`) are never called in the app crates, nor GPUI's own system notifications,
//! which use them (`tests/reach.rs`).

use std::ffi::OsStr;

#[cfg(target_os = "linux")]
use zbus::Connection;
#[cfg(target_os = "linux")]
use zbus::address::Address;

/// The variable the address is read from.
pub const ADDRESS_ENV: &str = "DBUS_SESSION_BUS_ADDRESS";

/// Why the app has no bus.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("no session bus was given ({ADDRESS_ENV} is unset or empty): no notifications and no tray")]
    NotGiven,
    #[error("the session bus address given isn't one this app takes: {0}")]
    Address(String),
    #[cfg(target_os = "linux")]
    #[error("the session bus given can't be reached: {0}")]
    Connect(#[source] zbus::Error),
}

/// The bus address the app is given: `value`, the app's `DBUS_SESSION_BUS_ADDRESS`, when it is set, valid UTF-8 and
/// one address (a list of fallbacks, `;`-separated, is refused: only the first could be meant, and it isn't guessed).
///
/// # Errors
/// [`BusError::NotGiven`] without one, [`BusError::Address`] for one that isn't a single address.
pub fn given_address(value: Option<&OsStr>) -> Result<String, BusError> {
    let value = value.ok_or(BusError::NotGiven)?;
    let text = value
        .to_str()
        .ok_or_else(|| BusError::Address("not UTF-8".to_owned()))?
        .trim();
    if text.is_empty() {
        return Err(BusError::NotGiven);
    }
    if text.contains(';') {
        return Err(BusError::Address("a list of addresses".to_owned()));
    }
    #[cfg(target_os = "linux")]
    Address::try_from(text).map_err(|error| BusError::Address(error.to_string()))?;
    Ok(text.to_owned())
}

/// The app's `DBUS_SESSION_BUS_ADDRESS`, read once ([`given_address`]).
///
/// # Errors
/// See [`given_address`].
#[cfg(target_os = "linux")]
pub fn address_from_env() -> Result<String, BusError> {
    given_address(std::env::var_os(ADDRESS_ENV).as_deref())
}

/// A connection to the bus at `address`, and only there.
///
/// # Errors
/// [`BusError::Connect`] when that bus can't be reached or refuses the connection.
#[cfg(target_os = "linux")]
pub async fn connect(address: &str) -> Result<Connection, BusError> {
    zbus::connection::Builder::address(address)
        .map_err(BusError::Connect)?
        .build()
        .await
        .map_err(BusError::Connect)
}

/// A connection to the bus the app was given (`address`, as [`address_from_env`] read it): the one way the app
/// reaches a bus ([`crate::desktop::Desktop::connect`]). Without an address, or at one that can't be reached, there is
/// none, and nothing else is tried.
///
/// # Errors
/// The address's own error, or [`connect`]'s.
#[cfg(target_os = "linux")]
pub async fn connect_given(address: Result<String, BusError>) -> Result<Connection, BusError> {
    connect(&address?).await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use super::*;

    #[test]
    fn only_one_address_given_is_taken() {
        assert!(matches!(given_address(None), Err(BusError::NotGiven)));
        assert!(matches!(given_address(Some(OsStr::new("  "))), Err(BusError::NotGiven)));
        assert_eq!(
            given_address(Some(OsStr::new("unix:path=/tmp/sandbox/bus"))).unwrap(),
            "unix:path=/tmp/sandbox/bus"
        );
        assert!(matches!(
            given_address(Some(OsStr::new("unix:path=/tmp/a;unix:path=/run/user/1000/bus"))),
            Err(BusError::Address(_))
        ));
        assert!(matches!(
            given_address(Some(OsStr::new("no address at all"))),
            Err(BusError::Address(_))
        ));
        let not_utf8 = OsString::from_vec(vec![b'u', 0xff]);
        assert!(matches!(given_address(Some(&not_utf8)), Err(BusError::Address(_))));
    }
}

/// Windows and macOS use their native shells, with no session bus address to read.
///
/// # Errors
/// Always returns `NotGiven`; the Windows and macOS desktops connect directly to their native sessions.
#[cfg(not(target_os = "linux"))]
pub fn address_from_env() -> Result<String, BusError> {
    Err(BusError::NotGiven)
}
