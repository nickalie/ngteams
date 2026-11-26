//! The noise suppression of a build without the model (the `effects` feature off): the model never loads, so at Auto
//! and High the frames pass unchanged, as before the model loads.

use crate::devices::AudioFrame;

/// The model never loads here.
pub fn prepare() {}

/// A state of the model, which a build without the model cannot make.
#[derive(Debug)]
pub enum DeepFilter {}

impl DeepFilter {
    /// Always `None`: there is no model.
    #[must_use]
    pub fn ready(_limit: f32) -> Option<Self> {
        None
    }

    pub fn set_limit(&mut self, _limit: f32) {
        match *self {}
    }

    /// # Errors
    /// Never: no value of it exists.
    pub fn process(&mut self, _frame: &mut AudioFrame) -> Result<(), String> {
        match *self {}
    }
}
