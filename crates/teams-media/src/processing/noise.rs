//! The noise suppression levels of Teams (Auto, High, Low, Off) and the live control a call reads them from.
//!
//! - **Auto** and **High** run DeepFilterNet3, a neural network that keeps speech and takes out the other sounds,
//!   steady or not (keys, a dog, traffic). High lets it take out all it can. Auto limits it to
//!   [`AUTO_LIMIT_DB`], so a little of the room and of music stays, as Teams' Auto is gentler than High.
//! - **Low** runs libwebrtc's classic suppressor: it lowers only steady noise (a fan, an air conditioner).
//! - **Off** suppresses no noise. Echo cancellation and gain control stay.
//!
//! The level is read at each frame, so a change during a call applies to the next 10 ms.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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

    fn code(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::High => 1,
            Self::Low => 2,
            Self::Off => 3,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|level| level.code() == code)
            .unwrap_or_default()
    }
}

/// The level a call's audio processing reads at each frame. Clones share it: the settings set it, the capture thread
/// reads it.
#[derive(Clone, Debug, Default)]
pub struct NoiseControl(Arc<AtomicU8>);

impl NoiseControl {
    #[must_use]
    pub fn new(level: NoiseSuppression) -> Self {
        Self(Arc::new(AtomicU8::new(level.code())))
    }

    pub fn set(&self, level: NoiseSuppression) {
        self.0.store(level.code(), Ordering::Relaxed);
    }

    #[must_use]
    pub fn level(&self) -> NoiseSuppression {
        NoiseSuppression::from_code(self.0.load(Ordering::Relaxed))
    }
}

impl PartialEq for NoiseControl {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for NoiseControl {}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn clones_of_a_control_share_its_level() {
        let control = NoiseControl::new(NoiseSuppression::High);
        let read = control.clone();
        assert_eq!(read.level(), NoiseSuppression::High);
        control.set(NoiseSuppression::Off);
        assert_eq!(read.level(), NoiseSuppression::Off);
        assert_eq!(read, control);
        assert_ne!(NoiseControl::default(), control);
        assert_eq!(NoiseControl::default().level(), NoiseSuppression::Auto);
    }
}
