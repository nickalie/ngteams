//! The height of each account's live captions area in a call, as the user drags or keys it. The file keeps a table
//! of heights in px under the account's MRI, only for the accounts that changed it from the default.

use std::collections::BTreeMap;

use toml::{Table, Value};

use super::{Problem, chat_id, keys};

/// The height of a captions area the user did not resize, px.
pub const DEFAULT: u32 = 244;
/// The lowest height the user can set, px: the header and one caption line.
pub const MIN: u32 = 100;
/// The highest height the user can set, px; a short window still keeps the stage's room.
pub const MAX: u32 = 1200;

/// `height` inside [`MIN`] and [`MAX`].
#[must_use]
pub fn clamp(height: u32) -> u32 {
    height.clamp(MIN, MAX)
}

/// Takes the accounts' heights into `heights`: a table of numbers under the account's MRI. An account whose height
/// is bad is left out, the others are taken.
pub(super) fn take(heights: &mut BTreeMap<String, u32>, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::CAPTION_HEIGHT,
        why: "not a table of accounts, each with a height in px",
    };
    let table = value.as_table().ok_or(bad.clone())?;
    let mut failed = false;
    for (account, height) in table {
        let height = height.as_integer().and_then(|height| u32::try_from(height).ok());
        match chat_id(&Value::String(account.clone())).zip(height) {
            Some((account, height)) if clamp(height) != DEFAULT => {
                heights.insert(account, clamp(height));
            }
            Some(_) => {}
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// The accounts' heights into the file's table, when any account changed its height.
pub(super) fn render(heights: &BTreeMap<String, u32>, table: &mut Table) {
    let accounts: Table = heights
        .iter()
        .filter(|(_, height)| **height != DEFAULT)
        .map(|(account, height)| (account.clone(), Value::Integer(i64::from(*height))))
        .collect();
    if !accounts.is_empty() {
        table.insert(keys::CAPTION_HEIGHT.to_owned(), Value::Table(accounts));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MRI: &str = "8:orgid:00000000-0000-0000-0000-00000000000a";

    #[test]
    fn a_table_of_heights_goes_through_the_file() {
        let mut heights = BTreeMap::new();
        heights.insert(MRI.to_owned(), 420);
        heights.insert("8:orgid:b".to_owned(), DEFAULT);
        let mut table = Table::new();
        render(&heights, &mut table);
        let mut taken = BTreeMap::new();
        take(&mut taken, &table[keys::CAPTION_HEIGHT]).unwrap();
        assert_eq!(taken, BTreeMap::from([(MRI.to_owned(), 420)]));
        let mut none = Table::new();
        render(&BTreeMap::new(), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn a_height_out_of_range_is_clamped_and_a_bad_one_left_out() {
        let mut taken = BTreeMap::new();
        let value: Value = toml::from_str::<Table>(&format!(
            "\"{MRI}\" = 5\n\"8:orgid:b\" = 99999\n\"8:orgid:c\" = \"tall\"\n\"8:orgid:d\" = -3"
        ))
        .unwrap()
        .into();
        assert!(matches!(take(&mut taken, &value), Err(Problem::BadValue { .. })));
        assert_eq!(
            taken,
            BTreeMap::from([(MRI.to_owned(), MIN), ("8:orgid:b".to_owned(), MAX)])
        );
        assert!(take(&mut taken, &Value::Integer(3)).is_err());
    }
}
