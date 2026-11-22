//! The reaction catalog and its pictures (`teams_ui::reactions`): every reaction Teams offers by its key (an older
//! name or a code point's, a skin tone appended to one that takes it), its text, its search, and an animation strip cut
//! into the frames GPUI plays. The strips here are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::Cursor;

use image::{ImageFormat, Rgba, RgbaImage};
use teams_ui::reactions::{
    EMOJI_3D_FRAMES, FRAME_PAD, MOST_FOUND, catalog, description, emoji_3d_frames, frame_at, frame_looping, frames,
    glyph, padded_frames,
};

#[test]
fn every_category_teams_offers_is_there_in_its_order() {
    let titles: Vec<&str> = catalog().categories().iter().map(|category| category.title).collect();
    assert_eq!(
        titles,
        [
            "Smileys",
            "Hand gestures",
            "People",
            "Animals and nature",
            "Food",
            "Travel and places",
            "Objects",
            "Activities",
            "Symbols"
        ]
    );
    let count: usize = catalog()
        .categories()
        .iter()
        .map(|category| category.reactions.len())
        .sum();
    assert!(count > 1_500, "{count} reactions");
}

#[test]
fn the_keys_teams_sends_are_offered_and_a_tone_only_on_one_that_takes_it() {
    for key in [
        "like",
        "heart",
        "laugh",
        "surprised",
        "sad",
        "angry",
        "1f440_eyes",
        "cool",
        "fistbump",
        "yes-tone2",
    ] {
        assert!(catalog().offers(key), "{key}");
    }
    for key in ["custom-key", "like-tone2", "yes-tone0", "yes-tone6", "yes-tone", ""] {
        assert!(!catalog().offers(key), "{key}");
    }
    let (reaction, tone) = catalog().get("yes-tone2").unwrap();
    assert_eq!((reaction.id, tone), ("yes", 2));
}

#[test]
fn a_key_as_text_is_its_emoji_in_its_tone_and_any_other_key_itself() {
    assert_eq!(glyph("like"), "👍");
    assert_eq!(glyph("1f440_eyes"), "👀");
    assert_eq!(glyph("yes-tone2"), "👍🏼");
    assert_eq!(glyph("custom-key"), "custom-key");
    assert_eq!(description("1f440_eyes"), "Eyes");
    assert_eq!(description("custom-key"), "custom-key");
}

#[test]
fn a_search_puts_the_reaction_named_so_first_and_holds_every_word() {
    let ids = |query: &str| -> Vec<&str> {
        catalog()
            .search(query)
            .into_iter()
            .map(|index| catalog().at(index).unwrap().id)
            .collect()
    };
    let eyes = ids("eyes");
    assert_eq!(eyes.first(), Some(&"1f440_eyes"));
    assert!(eyes.contains(&"hearteyes"));
    assert!(ids("heart eyes").iter().all(|id| id.contains("heart")));
    assert!(ids("   ").is_empty());
    assert!(
        ids("oreo").is_empty(),
        "a brand sticker without a picture isn't offered"
    );
    assert_eq!(
        ids("happy face").first(),
        Some(&"happyface"),
        "one per picture, the best match"
    );
    assert!(!ids("happy face").contains(&"1f603_grinningfacewithbigeyes"));
    let smileys = catalog().pickable(catalog().categories()[0].reactions.clone());
    assert!(smileys.contains(&catalog().search("grinning face with big eyes")[0]));
    assert!(
        smileys
            .iter()
            .all(|&index| catalog().at(index).unwrap().id != "happyface"),
        "the first of a picture in the category"
    );
    assert!(ids("e").len() <= MOST_FOUND);
}

fn strip(side: u32, frames: u32) -> Vec<u8> {
    let mut image = RgbaImage::new(side, side * frames);
    for (index, row) in (0..side * frames).enumerate() {
        let shade = u8::try_from(index % 256).unwrap();
        for column in 0..side {
            image.put_pixel(column, row, Rgba([shade, 0, 255, 255]));
        }
    }
    let mut png = Vec::new();
    image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
    png
}

#[test]
fn a_3d_emoji_plays_its_102_frames_once_over_4_25_s_and_holds_the_last() {
    use std::time::Duration;
    let at = |ms: u64| frame_at(Duration::from_millis(ms), 102);
    assert_eq!(at(0), 0);
    assert_eq!(at(41), 0);
    assert_eq!(at(42), 1, "24 frames a second");
    assert_eq!(at(1_000), 24);
    assert_eq!(at(2_125), 51, "half way");
    assert_eq!(at(4_208), 100);
    assert_eq!(at(4_250), 101, "the last frame at its end");
    assert_eq!(at(60_000), 101, "never again from the first");
    assert_eq!(frame_at(Duration::from_secs(1), 0), 0, "no frames");
}

#[test]
fn a_3d_emoji_is_decoded_only_as_teams_102_frames_of_its_side() {
    let emoji = emoji_3d_frames(&strip(4, EMOJI_3D_FRAMES), 4).unwrap();
    assert_eq!(emoji.frame_count(), 102);
    assert_eq!(emoji.size(0).width.0, 6, "padded for the stage");
    assert!(emoji_3d_frames(&strip(4, 1), 4).is_none(), "one frame");
    assert!(emoji_3d_frames(&strip(4, EMOJI_3D_FRAMES), 8).is_none(), "another side");
    assert!(
        emoji_3d_frames(&strip(8, EMOJI_3D_FRAMES / 2), 4).is_none(),
        "wider than its side"
    );
    assert!(emoji_3d_frames(b"not a png", 4).is_none());
}

#[test]
fn a_strip_is_cut_into_its_square_frames_in_bgra() {
    let image = frames(&strip(4, 3)).expect("frames");
    assert_eq!(image.frame_count(), 3);
    let first = image.as_bytes(0).unwrap();
    assert_eq!(&first[..4], [255, 0, 0, 255], "blue first: BGRA");
    assert!(frames(&strip(4, 1)[..10]).is_none(), "not a PNG");
    let mut uneven = RgbaImage::new(4, 6);
    uneven.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
    let mut png = Vec::new();
    uneven.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
    assert!(frames(&png).is_none(), "frames that aren't square");
}

#[test]
fn a_strip_loops_at_24_frames_a_second() {
    use std::time::Duration;
    let at = |ms: u64| frame_looping(Duration::from_millis(ms), 48);
    assert_eq!(at(0), 0);
    assert_eq!(at(42), 1);
    assert_eq!(at(1_990), 47);
    assert_eq!(at(2_000), 0, "from the first again after its 48 frames");
    assert_eq!(frame_looping(Duration::from_secs(1), 0), 0, "no frames");
}

/// A frame drawn on a stage repeats its own edge px around it: a sampler that reads past the frame's edge reads them,
/// never the next picture in GPUI's atlas (a line of its colours along the frame's top).
#[test]
fn a_padded_frame_repeats_its_own_edge_around_it() {
    let (side, pad) = (4, FRAME_PAD);
    let image = padded_frames(&strip(side, 3)).expect("frames");
    assert_eq!(image.frame_count(), 3);
    let outer = side + 2 * pad;
    assert_eq!(image.size(1).width.0, i32::try_from(outer).unwrap());
    assert_eq!(image.size(1).height.0, i32::try_from(outer).unwrap());
    let plain = frames(&strip(side, 3)).unwrap();
    let (frame, inner) = (image.as_bytes(1).unwrap(), plain.as_bytes(1).unwrap());
    let at = |bytes: &[u8], width: u32, x: u32, y: u32| {
        let start = usize::try_from((y * width + x) * 4).unwrap();
        bytes[start..start + 4].to_vec()
    };
    for y in 0..outer {
        for x in 0..outer {
            let clamp = |at: u32| at.saturating_sub(pad).min(side - 1);
            assert_eq!(
                at(frame, outer, x, y),
                at(inner, side, clamp(x), clamp(y)),
                "px {x},{y} is its frame's px or the nearest edge px"
            );
        }
    }
    assert!(padded_frames(b"not a png").is_none());
}
