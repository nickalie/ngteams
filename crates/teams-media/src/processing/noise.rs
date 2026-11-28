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

pub use teams_media_types::{AUTO_LIMIT_DB, NO_LIMIT_DB, NoiseSuppression};

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
