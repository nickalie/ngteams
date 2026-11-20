//! The sticker store and the import of a folder or a `.zip` of pictures (`teams_ui::stickers`): every picture is made
//! here, in the test; nothing is read from the repo.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::{Cursor, Write as _};
use std::path::Path;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, ImageFormat, Rgba, RgbaImage};
use teams_ui::stickers::import::{
    self, Archive, Folder, ImportError, ImportKind, KEPT_SIDE, MAX_FILE, MAX_FRAMES, Progress, SHOWN_SIDE, Skip, Source,
};
use teams_ui::stickers::store::{self, MAX_STICKERS, StickerRef};
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
            "notes.txt isn't a PNG, WebP or GIF picture",
            "photo.jpg isn't a PNG, WebP or GIF picture",
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
