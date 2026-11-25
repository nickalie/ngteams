use cosmic_text::Style;
use image::{Rgba, RgbaImage};

use std::sync::Mutex;

use super::{
    BOTTOM_HINT, Caption, Fitted, Fonts, Region, TOP_HINT, Words, breaks, decode, draw, fit, png, render, shown, split,
};

fn block(region: [u32; 4], words: &str) -> Caption {
    Caption {
        template: "Made_Up_Template.png".to_owned(),
        family: "Comic Sans MS".to_owned(),
        color: [0xcb, 0x46, 0x53],
        border: None,
        words: Words::Block {
            region,
            words: words.to_owned(),
        },
    }
}

fn meme() -> Caption {
    Caption {
        template: "Made-Up-Meme.png".to_owned(),
        family: "Impact".to_owned(),
        color: [255, 255, 255],
        border: Some([0, 0, 0]),
        words: Words::Meme,
    }
}

fn typed(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| (*text).to_owned()).collect()
}

/// A made-up measure: every character is half the size wide.
#[expect(clippy::cast_precision_loss, reason = "a few characters")]
fn half(text: &str) -> f32 {
    text.chars().count() as f32 * 50.0
}

fn region(left: f32, top: f32, right: f32, bottom: f32) -> Region {
    Region {
        left,
        top,
        right,
        bottom,
    }
}

/// A grey template of `width` by `height`, as a PNG.
fn template(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba([128, 128, 128, 255]))
}

fn pixels_of(image: &RgbaImage, color: [u8; 3]) -> usize {
    image
        .pixels()
        .filter(|pixel| pixel[0] == color[0] && pixel[1] == color[1] && pixel[2] == color[2])
        .count()
}

#[test]
fn a_caption_line_parses_in_both_forms() {
    let fields = [
        "OfficeDrama_HighFive_Template.png",
        "Comic Sans MS",
        "#000000",
        "-",
        "30 170 308 223",
        "HIGH FIVE!",
    ];
    let caption = Caption::parse(&fields).unwrap();
    assert_eq!(caption.template, "OfficeDrama_HighFive_Template.png");
    assert_eq!(caption.color, [0, 0, 0]);
    assert_eq!(caption.border, None);
    assert_eq!(
        caption.words,
        Words::Block {
            region: [30, 170, 308, 223],
            words: "HIGH FIVE!".to_owned()
        }
    );
    assert!(!caption.is_meme());
    assert_eq!(caption.first_text(), typed(&["HIGH FIVE!"]));

    let meme = Caption::parse(&["Memes-25.png", "Impact", "#FFFFFF", "black", "-", "-"]).unwrap();
    assert!(meme.is_meme());
    assert_eq!(meme.color, [255, 255, 255]);
    assert_eq!(meme.border, Some([0, 0, 0]));
    assert_eq!(meme.first_text(), typed(&["", ""]));
    assert_eq!(
        Caption::parse(&["a.png", "Impact", "white", "#417b84", "-", "-"])
            .unwrap()
            .border,
        Some([0x41, 0x7b, 0x84])
    );
}

#[test]
fn a_caption_line_out_of_form_is_refused() {
    let good = ["a.png", "Impact", "#000000", "-", "1 2 3 4", "Hi"];
    assert!(Caption::parse(&good).is_some());
    assert!(Caption::parse(&good[..5]).is_none());
    for (at, bad) in [
        (2, "#00000"),
        (2, "red"),
        (2, "#zzzzzz"),
        (3, "grey"),
        (4, "1 2 3"),
        (4, "1 2 x 4"),
        (4, "5 2 3 4"),
        (4, "1 4 3 4"),
        (4, "-"),
    ] {
        let mut fields = good;
        fields[at] = bad;
        assert!(Caption::parse(&fields).is_none(), "{fields:?}");
    }
    assert!(Caption::parse(&["a.png", "Impact", "#000000", "-", "1 2 3 4", "-"]).is_none());
}

#[test]
fn a_block_draws_the_trimmed_words_in_its_region() {
    let caption = block([10, 20, 110, 70], "Hi");
    assert_eq!(
        caption.texts(&typed(&["  So cool  "]), (200, 100)),
        vec![("So cool".to_owned(), region(10.0, 20.0, 110.0, 70.0))]
    );
    assert!(caption.texts(&typed(&["   "]), (200, 100)).is_empty());
    assert!(caption.texts(&[], (200, 100)).is_empty());
    // a line break or a run of spaces is one space, as Teams' canvas draws it
    assert_eq!(
        caption.texts(&typed(&["THAT'S\nSO  COOL\r\n"]), (200, 100))[0].0,
        "THAT'S SO COOL"
    );
}

#[test]
fn a_meme_draws_its_lines_in_upper_case_in_its_bands() {
    let caption = meme();
    assert_eq!(
        caption.texts(&typed(&["top words", "bottom"]), (250, 200)),
        vec![
            ("TOP WORDS".to_owned(), region(20.0, 20.0, 230.0, 70.0)),
            ("BOTTOM".to_owned(), region(20.0, 130.0, 230.0, 180.0)),
        ]
    );
    assert_eq!(
        caption.texts(&typed(&["", "only below"]), (250, 200)),
        vec![("ONLY BELOW".to_owned(), region(20.0, 130.0, 230.0, 180.0))]
    );
    let hints = caption.texts(&typed(&[" ", ""]), (250, 200));
    assert_eq!(hints[0].0, TOP_HINT.to_uppercase());
    assert_eq!(hints[1].0, BOTTOM_HINT.to_uppercase());
}

#[test]
fn the_label_is_what_was_typed_after_a_memes_name() {
    assert_eq!(
        block([0, 0, 9, 9], "x").label("High five", &typed(&[" Go team "])),
        "Go team"
    );
    assert_eq!(block([0, 0, 9, 9], "x").label("High five", &typed(&[""])), "");
    assert_eq!(meme().label("Doge", &typed(&["such", "wow"])), "Doge such, wow");
    assert_eq!(meme().label("Doge", &typed(&["", "wow"])), "Doge wow");
    assert_eq!(meme().label("Doge", &typed(&["", ""])), "Doge");
    assert_eq!(meme().label("Doge", &typed(&["such\nwow", "\n"])), "Doge such wow");
    assert_eq!(
        meme().texts(&typed(&["such\n", "\n"]), (250, 200)),
        vec![("SUCH".to_owned(), region(20.0, 20.0, 230.0, 70.0))]
    );
}

#[test]
fn breaks_take_the_nearest_space_to_each_even_split() {
    let chars: Vec<char> = "aaaa bbbb cccc".chars().collect();
    assert_eq!(breaks(&chars, 1), Vec::<usize>::new());
    assert_eq!(breaks(&chars, 3), vec![4, 9]);
    assert_eq!(split(&chars, &[4, 9]), vec!["aaaa", "bbbb", "cccc"]);
    // a tie goes to the space before
    let tie: Vec<char> = "ab cde fg".chars().collect();
    assert_eq!(breaks(&tie, 2), vec![2]);
    // no space after the split: the one before
    let late: Vec<char> = "abc defghij".chars().collect();
    assert_eq!(breaks(&late, 2), vec![3]);
    // no space before it and the one after is near
    let early: Vec<char> = "abcdefg hi".chars().collect();
    assert_eq!(breaks(&early, 2), vec![7]);
    // no space at all: one line
    let word: Vec<char> = "abcdefghij".chars().collect();
    assert_eq!(breaks(&word, 3), Vec::<usize>::new());
    // two splits nearest the same space give one break
    let one: Vec<char> = "abcdefgh ij".chars().collect();
    assert_eq!(breaks(&one, 4), vec![8]);
}

#[test]
fn a_short_caption_in_a_tall_region_fills_its_width() {
    let Fitted { size, lines } = fit("Hi", region(10.0, 10.0, 210.0, 410.0), &mut half).unwrap();
    assert_eq!(lines.len(), 1);
    // two characters 100 px wide at the base size fill 200 px
    assert!((size - 200.0).abs() < 0.01, "{size}");
    assert_eq!(lines[0].text, "Hi");
    assert!((lines[0].x - 10.0).abs() < 0.01);
}

#[test]
fn a_caption_shrinks_to_the_regions_height_and_is_centred() {
    let Fitted { size, lines } = fit("Hi", region(0.0, 0.0, 400.0, 50.0), &mut half).unwrap();
    assert_eq!(lines.len(), 1);
    assert!((size - 50.0).abs() < 0.01, "{size}");
    // 50 px wide in 400: 175 px in from the left, the baseline at the bottom
    assert!((lines[0].x - 175.0).abs() < 0.01);
    assert!((lines[0].baseline - 50.0).abs() < 0.01);
}

#[test]
#[expect(clippy::cast_precision_loss, reason = "a few lines")]
fn a_long_caption_wraps_to_the_regions_shape() {
    let text = "never enough coffee in this office today";
    let tall = fit(text, region(0.0, 0.0, 100.0, 200.0), &mut half).unwrap();
    let wide = fit(text, region(0.0, 0.0, 600.0, 30.0), &mut half).unwrap();
    assert!(tall.lines.len() > wide.lines.len(), "{tall:?} {wide:?}");
    assert_eq!(wide.lines.len(), 1);
    assert_eq!(
        tall.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        text
    );
    for pair in tall.lines.windows(2) {
        assert!((pair[1].baseline - pair[0].baseline - (tall.size + 2.0)).abs() < 0.01);
    }
    // the widest line fills the width or the block fills the height
    let widest = tall.lines.iter().map(|line| half(&line.text)).fold(0.0, f32::max) * tall.size / 100.0;
    let height = tall.lines.len() as f32 * tall.size + (tall.lines.len() - 1) as f32 * 2.0;
    assert!(widest <= 100.01 && height <= 200.01, "{widest} {height}");
    assert!((widest - 100.0).abs() < 0.01 || (height - 200.0).abs() < 0.01);
    // the block, its line spacing too, is centred in the height
    let top = tall.lines[0].baseline - tall.size;
    let bottom = tall.lines[tall.lines.len() - 1].baseline;
    assert!((top - (200.0 - bottom)).abs() < 0.01, "{top} {bottom}");
    // every line is centred
    for line in &tall.lines {
        let width = half(&line.text) * tall.size / 100.0;
        assert!((line.x + width / 2.0 - 50.0).abs() < 0.01);
    }
}

#[test]
fn nothing_with_a_width_fits_nothing() {
    assert!(fit("", region(0.0, 0.0, 100.0, 100.0), &mut half).is_none());
    assert!(fit("Hi", region(0.0, 0.0, 0.0, 100.0), &mut half).is_none());
    assert!(fit("Hi", region(0.0, 0.0, 100.0, 100.0), &mut |_| 0.0).is_none());
}

#[test]
fn a_font_the_system_lacks_has_a_free_stand_in() {
    let fonts = Fonts::bundled();
    assert_eq!(fonts.face("Impact"), (Some("Anton".to_owned()), Style::Normal));
    assert_eq!(
        fonts.face("Comic Sans MS"),
        (Some("Comic Neue".to_owned()), Style::Normal)
    );
    assert_eq!(fonts.face("comic neue"), (Some("comic neue".to_owned()), Style::Normal));
    assert_eq!(fonts.face("Cochin Italic"), (None, Style::Italic));
    assert_eq!(fonts.face("Anton Italic"), (Some("Anton".to_owned()), Style::Italic));
    assert_eq!(fonts.face("Arial"), (None, Style::Normal));
}

#[test]
fn a_block_is_drawn_in_its_colour_inside_its_region() {
    let fonts = Mutex::new(Fonts::bundled());
    let caption = block([20, 30, 180, 90], "x");
    let image = draw(&template(200, 120), &caption, &typed(&["HIGH FIVE!"]), &fonts);
    assert_eq!(image.dimensions(), (200, 120));
    let mut inside = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0 == [128, 128, 128, 255] {
            continue;
        }
        // a letter's soft edge may reach a pixel past the region
        assert!((18..=182).contains(&x) && (28..=92).contains(&y), "{x},{y}");
        inside += 1;
    }
    assert!(inside > 200, "{inside}");
    assert!(pixels_of(&image, caption.color) > 50);

    let empty = draw(&template(200, 120), &caption, &typed(&[""]), &fonts);
    assert!(empty.pixels().all(|pixel| pixel.0 == [128, 128, 128, 255]));
}

#[test]
fn a_memes_lines_are_white_with_a_black_edge() {
    let fonts = Mutex::new(Fonts::bundled());
    let image = draw(&template(250, 250), &meme(), &typed(&["such wow", ""]), &fonts);
    assert!(pixels_of(&image, [255, 255, 255]) > 50);
    assert!(pixels_of(&image, [0, 0, 0]) > 50);
    // the bottom band is empty: only the top line is drawn
    for (_, y, pixel) in image.enumerate_pixels() {
        if y > 100 {
            assert_eq!(pixel.0, [128, 128, 128, 255]);
        }
    }
}

#[test]
fn a_caption_on_a_clear_template_makes_its_letters_solid() {
    let fonts = Mutex::new(Fonts::bundled());
    let clear = RgbaImage::new(100, 60);
    let image = draw(&clear, &block([0, 0, 100, 60], "x"), &typed(&["OK"]), &fonts);
    assert!(image.pixels().any(|pixel| pixel.0 == [0xcb, 0x46, 0x53, 255]));
}

#[test]
fn a_template_that_is_no_picture_decodes_to_nothing() {
    assert!(decode(b"not a picture").is_none());
    let picture = png(&template(30, 20)).unwrap();
    assert_eq!(decode(&picture.bytes).unwrap().dimensions(), (30, 20));
}

#[test]
fn render_gives_a_png_of_the_templates_size() {
    let picture = render(&template(120, 90), &meme(), &typed(&["top", "bottom"])).unwrap();
    assert_eq!(picture.format, teams_proto::PastedFormat::Png);
    let image = image::load_from_memory(&picture.bytes).unwrap();
    assert_eq!((image.width(), image.height()), (120, 90));
}

#[test]
fn a_captioned_sticker_is_shown_180_high() {
    assert_eq!(shown((334, 250)), (240, 180));
    assert_eq!(shown((250, 250)), (180, 180));
    assert_eq!(shown((0, 0)), (180, 180));
}
