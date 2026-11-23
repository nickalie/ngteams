//! The sticker store and the import of a folder, a `.zip` or chosen files of pictures and Telegram stickers, and of a
//! Telegram pack's link against the mock Bot API (`teams_ui::stickers`): every picture is made here, in the test; the
//! one file read from the repo is the synthetic, marked Lottie animation of `tests/fixtures/telegram/`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::{Cursor, Write as _};
use std::path::Path;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, ImageFormat, Rgba, RgbaImage};
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_mock::telegram::{BotApiMock, MockKind, MockSet, MockSticker, SYNTHETIC_BOT_TOKEN};
use teams_ui::stickers::import::{
    self, Archive, Files, Folder, ImportError, ImportKind, KEPT_SIDE, MAX_FILE, MAX_FRAMES, MAX_TOTAL, Progress,
    SHOWN_SIDE, Skip, Source,
};
use teams_ui::stickers::store::{self, MAX_STICKERS, StickerRef};
use teams_ui::stickers::telegram::{self, BotApis, BotToken};
use zip::write::{SimpleFileOptions, ZipWriter};

/// A made-up still picture, `width` by `height`, in `format`: a square of `colour` on a clear ground.
fn still(width: u32, height: u32, colour: [u8; 3], format: ImageFormat) -> Vec<u8> {
    let picture = RgbaImage::from_fn(width, height, |x, y| {
        let inside = x > width / 4 && x < width * 3 / 4 && y > height / 4 && y < height * 3 / 4;
        if inside {
            Rgba([colour[0], colour[1], colour[2], 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    });
    let mut bytes = Vec::new();
    if format == ImageFormat::Jpeg {
        image::DynamicImage::ImageRgba8(picture)
            .into_rgb8()
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
    } else {
        picture.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
    }
    bytes
}

/// A made-up animated GIF of `frames` frames, `side` square, each `ms` long.
fn animated_gif(side: u32, frames: usize, ms: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut bytes, 30);
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for index in 0..frames {
            #[expect(clippy::cast_possible_truncation, reason = "a few frames")]
            let shade = (index * 40 % 256) as u8;
            let frame = RgbaImage::from_fn(side, side, |x, _| {
                if x < side / 2 {
                    Rgba([shade, 10, 200, 255])
                } else {
                    Rgba([0, 0, 0, 0])
                }
            });
            encoder
                .encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1)))
                .unwrap();
        }
    }
    bytes
}

/// CRC-32 of a PNG chunk's type and data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    let mut typed = kind.to_vec();
    typed.extend_from_slice(data);
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
}

/// The chunks of a PNG file, as (type, data).
fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let mut at = 8;
    let mut found = Vec::new();
    while at + 8 <= png.len() {
        let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
        found.push((kind, png[at + 8..at + 8 + length].to_vec()));
        at += 12 + length;
    }
    found
}

/// A made-up animated PNG (APNG) of `frames` frames, `side` square, a tenth of a second each.
fn animated_png(side: u32, frames: u32) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = (0..frames)
        .map(|index| {
            #[expect(clippy::cast_possible_truncation, reason = "a few frames")]
            still(side, side, [(index * 60) as u8, 200, 30], ImageFormat::Png)
        })
        .collect();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let first = chunks(&encoded[0]);
    chunk(&mut out, *b"IHDR", &first[0].1);
    let mut actl = frames.to_be_bytes().to_vec();
    actl.extend_from_slice(&0_u32.to_be_bytes());
    chunk(&mut out, *b"acTL", &actl);
    let mut sequence = 0_u32;
    for (index, png) in encoded.iter().enumerate() {
        let mut fctl = sequence.to_be_bytes().to_vec();
        sequence += 1;
        for value in [side, side, 0, 0] {
            fctl.extend_from_slice(&value.to_be_bytes());
        }
        fctl.extend_from_slice(&1_u16.to_be_bytes());
        fctl.extend_from_slice(&10_u16.to_be_bytes());
        fctl.extend_from_slice(&[0, 0]);
        chunk(&mut out, *b"fcTL", &fctl);
        for (kind, data) in chunks(png) {
            if &kind != b"IDAT" {
                continue;
            }
            if index == 0 {
                chunk(&mut out, *b"IDAT", &data);
            } else {
                let mut fdat = sequence.to_be_bytes().to_vec();
                sequence += 1;
                fdat.extend_from_slice(&data);
                chunk(&mut out, *b"fdAT", &fdat);
            }
        }
    }
    chunk(&mut out, *b"IEND", &[]);
    out
}

fn no_progress() -> impl FnMut(Progress) {
    |_| {}
}

/// A folder holding `files`, each (relative path, bytes).
fn folder(root: &Path, files: &[(&str, Vec<u8>)]) {
    for (name, bytes) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

/// A `.zip` holding `files`, each (entry name, bytes).
fn archive(path: &Path, files: &[(&str, Vec<u8>)]) {
    let mut zip = ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, bytes) in files {
        if name.ends_with('/') {
            zip.add_directory(*name, SimpleFileOptions::default()).unwrap();
        } else {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
    }
    zip.finish().unwrap();
}

#[test]
fn a_folder_of_mixed_files_imports_its_pictures_and_says_what_it_skipped() {
    let source = tempfile::tempdir().unwrap();
    let pack = source.path().join("Lab cats");
    folder(
        &pack,
        &[
            ("happy_cat.png", still(400, 300, [200, 40, 40], ImageFormat::Png)),
            ("sleepy-cat.webp", still(64, 64, [40, 200, 40], ImageFormat::WebP)),
            ("waving cat.gif", animated_gif(120, 3, 80)),
            ("moving.png", animated_png(90, 3)),
            ("more/deep_cat.png", still(32, 48, [1, 2, 3], ImageFormat::Png)),
            ("notes.txt", b"not a picture".to_vec()),
            ("photo.jpg", still(16, 16, [9, 9, 9], ImageFormat::Jpeg)),
            ("broken.png", b"\x89PNG\r\n\x1a\nbroken".to_vec()),
            (".hidden.png", still(16, 16, [9, 9, 9], ImageFormat::Png)),
        ],
    );
    let stickers = tempfile::tempdir().unwrap();
    let mut told = Vec::new();
    let imported = import::import(&pack, ImportKind::Folder, stickers.path(), &mut |progress| {
        told.push(progress);
    })
    .expect("imported");

    assert_eq!(imported.pack.name, "Lab cats");
    assert_eq!(imported.pack.id, "lab-cats");
    let labels: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["Happy cat", "Deep cat", "Moving", "Sleepy cat", "Waving cat"],
        "in the order of their paths"
    );
    let files: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.file.as_str())
        .collect();
    assert_eq!(
        files,
        ["001.png", "002.png", "003.gif", "004.png", "005.gif"],
        "animated ones as GIF"
    );
    let skipped: Vec<String> = imported.skipped.iter().map(ToString::to_string).collect();
    assert_eq!(
        skipped,
        [
            "broken.png can't be read",
            "notes.txt isn't a PNG, WebP or GIF picture or a Telegram sticker",
            "photo.jpg isn't a PNG, WebP or GIF picture or a Telegram sticker",
        ]
    );
    assert_eq!(
        told.first(),
        Some(&Progress { done: 0, total: 8 }),
        "the hidden file isn't counted"
    );
    assert_eq!(told.last(), Some(&Progress { done: 8, total: 8 }));
    assert_eq!(told.len(), 9, "after each file");

    sizes_and_frames_are_kept(&imported, stickers.path());
}

/// Kept at most 250 px, shown at most 180, never made larger; the animated ones keep their frames, loop, and have
/// only clear or whole pixels; nothing half-made is left behind.
fn sizes_and_frames_are_kept(imported: &import::Imported, stickers: &Path) {
    let happy = &imported.pack.stickers[0];
    assert_eq!(happy.size, (SHOWN_SIDE, 135));
    let kept = image::load_from_memory(&std::fs::read(stickers.join("lab-cats/001.png")).unwrap()).unwrap();
    assert_eq!((kept.width(), kept.height()), (KEPT_SIDE, 188));
    assert_eq!(imported.pack.stickers[3].size, (64, 64), "a small one stays its size");

    // the animated ones keep their frames, loop, and have only clear or whole pixels
    let waving = std::fs::read(stickers.join("lab-cats/005.gif")).unwrap();
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&waving)).unwrap();
    let frames: Vec<image::Frame> = image::AnimationDecoder::into_frames(decoder).collect_frames().unwrap();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].delay().numer_denom_ms(), (80, 1));
    assert!(frames[0].buffer().pixels().all(|pixel| matches!(pixel.0[3], 0 | 255)));
    assert_eq!(
        teams_proto::PastedFormat::sniff(&waving),
        Some(teams_proto::PastedFormat::Gif)
    );
    let moving = std::fs::read(stickers.join("lab-cats/003.gif")).unwrap();
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&moving)).unwrap();
    assert_eq!(
        image::AnimationDecoder::into_frames(decoder).count(),
        3,
        "an APNG's frames"
    );

    // nothing half-made is left behind
    let names: Vec<String> = std::fs::read_dir(stickers)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["lab-cats"]);
}

#[test]
fn a_zip_imports_like_a_folder_its_folders_and_a_mac_s_extras_passed_over() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("Party pack.zip");
    archive(
        &path,
        &[
            ("party/", Vec::new()),
            ("party/b-balloon.png", still(50, 50, [1, 200, 3], ImageFormat::Png)),
            ("party/a-cake.gif", animated_gif(60, 2, 0)),
            ("__MACOSX/party/._a-cake.gif", b"resource fork".to_vec()),
            ("party/.DS_Store", b"finder".to_vec()),
            ("party/readme.md", b"# made up".to_vec()),
        ],
    );
    let stickers = tempfile::tempdir().unwrap();
    let imported = import::import(&path, ImportKind::Archive, stickers.path(), &mut no_progress()).unwrap();
    assert_eq!(imported.pack.name, "Party pack");
    let labels: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.label.as_str())
        .collect();
    assert_eq!(labels, ["A cake", "B balloon"]);
    assert!(imported.pack.stickers[0].animated());
    assert!(!imported.pack.stickers[1].animated());
    assert_eq!(imported.skipped.len(), 1, "{:?}", imported.skipped);
    assert_eq!(imported.skipped[0].name, "readme.md");
    // a frame with no time gets a tenth of a second, as browsers play it
    let cake = std::fs::read(stickers.path().join("party-pack/001.gif")).unwrap();
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&cake)).unwrap();
    let first = image::AnimationDecoder::into_frames(decoder).next().unwrap().unwrap();
    assert_eq!(first.delay().numer_denom_ms(), (100, 1));
}

#[test]
fn bad_oversized_and_overlong_files_are_skipped_each_with_its_reason() {
    assert_eq!(import::convert(b"just text").unwrap_err(), Skip::NotPicture);
    assert_eq!(
        import::convert(&still(8, 8, [1, 2, 3], ImageFormat::Jpeg)).unwrap_err(),
        Skip::NotPicture
    );
    assert_eq!(import::convert(b"GIF89a\x01\x00").unwrap_err(), Skip::Unreadable);
    assert_eq!(
        import::convert(&animated_gif(4, MAX_FRAMES + 1, 20)).unwrap_err(),
        Skip::TooManyFrames
    );
    assert!(import::convert(&animated_gif(4, MAX_FRAMES, 20)).unwrap().animated);
    // a picture larger than the source limit is refused before it is decoded whole
    assert_eq!(
        import::convert(&still(4097, 2, [1, 2, 3], ImageFormat::Png)).unwrap_err(),
        Skip::TooBig
    );
    // a one-frame GIF is a still sticker
    let one = import::convert(&animated_gif(20, 1, 50)).unwrap();
    assert!(!one.animated);
    assert_eq!(
        teams_proto::PastedFormat::sniff(&one.bytes),
        Some(teams_proto::PastedFormat::Png)
    );

    let source = tempfile::tempdir().unwrap();
    let mut huge = still(8, 8, [1, 2, 3], ImageFormat::Png);
    huge.resize(usize::try_from(MAX_FILE).unwrap() + 1, 0);
    folder(
        source.path(),
        &[
            ("huge.png", huge.clone()),
            ("fine.png", still(8, 8, [1, 2, 3], ImageFormat::Png)),
        ],
    );
    let stickers = tempfile::tempdir().unwrap();
    let imported = import::import(source.path(), ImportKind::Folder, stickers.path(), &mut no_progress()).unwrap();
    assert_eq!(imported.skipped.len(), 1);
    assert_eq!(imported.skipped[0].to_string(), "huge.png is larger than 10 MB");
    // a picture made larger than a sticker may be is told apart from a file too large to read
    assert_eq!(Skip::TooLargeSticker.to_string(), "is larger than 8 MB as a sticker");
    assert_eq!(store::MAX_STICKER, 8 * 1024 * 1024);
    // the same in an archive, whose entry says its size
    let zipped = source.path().join("huge.zip");
    archive(
        &zipped,
        &[
            ("huge.png", huge),
            ("fine.png", still(8, 8, [1, 2, 3], ImageFormat::Png)),
        ],
    );
    let imported = import::import(&zipped, ImportKind::Archive, stickers.path(), &mut no_progress()).unwrap();
    assert_eq!(imported.skipped[0].why, Skip::TooLarge);
}

#[test]
fn nothing_to_import_says_why_and_leaves_nothing_behind() {
    let stickers = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    folder(source.path(), &[("a.txt", b"text".to_vec())]);
    match import::import(source.path(), ImportKind::Folder, stickers.path(), &mut no_progress()) {
        Err(ImportError::NoStickers(skipped)) => assert_eq!(skipped[0].why, Skip::NotPicture),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        import::import(
            &source.path().join("missing"),
            ImportKind::Folder,
            stickers.path(),
            &mut no_progress()
        )
        .unwrap_err(),
        ImportError::NoFolder
    );
    let not_zip = source.path().join("a.txt");
    assert_eq!(
        import::import(&not_zip, ImportKind::Archive, stickers.path(), &mut no_progress()).unwrap_err(),
        ImportError::BadArchive
    );
    assert_eq!(
        import::import(source.path(), ImportKind::Archive, stickers.path(), &mut no_progress()).unwrap_err(),
        ImportError::BadArchive,
        "a folder isn't an archive"
    );
    assert!(std::fs::read_dir(stickers.path()).unwrap().next().is_none());
    assert!(
        ImportError::NoStickers(Vec::new())
            .to_string()
            .contains("PNG, WebP or GIF")
    );
}

/// A source of `count` copies of one small picture.
struct Copies {
    count: usize,
    picture: Vec<u8>,
}

impl Source for Copies {
    fn name(&self) -> String {
        "  ".to_owned()
    }

    fn len(&self) -> usize {
        self.count
    }

    fn file_name(&self, index: usize) -> String {
        format!("{index:04}.png")
    }

    fn read(&mut self, _: usize) -> Result<Vec<u8>, Skip> {
        Ok(self.picture.clone())
    }
}

#[test]
fn a_pack_holds_at_most_its_limit_and_a_nameless_one_gets_a_name() {
    let stickers = tempfile::tempdir().unwrap();
    let mut source = Copies {
        count: MAX_STICKERS + 2,
        picture: still(4, 4, [1, 2, 3], ImageFormat::Png),
    };
    assert!(!source.is_empty());
    let imported = import::import_from(&mut source, stickers.path(), &mut no_progress()).unwrap();
    assert_eq!(imported.pack.stickers.len(), MAX_STICKERS);
    assert_eq!(imported.pack.name, import::DEFAULT_PACK_NAME);
    assert_eq!(imported.skipped.len(), 2);
    assert_eq!(imported.skipped[0].why, Skip::PackFull);
    assert_eq!(imported.pack.stickers[0].label, "0000", "a number is its own label");
    // a second pack of the same name is numbered
    let mut again = Copies {
        count: 1,
        picture: still(4, 4, [1, 2, 3], ImageFormat::Png),
    };
    let second = import::import_from(&mut again, stickers.path(), &mut no_progress()).unwrap();
    assert_eq!(
        (second.pack.id.as_str(), second.pack.name.as_str()),
        ("my-stickers-2", "My stickers (2)")
    );
}

#[test]
fn labels_and_sizes_are_made_as_teams_shows_stickers() {
    assert_eq!(import::label_of("dir/thumbs_up-big.png"), "Thumbs up big");
    assert_eq!(import::label_of("éclair.gif"), "Éclair");
    assert_eq!(import::label_of(".png"), import::DEFAULT_LABEL);
    assert_eq!(import::label_of("___.png"), import::DEFAULT_LABEL);
    assert_eq!(import::fit((500, 250), 180), (180, 90));
    assert_eq!(import::fit((100, 50), 180), (100, 50));
    assert_eq!(import::fit((0, 0), 180), (1, 1));
}

#[test]
fn packs_are_kept_across_a_restart_renamed_and_removed_with_their_recent_stickers() {
    let stickers = tempfile::tempdir().unwrap();
    let dir = stickers.path();
    let source = tempfile::tempdir().unwrap();
    folder(
        &source.path().join("One"),
        &[
            ("a.png", still(10, 10, [1, 2, 3], ImageFormat::Png)),
            ("b.gif", animated_gif(10, 2, 50)),
        ],
    );
    folder(
        &source.path().join("Two"),
        &[("c.png", still(10, 10, [1, 2, 3], ImageFormat::Png))],
    );
    let one = import::import(&source.path().join("One"), ImportKind::Folder, dir, &mut no_progress()).unwrap();
    let two = import::import(&source.path().join("Two"), ImportKind::Folder, dir, &mut no_progress()).unwrap();

    // read again from the disk, as the next run does
    let packs = store::list(dir);
    assert_eq!(packs, [one.pack.clone(), two.pack.clone()]);
    let b = StickerRef {
        pack: "one".to_owned(),
        file: "002.gif".to_owned(),
    };
    let c = StickerRef {
        pack: "two".to_owned(),
        file: "001.png".to_owned(),
    };
    assert_eq!(
        store::read_sticker(dir, &b).map(|bytes| bytes.len()),
        Some(std::fs::read(dir.join("one/002.gif")).unwrap().len())
    );
    assert!(store::recent(dir).is_empty());
    store::record_recent(dir, &b).unwrap();
    store::record_recent(dir, &c).unwrap();
    assert_eq!(
        store::record_recent(dir, &b).unwrap(),
        [b.clone(), c.clone()],
        "the last first, once each"
    );
    assert_eq!(store::recent(dir), [b.clone(), c.clone()]);

    let renamed = store::rename(dir, "one", "  Lab favourites ").unwrap();
    assert_eq!(renamed.name, "Lab favourites");
    assert_eq!(store::list(dir)[0].name, "Lab favourites");
    assert_eq!(store::list(dir)[0].stickers, one.pack.stickers, "its stickers kept");
    assert_eq!(store::rename(dir, "one", " ").unwrap_err(), store::StoreError::BadName);
    assert_eq!(store::rename(dir, "nine", "x").unwrap_err(), store::StoreError::NoPack);

    store::remove(dir, "one").unwrap();
    assert_eq!(store::list(dir), [two.pack]);
    assert_eq!(store::recent(dir), [c], "its stickers left Recent");
    assert_eq!(store::remove(dir, "../two").unwrap_err(), store::StoreError::NoPack);
    assert_eq!(store::list(dir).len(), 1);
}

#[test]
fn the_store_reads_only_what_the_app_writes() {
    let stickers = tempfile::tempdir().unwrap();
    let dir = stickers.path();
    // a pack whose manifest names files the app doesn't make, and one whose file is gone
    std::fs::create_dir_all(dir.join("odd")).unwrap();
    std::fs::write(
        dir.join("odd/pack.toml"),
        "name = \"Odd\"\nadded = 5\n[[stickers]]\nfile = \"../../secret.png\"\nlabel = \"x\"\nwidth = 10\nheight = 10\n\
         [[stickers]]\nfile = \"001.png\"\nlabel = \"\"\nwidth = 10\nheight = 10\n\
         [[stickers]]\nfile = \"002.png\"\nlabel = \"too wide\"\nwidth = 1000\nheight = 10\n",
    )
    .unwrap();
    std::fs::write(dir.join("odd/001.png"), still(4, 4, [1, 2, 3], ImageFormat::Png)).unwrap();
    std::fs::write(dir.join("odd/002.png"), still(4, 4, [1, 2, 3], ImageFormat::Png)).unwrap();
    std::fs::create_dir_all(dir.join("Bad Id")).unwrap();
    std::fs::write(dir.join("Bad Id/pack.toml"), "name = \"x\"\nstickers = []\n").unwrap();
    std::fs::create_dir_all(dir.join(".import-1-1")).unwrap();
    std::fs::write(
        dir.join("recent.toml"),
        "recent = [\"odd/001.png\", \"odd/001.png\", \"odd/../x\", \"gone/001.png\"]",
    )
    .unwrap();
    let packs = store::list(dir);
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].stickers.len(), 1, "{:?}", packs[0].stickers);
    assert_eq!(packs[0].stickers[0].label, import::DEFAULT_LABEL);
    assert_eq!(store::recent(dir).len(), 1);
    assert!(
        store::read_sticker(
            dir,
            &StickerRef {
                pack: "odd".to_owned(),
                file: "../pack.toml".to_owned()
            }
        )
        .is_none()
    );
    assert!(store::is_id("lab-cats-2") && !store::is_id("-x") && !store::is_id("A") && !store::is_id(""));
    assert!(store::is_file_name("001.gif") && !store::is_file_name("001.webp") && !store::is_file_name("a.png"));
}

#[test]
fn an_archive_from_memory_and_a_folder_tell_their_files() {
    let mut bytes = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut bytes));
        zip.start_file("x/one.png", SimpleFileOptions::default()).unwrap();
        zip.write_all(&still(4, 4, [1, 2, 3], ImageFormat::Png)).unwrap();
        zip.finish().unwrap();
    }
    let mut zip = Archive::over("Mem".to_owned(), Cursor::new(bytes)).unwrap();
    assert_eq!(
        (zip.name(), zip.len(), zip.file_name(0)),
        ("Mem".to_owned(), 1, "one.png".to_owned())
    );
    assert!(zip.read(0).is_ok());
    assert_eq!(zip.read(5).unwrap_err(), Skip::Unreadable);
    assert_eq!(zip.file_name(5), "");

    let source = tempfile::tempdir().unwrap();
    folder(source.path(), &[("a/b/c/d/e/deep.png", vec![1]), ("top.png", vec![1])]);
    let mut files = Folder::open(source.path()).unwrap();
    assert_eq!(files.len(), 1, "five folders deep is too deep");
    assert_eq!(files.file_name(0), "top.png");
    assert_eq!(files.read(3).unwrap_err(), Skip::Unreadable);
    assert_eq!(files.file_name(3), "");
}

/// A `.zip` of one deflated entry `name` holding `bytes`, whose two headers say it is `declared` bytes uncompressed.
fn archive_that_lies(name: &str, bytes: &[u8], declared: u32) -> Vec<u8> {
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(name, options).unwrap();
    zip.write_all(bytes).unwrap();
    let mut out = zip.finish().unwrap().into_inner();
    let real = u32::try_from(bytes.len()).unwrap().to_le_bytes();
    let local = out
        .windows(4)
        .position(|four| four == [0x50, 0x4b, 0x03, 0x04])
        .unwrap()
        + 22;
    let central = out
        .windows(4)
        .rposition(|four| four == [0x50, 0x4b, 0x01, 0x02])
        .unwrap()
        + 24;
    for at in [local, central] {
        assert_eq!(out[at..at + 4], real, "the uncompressed size at {at}");
        out[at..at + 4].copy_from_slice(&declared.to_le_bytes());
    }
    out
}

#[test]
fn a_zip_entry_that_says_it_is_small_is_not_read_past_its_word() {
    let mut huge = still(8, 8, [1, 2, 3], ImageFormat::Png);
    huge.resize(usize::try_from(MAX_FILE).unwrap() + 1024 * 1024, 0);
    let bytes = archive_that_lies("huge.png", &huge, 100);
    let mut zip = Archive::over("Liar".to_owned(), Cursor::new(bytes)).unwrap();
    // the archive's reader stops at the size it declares, and the entry's check then fails
    assert_eq!(zip.read(0).unwrap_err(), Skip::Unreadable);
    let stickers = tempfile::tempdir().unwrap();
    match import::import_from(&mut zip, stickers.path(), &mut no_progress()) {
        Err(ImportError::NoStickers(skipped)) => assert_eq!(skipped[0].why, Skip::Unreadable),
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_folder_s_symbolic_links_are_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    folder(
        outside.path(),
        &[("secret.png", still(8, 8, [9, 9, 9], ImageFormat::Png))],
    );
    let source = tempfile::tempdir().unwrap();
    let pack = source.path().join("Linked");
    folder(&pack, &[("fine.png", still(8, 8, [1, 2, 3], ImageFormat::Png))]);
    std::os::unix::fs::symlink(outside.path().join("secret.png"), pack.join("link.png")).unwrap();
    std::os::unix::fs::symlink(outside.path(), pack.join("linked-folder")).unwrap();
    let stickers = tempfile::tempdir().unwrap();
    let imported = import::import(&pack, ImportKind::Folder, stickers.path(), &mut no_progress()).unwrap();
    let labels: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.label.as_str())
        .collect();
    assert_eq!(labels, ["Fine"], "a link to a file or a folder is passed over");
    assert!(imported.skipped.is_empty(), "{:?}", imported.skipped);
}

/// A source of a picture, then files of [`MAX_FILE`] bytes each, more than [`MAX_TOTAL`] together.
struct Heavy {
    picture: Vec<u8>,
}

impl Source for Heavy {
    fn name(&self) -> String {
        "Heavy".to_owned()
    }

    fn len(&self) -> usize {
        usize::try_from(MAX_TOTAL / MAX_FILE).unwrap() + 2
    }

    fn file_name(&self, index: usize) -> String {
        format!("{index:02}.png")
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        if index == 0 {
            return Ok(self.picture.clone());
        }
        Ok(vec![0; usize::try_from(MAX_FILE).unwrap()])
    }
}

#[test]
fn an_import_stops_at_its_total_and_keeps_nothing() {
    let stickers = tempfile::tempdir().unwrap();
    let mut source = Heavy {
        picture: still(4, 4, [1, 2, 3], ImageFormat::Png),
    };
    let mut done = 0;
    let outcome = import::import_from(&mut source, stickers.path(), &mut |progress| done = progress.done);
    assert_eq!(outcome.unwrap_err(), ImportError::TooMuch);
    assert!(done < source.len(), "it stops at the file past the total");
    assert!(
        std::fs::read_dir(stickers.path()).unwrap().next().is_none(),
        "no pack is made"
    );
}

// --- Telegram ---

/// The synthetic, marked animation of the fixtures, gzipped as a `.tgs` is.
fn synthetic_tgs() -> Vec<u8> {
    let json = include_str!("../../../tests/fixtures/telegram/synthetic-sticker.json");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(json.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

/// The first bytes of a Matroska file, as a `.webm` video sticker starts.
fn webm() -> Vec<u8> {
    vec![0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01]
}

/// What a Telegram import made: still and animated stickers, the video skipped.
fn telegram_pack_made(imported: &import::Imported, stickers: &Path, video: &str) {
    let files: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.file.as_str())
        .collect();
    assert_eq!(files, ["001.gif", "002.png"], "the .tgs animated, the .webp still");
    let gif = std::fs::read(stickers.join(&imported.pack.id).join("001.gif")).unwrap();
    assert_eq!(
        teams_proto::PastedFormat::sniff(&gif),
        Some(teams_proto::PastedFormat::Gif)
    );
    assert_eq!(imported.pack.stickers[0].size, (SHOWN_SIDE, SHOWN_SIDE));
    assert_eq!(
        imported.skipped,
        [import::Skipped {
            name: video.to_owned(),
            why: Skip::Video
        }]
    );
    assert_eq!(
        imported.skipped[0].to_string(),
        format!("{video} is a video sticker, which can't be imported yet")
    );
}

#[test]
fn telegram_files_import_one_by_one_from_a_folder_and_from_a_zip() {
    let files = [
        ("hello.tgs", synthetic_tgs()),
        ("wave.webp", still(512, 512, [10, 200, 10], ImageFormat::WebP)),
        ("party.webm", webm()),
    ];
    let source = tempfile::tempdir().unwrap();
    let pack = source.path().join("Telegram cats");
    folder(&pack, &files);
    let stickers = source.path().join("stickers");

    let imported = import::import(&pack, ImportKind::Folder, &stickers, &mut no_progress()).unwrap();
    telegram_pack_made(&imported, &stickers, "party.webm");
    assert_eq!(imported.pack.stickers[0].label, "Hello");

    let zip = source.path().join("Zipped cats.zip");
    archive(&zip, &files);
    let imported = import::import(&zip, ImportKind::Archive, &stickers, &mut no_progress()).unwrap();
    telegram_pack_made(&imported, &stickers, "party.webm");
    assert_eq!(imported.pack.name, "Zipped cats");

    let chosen = vec![
        pack.join("wave.webp"),
        pack.join("party.webm"),
        pack.join("hello.tgs"),
        pack.join("hello.tgs"),
    ];
    let mut one_by_one = Files::new(chosen);
    assert_eq!((one_by_one.name(), one_by_one.len()), ("Telegram cats".to_owned(), 3));
    assert_eq!(one_by_one.read(7).unwrap_err(), Skip::Unreadable);
    let imported = import::import_from(&mut one_by_one, &stickers, &mut no_progress()).unwrap();
    telegram_pack_made(&imported, &stickers, "party.webm");
    assert_eq!(imported.pack.name, "Telegram cats (2)");
}

/// A mock pack: a still, an animated and a video sticker.
fn mock_cats() -> MockSet {
    MockSet {
        name: "SyntheticCats".to_owned(),
        title: "Synthetic cats".to_owned(),
        stickers: vec![
            MockSticker {
                kind: MockKind::Animated,
                emoji: "\u{1f638}".to_owned(),
                bytes: synthetic_tgs(),
            },
            MockSticker {
                kind: MockKind::Still,
                emoji: "\u{1f63a}".to_owned(),
                bytes: still(512, 512, [200, 10, 10], ImageFormat::WebP),
            },
            MockSticker {
                kind: MockKind::Video,
                emoji: "\u{1f639}".to_owned(),
                bytes: webm(),
            },
        ],
    }
}

/// The app's token (in the memory) holding `token`, and the Bot API made for the mock.
fn bot(mock: &BotApiMock, token: Option<&str>) -> (BotToken, BotApis) {
    let tokens = BotToken::default();
    if let Some(token) = token {
        tokens.put(&Secret::new(token)).unwrap();
    }
    let origin = mock.origin();
    let apis = BotApis::new(Arc::new(move |token| {
        teams_core::telegram::BotApi::with_mock_origin(token, &origin)
    }));
    (tokens, apis)
}

#[test]
fn a_telegram_link_imports_its_pack_through_the_mock_bot_api() {
    let log_dir = tempfile::tempdir().unwrap();
    let log = teams_log::install(&teams_log::Config {
        dir: log_dir.path().to_owned(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let mock = BotApiMock::start(vec![mock_cats()]).unwrap();
    let (tokens, apis) = bot(&mock, Some(SYNTHETIC_BOT_TOKEN));
    let stickers = tempfile::tempdir().unwrap();
    let mut told = Vec::new();
    let imported = telegram::import_link(
        "https://t.me/addstickers/SyntheticCats",
        &tokens,
        &apis,
        stickers.path(),
        &mut |progress| told.push(progress),
    )
    .unwrap();
    assert_eq!(imported.pack.name, "Synthetic cats");
    telegram_pack_made(&imported, stickers.path(), "Sticker 3 \u{1f639}");
    let labels: Vec<&str> = imported
        .pack
        .stickers
        .iter()
        .map(|sticker| sticker.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["\u{1f638}", "\u{1f63a}"],
        "each sticker is called by its emoji"
    );
    assert_eq!(told.last(), Some(&Progress { done: 3, total: 3 }));

    let targets: Vec<String> = mock.requests().into_iter().map(|request| request.target).collect();
    assert_eq!(
        targets.len(),
        5,
        "the pack, then two files' paths and bytes; the video not asked: {targets:?}"
    );
    assert!(targets.iter().all(|target| !target.contains("__2")), "{targets:?}");
    assert!(mock.requests().iter().all(|request| !request.authorization));

    assert!(log.flush_written());
    let mut text = String::new();
    for entry in std::fs::read_dir(log_dir.path()).unwrap() {
        text.push_str(&String::from_utf8_lossy(&std::fs::read(entry.unwrap().path()).unwrap()));
    }
    assert!(!text.contains(SYNTHETIC_BOT_TOKEN), "the token is in no log line");
    assert!(!text.contains("SYNTHETIC_bot_token"), "nor a part of it");
}

#[test]
fn a_telegram_link_s_failures_each_say_what_to_do() {
    let mock = BotApiMock::start(vec![mock_cats()]).unwrap();
    let stickers = tempfile::tempdir().unwrap();
    let import = |link: &str, token: Option<&str>| {
        let (tokens, apis) = bot(&mock, token);
        telegram::import_link(link, &tokens, &apis, stickers.path(), &mut no_progress())
    };
    assert_eq!(
        import("t.me/addstickers/NoSuchPack", Some(SYNTHETIC_BOT_TOKEN)).unwrap_err(),
        ImportError::NoSuchPack
    );
    let wrong = "123456789:SYNTHETIC_wrong_token_made_up_0000";
    assert_eq!(
        import("tg://addstickers?set=SyntheticCats", Some(wrong)).unwrap_err(),
        ImportError::BadToken
    );
    assert_eq!(
        import("tg://addstickers?set=SyntheticCats", Some("not a token")).unwrap_err(),
        ImportError::BadToken
    );
    let asked = mock.requests().len();
    assert_eq!(
        import("t.me/addstickers/SyntheticCats", None).unwrap_err(),
        ImportError::NoToken
    );
    assert_eq!(
        import("https://example.test/addstickers/x", Some(SYNTHETIC_BOT_TOKEN)).unwrap_err(),
        ImportError::BadLink
    );
    assert_eq!(
        mock.requests().len(),
        asked,
        "nothing is sent without a token or a link"
    );
    assert_eq!(
        ImportError::BadToken.to_string(),
        "Telegram refused the bot token. Check it, or replace it."
    );
    assert!(std::fs::read_dir(stickers.path()).map_or(true, |mut dir| dir.next().is_none()));

    let only_video = MockSet {
        name: "OnlyVideo".to_owned(),
        title: "Only video".to_owned(),
        stickers: vec![MockSticker {
            kind: MockKind::Video,
            emoji: "\u{1f3ac}".to_owned(),
            bytes: webm(),
        }],
    };
    let mock = BotApiMock::start(vec![only_video]).unwrap();
    let (tokens, apis) = bot(&mock, Some(SYNTHETIC_BOT_TOKEN));
    let failed = telegram::import_link(
        "t.me/addstickers/OnlyVideo",
        &tokens,
        &apis,
        stickers.path(),
        &mut no_progress(),
    );
    let Err(ImportError::NoStickers(skipped)) = failed else {
        panic!("{failed:?}");
    };
    assert_eq!(skipped[0].why, Skip::Video);

    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let apis = BotApis::new(Arc::new(move |token| {
        teams_core::telegram::BotApi::with_mock_origin(token, &format!("http://{dead}"))
    }));
    assert_eq!(
        telegram::import_link(
            "t.me/addstickers/SyntheticCats",
            &tokens,
            &apis,
            stickers.path(),
            &mut no_progress()
        )
        .unwrap_err(),
        ImportError::TelegramDown
    );
}

#[test]
fn a_telegram_link_names_its_pack_in_each_form() {
    for link in [
        "https://t.me/addstickers/SyntheticCats",
        "http://www.t.me/addstickers/SyntheticCats/",
        "t.me/addstickers/SyntheticCats?startapp=1",
        "  https://telegram.me/addstickers/SyntheticCats#top  ",
        "HTTPS://T.ME/addstickers/SyntheticCats",
        "https://t.me/addemoji/SyntheticCats",
        "tg://addstickers?set=SyntheticCats",
        "tg://addemoji?foo=1&set=SyntheticCats",
        "SyntheticCats",
    ] {
        assert_eq!(telegram::set_name(link).as_deref(), Some("SyntheticCats"), "{link}");
    }
    for link in [
        "",
        "https://example.test/addstickers/SyntheticCats",
        "https://t.me/joinchat/SyntheticCats",
        "https://t.me/addstickers/",
        "https://t.me/addstickers/bad name",
        "tg://addstickers?name=SyntheticCats",
        "tg://resolve?set=SyntheticCats",
        "Synthetic Cats",
    ] {
        assert_eq!(telegram::set_name(link), None, "{link}");
    }
}
