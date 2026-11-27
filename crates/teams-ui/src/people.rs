//! People as the views show them (M2 Task 12): a name and a picture, both from the engine
//! ([`Core::person`](crate::Core::person): their profile's name (Graph's, else the middle tier's profile lookup's),
//! else their newest message's, else one the binary knew) and their picture's bytes, decoded by GPUI only once a view
//! draws them. Without a picture an avatar shows the initials of a name; without a name, a blank one.
//!
//! A picture reaches a view only as bytes from the engine ([`Shown::of`]): nothing here names a URL, and GPUI's HTTP
//! client stays the null one. A [`Picture`]'s `Debug` shows its format and size, never its bytes.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use gpui::{AnyElement, Image, ImageFormat, IntoElement as _, Pixels, SharedString, Styled as _, px};
use gpui_component::avatar::Avatar;
use gpui_component::{Sizable as _, Size};
use teams_core::{NameFrom, Person};
use teams_store::StoredPicture;

use crate::icons::Glyph;

mod initials;

pub use initials::{first_initial, initials, team_initials};

/// What an avatar a view draws itself holds when it has no picture: `initials`, else (a name in a script Teams draws
/// no initials for, a phone number) the blank face the kit's avatar shows, `size` across.
#[must_use]
pub fn initials_or_face(initials: String, size: Pixels) -> AnyElement {
    if initials.is_empty() {
        Glyph::USER.icon().size(size).into_any_element()
    } else {
        initials.into_any_element()
    }
}

/// Someone as a view shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The name to show; `None` when nothing gave one (a view shows the MRI).
    pub name: Option<SharedString>,
    /// Where the name came from.
    pub from: Option<NameFrom>,
    /// Their picture, from the engine's bytes; `None` when they have none (or it wasn't read yet).
    pub picture: Option<Picture>,
}

impl Shown {
    /// `person` as a view shows them; a picture of a format GPUI can't be handed is none.
    #[must_use]
    pub fn of(person: Person) -> Self {
        Self {
            name: person.name.map(SharedString::from),
            from: person.from,
            picture: person.picture.as_ref().and_then(image_of),
        }
    }

    /// Whether the name came from a profile (Graph's or the lookup's), which a message's own name doesn't override.
    #[must_use]
    pub fn profiled(&self) -> bool {
        matches!(self.from, Some(NameFrom::Graph | NameFrom::ShortProfile))
    }
}

/// What an avatar beside a name shows: a picture, else the initials of the name when it is one (`named`: not an
/// MRI), else nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Face {
    pub named: bool,
    pub picture: Option<Picture>,
}

impl Face {
    /// The avatar of `size` beside `name`.
    #[must_use]
    pub fn avatar(&self, name: &SharedString, size: Size) -> Avatar {
        avatar(self.named.then_some(name), self.picture.as_ref(), size)
    }
}

/// A picture as a view holds it, for GPUI to decode when it is drawn. Its `Debug` shows only its format and size, as
/// the engine's [`StoredPicture`] does: GPUI's own [`Image`] would print every byte.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture(Arc<Image>);

impl Picture {
    /// The image GPUI is handed.
    #[must_use]
    pub fn image(&self) -> &Arc<Image> {
        &self.0
    }

    /// The picture of PNG `bytes`.
    #[must_use]
    pub fn png(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
    }
}

impl Deref for Picture {
    type Target = Image;

    fn deref(&self) -> &Image {
        &self.0
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("format", &self.0.format)
            .field("bytes", &self.0.bytes.len())
            .finish()
    }
}

/// The picture of a stored one: JPEG and PNG, the formats the engine keeps.
#[must_use]
pub fn image_of(picture: &StoredPicture) -> Option<Picture> {
    let format = match picture.media_type.as_str() {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        _ => return None,
    };
    Some(Picture(Arc::new(Image::from_bytes(format, picture.bytes.clone()))))
}

/// A picture given as bytes alone, told by its first bytes: PNG or JPEG.
#[must_use]
pub fn sniffed_image(bytes: &[u8]) -> Option<Picture> {
    let format = crate::team_app_dialogs::picture_format(bytes)?;
    Some(Picture(Arc::new(Image::from_bytes(format, bytes.to_vec()))))
}

/// The picture of an app's tile as the engine read it: PNG, JPEG and WebP.
#[must_use]
pub fn app_image(picture: &teams_proto::PastedPicture) -> Option<Picture> {
    let format = match picture.format {
        teams_proto::PastedFormat::Png => ImageFormat::Png,
        teams_proto::PastedFormat::Jpeg => ImageFormat::Jpeg,
        teams_proto::PastedFormat::Webp => ImageFormat::Webp,
        teams_proto::PastedFormat::Gif => return None,
    };
    Some(Picture(Arc::new(Image::from_bytes(format, picture.bytes.clone()))))
}

/// An avatar of `size`: the picture when there is one, else the initials of `name`, else a blank one.
#[must_use]
pub fn avatar(name: Option<&SharedString>, picture: Option<&Picture>, size: Size) -> Avatar {
    let mut avatar = Avatar::new().with_size(size);
    if let Some(shown) = name.map(|name| initials(name)).filter(|shown| !shown.is_empty()) {
        avatar = avatar.name(initials::kit_name(&shown));
        if let Size::Size(side) = size {
            avatar = custom_initials(avatar, f32::from(side));
        }
    }
    if let Some(picture) = picture {
        avatar = avatar.src(Arc::clone(picture.image()));
    }
    avatar
}

/// The initials of an avatar `side` px across, in the middle of its circle at Teams' size. The kit gives custom-size
/// initials a box of half the side and no text size of their own: they took the size of the text round them, and a
/// small face drew them under its middle, cut by the circle.
fn custom_initials(avatar: Avatar, side: f32) -> Avatar {
    avatar
        .text_size(px(initials_size(side)))
        .line_height(px(side * 0.5))
        .text_center()
        .whitespace_nowrap()
}

/// The size of the initials on a face `side` px across, as Teams' (Fluent's) avatar sizes them, px.
fn initials_size(side: f32) -> f32 {
    match side {
        ..=24.0 => 10.0,
        ..=28.0 => 12.0,
        ..=40.0 => 14.0,
        ..=56.0 => 16.0,
        ..=96.0 => 20.0,
        _ => 24.0,
    }
}

#[cfg(test)]
mod tests {
    use super::initials_size;

    #[test]
    fn custom_initials_take_fluent_s_text_size_for_the_side() {
        let sizes = [
            (16.0, 10.0),
            (20.0, 10.0),
            (24.0, 10.0),
            (28.0, 12.0),
            (32.0, 14.0),
            (40.0, 14.0),
            (48.0, 16.0),
            (56.0, 16.0),
            (64.0, 20.0),
            (96.0, 20.0),
            (120.0, 24.0),
        ];
        for (side, text) in sizes {
            assert!((initials_size(side) - text).abs() < f32::EPSILON, "a face of {side} px");
        }
    }
}
