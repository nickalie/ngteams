---
worth: later
where: crates/ngteams/tests/support/linux/qa_noise_window.rs, crates/teams-media/src/test_signal.rs, crates/teams-media/tests/processing.rs
added: 2026-11-26
---
# no test shows that DeepFilterNet keeps a voice at High and Auto

The QA walk of the noise suppression (`qa_noise_window`, run `noise`) checks the made-up voice at Off and Low only.
DeepFilterNet removes that voice too at High and Auto: it is moving tones (`test_signal::voice`), which are no speech
to a model trained on speech. In the walk the voice at High reads 0.0000 and at Auto about 20 dB under Off. So the walk
cannot tell "noise lowered" from "microphone silenced" at those levels. teams-media's processing tests check the voice
only for the classic suppressor (Low) too.

Suggested next step: make a synthetic voice that DeepFilterNet keeps (for example a pulse train at a moving pitch
through two or three formant filters, with syllables and pauses), check in `crates/teams-media/tests/processing.rs`
that High and Auto keep it within a few dB, then use it in the walk and require the voice at every level within a few
dB of Off. Read the noise where that voice has no energy, or from its pauses.
