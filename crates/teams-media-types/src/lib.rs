//! The plain types of a call's media that the settings keep. `teams-media` re-exports them at their places there
//! (`devices::catalog::DeviceKind`, `processing::NoiseSuppression`), and `teams-app`'s settings read and write them,
//! so a build without the media (the phone's chats) keeps the same settings file.

/// What kind of device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceKind {
    Microphone,
    Speaker,
    Camera,
}

impl DeviceKind {
    pub const ALL: [Self; 3] = [Self::Microphone, Self::Speaker, Self::Camera];

    /// Its name, as a settings file or a probe's output gives it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Speaker => "speaker",
            Self::Camera => "camera",
        }
    }

    /// Its place in [`DeviceKind::ALL`]: the slot of a table that keeps one value of each kind.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::Microphone => 0,
            Self::Speaker => 1,
            Self::Camera => 2,
        }
    }
}

/// The most Auto lets DeepFilterNet take out, in dB: it keeps about 6% of the noise's amplitude.
pub const AUTO_LIMIT_DB: f32 = 24.0;
/// The attenuation limit at which DeepFilterNet has no limit.
pub const NO_LIMIT_DB: f32 = 100.0;

/// How much of the microphone's background noise is taken out, as Teams names the levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NoiseSuppression {
    #[default]
    Auto,
    High,
    Low,
    Off,
}

impl NoiseSuppression {
    /// In the order Teams lists them, Off (Teams' switch turned off) last.
    pub const ALL: [Self; 4] = [Self::Auto, Self::Low, Self::High, Self::Off];

    /// The level's name in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::High => "high",
            Self::Low => "low",
            Self::Off => "off",
        }
    }

    /// The level named `key` in the settings file.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.key() == key)
    }

    /// DeepFilterNet's attenuation limit at this level, in dB; `None` where it does not run.
    #[must_use]
    pub fn deep_filter_limit(self) -> Option<f32> {
        match self {
            Self::Auto => Some(AUTO_LIMIT_DB),
            Self::High => Some(NO_LIMIT_DB),
            Self::Low | Self::Off => None,
        }
    }

    /// Whether libwebrtc's classic suppressor runs at this level.
    #[must_use]
    pub fn classic(self) -> bool {
        self == Self::Low
    }

    /// Its code in one byte, as a live control keeps it.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::High => 1,
            Self::Low => 2,
            Self::Off => 3,
        }
    }

    /// The level of `code`; Auto for a code that names none.
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|level| level.code() == code)
            .unwrap_or_default()
    }
}

/// The keys of the backgrounds the app ships, in the order the picker shows them: the settings file names a stock
/// background by one of them (`teams-media`'s `video::effects::STOCK` holds the pictures).
pub const STOCK_BACKGROUNDS: [&str; 6] = ["office", "mountains", "city-lights", "waves", "seaside", "forest"];

/// The stock background's key `key`, when the app ships it.
#[must_use]
pub fn stock_background(key: &str) -> Option<&'static str> {
    STOCK_BACKGROUNDS.into_iter().find(|stock| *stock == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_its_key_and_its_slot() {
        let mapped: Vec<_> = DeviceKind::ALL
            .into_iter()
            .map(|kind| (kind.key(), kind.index()))
            .collect();
        assert_eq!(mapped, [("microphone", 0), ("speaker", 1), ("camera", 2)]);
    }

    #[test]
    fn each_level_maps_to_its_suppressor() {
        let mapped: Vec<_> = NoiseSuppression::ALL
            .into_iter()
            .map(|level| (level, level.deep_filter_limit(), level.classic()))
            .collect();
        assert_eq!(
            mapped,
            [
                (NoiseSuppression::Auto, Some(AUTO_LIMIT_DB), false),
                (NoiseSuppression::Low, None, true),
                (NoiseSuppression::High, Some(NO_LIMIT_DB), false),
                (NoiseSuppression::Off, None, false),
            ]
        );
        const { assert!(AUTO_LIMIT_DB < NO_LIMIT_DB) };
        assert_eq!(NoiseSuppression::default(), NoiseSuppression::Auto);
    }

    #[test]
    fn a_level_goes_through_its_key_and_its_code() {
        for level in NoiseSuppression::ALL {
            assert_eq!(NoiseSuppression::from_key(level.key()), Some(level));
            assert_eq!(NoiseSuppression::from_code(level.code()), level);
        }
        assert_eq!(NoiseSuppression::from_key("medium"), None);
        assert_eq!(NoiseSuppression::from_code(200), NoiseSuppression::Auto);
    }

    #[test]
    fn a_stock_background_is_found_by_its_key() {
        assert_eq!(stock_background("waves"), Some("waves"));
        assert_eq!(stock_background("beach"), None);
    }
}
