//! A reaction's animation on Teams' CDN ([`ReactionAnimation`], a read on [`Service::Emoticons`], no token), a PNG of
//! the animation's square frames stacked top to bottom, in one of two forms:
//!
//! - **The picker's strip** (`docs/protocol/chat.md`, "Reaction pictures"): `GET
//!   https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/<id>/default/50_anim_f.png`,
//!   50×50 frames. The web client reads it from the same path for its picker.
//! - **A call reaction's 3D emoji** (`docs/protocol/calling.md`, "Live reactions"): `GET
//!   https://statics.teams.cdn.office.net/evergreen-assets/skype/v2/<id>V4-3D/<side>a.png`, 102 frames of `<side>` px
//!   (56, 112 or 140), which the web client plays once on the sender's tile.

use std::fmt;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The largest animation kept, in bytes (the 50 px strips seen were 5–60 KB, the 140 px 3D emoji 250–270 KB).
pub const MAX_ANIMATION: usize = 1024 * 1024;
/// The strip read: 50 px frames, the size the web client's picker draws.
const STRIP: &str = "50_anim_f.png";

/// The sides of a call reaction's 3D emoji that Teams' CDN has, px, the smallest first.
pub const EMOJI_3D_SIDES: [u16; 3] = [56, 112, 140];

/// Which picture of a reaction a [`ReactionAnimation`] reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Form {
    /// The picker's 50 px strip.
    Strip,
    /// The call's 3D emoji, frames of this side.
    Emoji3d(u16),
}

/// `GET` of a reaction's animation, by its id (the reaction's key without a skin tone): the picker's strip
/// ([`ReactionAnimation::of`]) or a call reaction's 3D emoji ([`ReactionAnimation::emoji_3d`]). Its answer is read with
/// [`ReactionAnimation::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct ReactionAnimation {
    id: String,
    form: Form,
}

impl ReactionAnimation {
    /// The strip of the reaction `id`; `None` unless it is in the form Teams' reaction ids take (lower-case ASCII
    /// letters, digits and `_`, at most 64 of them), so it is always one path segment of a picture.
    #[must_use]
    pub fn of(id: &str) -> Option<Self> {
        plain(id).then(|| Self {
            id: id.to_owned(),
            form: Form::Strip,
        })
    }

    /// The 3D emoji of the call reaction `id` (`like`, `heart`, `applause` or its emoji's key `clap`, `laugh`,
    /// `surprised`, …) with frames of `side` px, one of [`EMOJI_3D_SIDES`]; `None` for another side or an id
    /// [`ReactionAnimation::of`] refuses. The CDN names Like's emoji `yes`.
    #[must_use]
    pub fn emoji_3d(id: &str, side: u16) -> Option<Self> {
        let id = match id {
            "like" => "yes",
            "applause" => "clap",
            other => other,
        };
        (plain(id) && EMOJI_3D_SIDES.contains(&side)).then(|| Self {
            id: id.to_owned(),
            form: Form::Emoji3d(side),
        })
    }

    /// The animation's bytes in the answer: when it is a PNG of at most [`MAX_ANIMATION`] bytes, else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<Vec<u8>> {
        let png = answer.content_type().is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("image/png"))
        });
        let bytes = answer.body();
        (png && !bytes.is_empty() && bytes.len() <= MAX_ANIMATION).then(|| bytes.to_vec())
    }

    /// A name of this read that no other read has: what an engine keeps its answer by.
    #[must_use]
    pub fn key(&self) -> String {
        match self.form {
            Form::Strip => self.id.clone(),
            Form::Emoji3d(side) => format!("{}V4-3D/{side}", self.id),
        }
    }
}

/// Whether `id` is in the form of Teams' reaction ids.
fn plain(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl fmt::Debug for ReactionAnimation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReactionAnimation")
            .field("id", &self.id)
            .field("form", &self.form)
            .finish()
    }
}

impl Read for ReactionAnimation {
    fn service(&self) -> Service {
        Service::Emoticons
    }

    fn endpoint(&self) -> Endpoint {
        match self.form {
            Form::Strip => Endpoint::new([
                "personal-expressions",
                "v2",
                "assets",
                "emoticons",
                self.id.as_str(),
                "default",
                STRIP,
            ]),
            Form::Emoji3d(side) => Endpoint::new([
                "skype".to_owned(),
                "v2".to_owned(),
                format!("{}V4-3D", self.id),
                format!("{side}a.png"),
            ]),
        }
    }
}
