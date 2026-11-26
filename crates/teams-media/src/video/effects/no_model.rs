//! The segmenter of a build without the model (the `effects` feature off): it never makes a mask, so an effect that
//! the user chose takes the whole frame as the background, as when the model cannot load.

use crate::devices::VideoFrame;

/// The model's input width, as the masks give it.
pub const MODEL_WIDTH: usize = 256;
/// The model's input height, as the masks give it.
pub const MODEL_HEIGHT: usize = 144;

/// A segmenter without a model: every mask is all background.
#[derive(Default)]
pub(super) struct Segmenter {
    mask: Vec<u8>,
}

impl Segmenter {
    pub(super) fn forget(&mut self) {
        self.mask = Vec::new();
    }

    pub(super) fn mask(&mut self, _frame: &VideoFrame) -> &[u8] {
        self.mask.resize(MODEL_WIDTH * MODEL_HEIGHT, 0);
        &self.mask
    }
}
