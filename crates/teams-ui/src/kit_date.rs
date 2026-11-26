//! The UI kit's dates at its boundary. The kit's date pickers take and give `jiff::civil` dates, and the app keeps its
//! dates in Chrono. A day outside the range of the other crate has no counterpart, thus each conversion can give none.

use chrono::{Datelike as _, NaiveDate};
use jiff::civil;

/// The kit's day for `day`; `None` for a year outside the kit's range (-9999 to 9999).
#[must_use]
pub fn to_kit(day: NaiveDate) -> Option<civil::Date> {
    let year = i16::try_from(day.year()).ok()?;
    let month = i8::try_from(day.month()).ok()?;
    let date = i8::try_from(day.day()).ok()?;
    civil::Date::new(year, month, date).ok()
}

/// The app's day for the kit's `day`.
#[must_use]
pub fn from_kit(day: civil::Date) -> Option<NaiveDate> {
    let month = u32::try_from(day.month()).ok()?;
    let date = u32::try_from(day.day()).ok()?;
    NaiveDate::from_ymd_opt(i32::from(day.year()), month, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_goes_to_the_kit_and_back() {
        let day = NaiveDate::from_ymd_opt(2026, 2, 28);
        let kit = day.and_then(to_kit);
        assert_eq!(kit, civil::Date::new(2026, 2, 28).ok());
        assert_eq!(kit.and_then(from_kit), day);
    }

    #[test]
    fn a_leap_day_keeps_its_date() {
        let day = NaiveDate::from_ymd_opt(2028, 2, 29);
        assert_eq!(day.and_then(to_kit).and_then(from_kit), day);
    }

    #[test]
    fn a_year_past_the_kits_range_has_no_kit_day() {
        assert_eq!(NaiveDate::from_ymd_opt(10_000, 1, 1).and_then(to_kit), None);
        assert_eq!(NaiveDate::from_ymd_opt(-10_000, 1, 1).and_then(to_kit), None);
    }

    #[test]
    fn the_kits_first_and_last_days_reach_the_app() {
        assert_eq!(from_kit(civil::Date::MIN), NaiveDate::from_ymd_opt(-9999, 1, 1));
        assert_eq!(from_kit(civil::Date::MAX), NaiveDate::from_ymd_opt(9999, 12, 31));
    }
}
