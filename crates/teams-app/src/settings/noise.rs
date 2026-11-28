//! The noise suppression level of each account's calls (Settings > Calls > Devices, and the call's audio menu), as
//! Teams keeps it for each user: Auto until the account chooses another. The file keeps a table of level names under
//! the account's MRI, only for the accounts that chose another level than Auto.
//!
//! Each account's calls read their level from a live control (the desktop's `settings::noise::follow`), so a change
//! applies to a call at once.

use std::collections::BTreeMap;

use teams_media_types::NoiseSuppression;
use toml::{Table, Value};

use super::{Problem, chat_id, keys};

/// What each level says, in the settings and the call's audio menu: Teams' words, shortened.
#[must_use]
pub fn label(level: NoiseSuppression) -> &'static str {
    match level {
        NoiseSuppression::Auto => "Auto (default)",
        NoiseSuppression::High => "High",
        NoiseSuppression::Low => "Low",
        NoiseSuppression::Off => "Off",
    }
}

/// What each level does, in a line, as Teams' help says it.
#[must_use]
pub fn description(level: NoiseSuppression) -> &'static str {
    match level {
        NoiseSuppression::Auto => "Best for most situations: the app picks the level of noise suppression.",
        NoiseSuppression::Low => {
            "Suppresses low, steady background noise, such as a computer fan or an air conditioner. Use it to play \
             music for others to hear."
        }
        NoiseSuppression::High => "Suppresses all background sound that isn't speech.",
        NoiseSuppression::Off => {
            "No noise suppression, for a high-fidelity microphone in a quiet place such as a studio."
        }
    }
}

/// Takes the accounts' levels into `levels`: a table of level names under the account's MRI. An account whose
/// level is bad is left out, the others are taken.
pub(super) fn take(levels: &mut BTreeMap<String, NoiseSuppression>, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::NOISE_SUPPRESSION,
        why: "not a table of accounts, each with one of auto, high, low, off",
    };
    let table = value.as_table().ok_or(bad.clone())?;
    let mut failed = false;
    for (account, level) in table {
        let taken = chat_id(&Value::String(account.clone())).zip(level.as_str().and_then(NoiseSuppression::from_key));
        match taken {
            Some((account, level)) if level != NoiseSuppression::default() => {
                levels.insert(account, level);
            }
            Some(_) => {}
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// The accounts' levels into the file's table, when any account chose another than Auto.
pub(super) fn render(levels: &BTreeMap<String, NoiseSuppression>, table: &mut Table) {
    let accounts: Table = levels
        .iter()
        .filter(|(_, level)| **level != NoiseSuppression::default())
        .map(|(account, level)| (account.clone(), Value::String(level.key().to_owned())))
        .collect();
    if !accounts.is_empty() {
        table.insert(keys::NOISE_SUPPRESSION.to_owned(), Value::Table(accounts));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MRI: &str = "8:orgid:00000000-0000-0000-0000-00000000000a";

    #[test]
    fn each_level_has_its_words() {
        for level in NoiseSuppression::ALL {
            assert!(!label(level).is_empty());
            assert!(description(level).ends_with('.'));
        }
    }

    #[test]
    fn a_table_of_levels_goes_through_the_file() {
        let mut levels = BTreeMap::new();
        levels.insert(MRI.to_owned(), NoiseSuppression::High);
        levels.insert("8:orgid:b".to_owned(), NoiseSuppression::Auto);
        let mut table = Table::new();
        render(&levels, &mut table);
        let mut taken = BTreeMap::new();
        take(&mut taken, &table[keys::NOISE_SUPPRESSION]).unwrap();
        assert_eq!(taken, BTreeMap::from([(MRI.to_owned(), NoiseSuppression::High)]));
        let mut none = Table::new();
        render(&BTreeMap::new(), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn a_bad_level_is_left_out_and_reported() {
        let mut taken = BTreeMap::new();
        let value: Value = toml::from_str::<Table>(&format!("\"{MRI}\" = \"low\"\n\"8:orgid:b\" = \"loud\""))
            .unwrap()
            .into();
        assert!(matches!(take(&mut taken, &value), Err(Problem::BadValue { .. })));
        assert_eq!(taken, BTreeMap::from([(MRI.to_owned(), NoiseSuppression::Low)]));
        assert!(take(&mut taken, &Value::Integer(3)).is_err());
    }
}
