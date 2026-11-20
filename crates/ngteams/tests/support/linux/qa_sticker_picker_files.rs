//! The made-up sticker files of the sticker picker QA: a folder and a `.zip` file of pictures that the user imports
//! as packs. Every picture is a face of one colour on a clear ground, so the test finds each sticker on the screen by
//! that colour. The animated ones open and close their mouth. All of them are made here, in the test.

use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, ImageFormat, Rgba, RgbaImage};
use ui_sandbox::SandboxResult;
use zip::write::{SimpleFileOptions, ZipWriter};

pub type Rgb = [u8; 3];

/// The eyes, the mouth and the rim of each face.
pub const INK: Rgb = [50, 35, 20];
/// A still PNG.
pub const SUNNY: Rgb = [250, 200, 40];
/// An animated PNG (APNG).
pub const MINTY: Rgb = [60, 190, 90];
/// A still WebP.
pub const ROSY: Rgb = [235, 120, 185];
/// An animated WebP.
pub const TANGY: Rgb = [245, 140, 50];
/// An animated GIF.
pub const PLUMMY: Rgb = [170, 110, 230];
/// The animated GIFs that make the import of the folder take time.
pub const FILLER: Rgb = [150, 150, 150];
/// A still PNG in the `.zip` file.
pub const CYAN: Rgb = [40, 200, 210];
/// An animated GIF in the `.zip` file.
pub const LIME: Rgb = [170, 220, 40];
/// The pack that the user removes.
pub const TAN: Rgb = [210, 160, 120];

/// The colours of the stickers that the picker can show.
pub const ALL: [Rgb; 8] = [SUNNY, MINTY, ROSY, TANGY, PLUMMY, CYAN, LIME, TAN];

/// The side of each made-up picture, px; the face's radius.
const SIDE: u32 = 240;
const RADIUS: i64 = 100;
/// The slow GIFs in the folder, and their frames.
pub const FILLERS: usize = 8;
const FILLER_FRAMES: usize = 12;

/// The packs, as the import names them (the folder's or the file's name) and gives them an id.
pub const ZORBLETS: &str = "Qa zorblets";
pub const ZORBLETS_ID: &str = "qa-zorblets";
pub const QUIBBLES: &str = "Qa quibbles";
pub const QUIBBLES_ID: &str = "qa-quibbles";
pub const DOOMED: &str = "Qa doomed";
pub const DOOMED_ID: &str = "qa-doomed";
/// The folder the user asks to import while another import runs: the app refuses it.
pub const SPARE: &str = "Qa spare";
pub const SPARE_ID: &str = "qa-spare";

/// The files the folder import must skip, with a word of the reason the summary must give.
pub const SKIPPED: [(&str, &str); 5] = [
    ("broken.png", "can't be read"),
    ("notes.txt", "isn't a PNG, WebP or GIF"),
    ("huge.png", "10 MB"),
    ("giant.png", ""),
    ("frames.gif", ""),
];
/// The files the folder import must pass over without a word: a hidden file and a Mac's extras.
pub const PASSED_OVER: [&str; 2] = [".hidden.png", "._zorb_sunny.png"];

/// A face of `colour`, its mouth `open` px high.
fn face(colour: Rgb, open: i64) -> RgbaImage {
    RgbaImage::from_fn(SIDE, SIDE, |x, y| {
        let centre = i64::from(SIDE / 2);
        let (dx, dy) = (i64::from(x) - centre, i64::from(y) - centre);
        let distance = dx * dx + dy * dy;
        let eye = (dx + 35).pow(2) + (dy + 25).pow(2) <= 150 || (dx - 35).pow(2) + (dy + 25).pow(2) <= 150;
        let mouth = dx.abs() <= 30 && (dy - 40).abs() <= open / 2 + 1;
        if distance > RADIUS * RADIUS {
            Rgba([0, 0, 0, 0])
        } else if distance >= (RADIUS - 6).pow(2) || eye || mouth {
            Rgba([INK[0], INK[1], INK[2], 255])
        } else {
            Rgba([colour[0], colour[1], colour[2], 255])
        }
    })
}

/// The frames of an animated face: the mouth opens and closes.
fn moving(colour: Rgb, frames: usize) -> Vec<RgbaImage> {
    (0..frames)
        .map(|index| face(colour, if index % 2 == 0 { 4 } else { 30 }))
        .collect()
}

fn encoded(picture: &RgbaImage, format: ImageFormat) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    picture.write_to(&mut Cursor::new(&mut bytes), format)?;
    Ok(bytes)
}

fn gif(frames: Vec<RgbaImage>, ms: u32) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut bytes, 30);
        encoder.set_repeat(Repeat::Infinite)?;
        for frame in frames {
            encoder.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1)))?;
        }
    }
    Ok(bytes)
}

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

fn png_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> SandboxResult {
    out.extend_from_slice(&u32::try_from(data.len())?.to_be_bytes());
    let mut typed = kind.to_vec();
    typed.extend_from_slice(data);
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
    Ok(())
}

/// The chunks of a PNG file, as (type, data).
fn png_chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let mut at = 8;
    let mut found = Vec::new();
    while at + 8 <= png.len() {
        let mut length = [0; 4];
        length.copy_from_slice(&png[at..at + 4]);
        let length = u32::from_be_bytes(length) as usize;
        let mut kind = [0; 4];
        kind.copy_from_slice(&png[at + 4..at + 8]);
        let end = (at + 8 + length).min(png.len());
        found.push((kind, png[at + 8..end].to_vec()));
        at += 12 + length;
    }
    found
}

/// An animated PNG of `frames`, a quarter of a second each.
fn apng(frames: &[RgbaImage]) -> SandboxResult<Vec<u8>> {
    let pngs = frames
        .iter()
        .map(|frame| encoded(frame, ImageFormat::Png))
        .collect::<SandboxResult<Vec<_>>>()?;
    let first = pngs.first().ok_or("no frame")?;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let header = png_chunks(first).into_iter().next().ok_or("no IHDR")?;
    png_chunk(&mut out, *b"IHDR", &header.1)?;
    let mut actl = u32::try_from(frames.len())?.to_be_bytes().to_vec();
    actl.extend_from_slice(&0_u32.to_be_bytes());
    png_chunk(&mut out, *b"acTL", &actl)?;
    let mut sequence = 0_u32;
    for (index, png) in pngs.iter().enumerate() {
        let mut fctl = sequence.to_be_bytes().to_vec();
        sequence += 1;
        for value in [SIDE, SIDE, 0, 0] {
            fctl.extend_from_slice(&value.to_be_bytes());
        }
        fctl.extend_from_slice(&1_u16.to_be_bytes());
        fctl.extend_from_slice(&4_u16.to_be_bytes());
        fctl.extend_from_slice(&[0, 0]);
        png_chunk(&mut out, *b"fcTL", &fctl)?;
        for (kind, data) in png_chunks(png).into_iter().filter(|(kind, _)| kind == b"IDAT") {
            if index == 0 {
                png_chunk(&mut out, kind, &data)?;
            } else {
                let mut fdat = sequence.to_be_bytes().to_vec();
                sequence += 1;
                fdat.extend_from_slice(&data);
                png_chunk(&mut out, *b"fdAT", &fdat)?;
            }
        }
    }
    png_chunk(&mut out, *b"IEND", &[])?;
    Ok(out)
}

fn riff_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> SandboxResult {
    out.extend_from_slice(&kind);
    out.extend_from_slice(&u32::try_from(data.len())?.to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// The `VP8L` data of a still lossless WebP file.
fn vp8l_of(webp: &[u8]) -> SandboxResult<Vec<u8>> {
    let mut at = 12;
    while at + 8 <= webp.len() {
        let mut size = [0; 4];
        size.copy_from_slice(&webp[at + 4..at + 8]);
        let size = u32::from_le_bytes(size) as usize;
        let end = (at + 8 + size).min(webp.len());
        if &webp[at..at + 4] == b"VP8L" {
            return Ok(webp[at + 8..end].to_vec());
        }
        at += 8 + size + size % 2;
    }
    Err("no VP8L chunk in the encoded WebP".into())
}

fn u24(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes()[..3]);
}

/// An animated WebP of `frames`, a quarter of a second each, each frame a lossless picture of its own.
fn animated_webp(frames: &[RgbaImage]) -> SandboxResult<Vec<u8>> {
    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![0x12, 0, 0, 0];
    u24(&mut vp8x, SIDE - 1);
    u24(&mut vp8x, SIDE - 1);
    riff_chunk(&mut body, *b"VP8X", &vp8x)?;
    riff_chunk(&mut body, *b"ANIM", &[0, 0, 0, 0, 0, 0])?;
    for frame in frames {
        let mut anmf = Vec::new();
        for value in [0, 0, SIDE - 1, SIDE - 1, 250] {
            u24(&mut anmf, value);
        }
        anmf.push(0b10);
        riff_chunk(&mut anmf, *b"VP8L", &vp8l_of(&encoded(frame, ImageFormat::WebP)?)?)?;
        riff_chunk(&mut body, *b"ANMF", &anmf)?;
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(body.len())?.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn write_all(root: &Path, files: Vec<(String, Vec<u8>)>) -> SandboxResult {
    for (name, bytes) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// The good pictures of the folder: a still PNG, an APNG, a still WebP, an animated WebP and an animated GIF, and the
/// slow GIFs.
fn good_files() -> SandboxResult<Vec<(String, Vec<u8>)>> {
    let mut files = vec![
        ("zorb_sunny.png".to_owned(), encoded(&face(SUNNY, 4), ImageFormat::Png)?),
        ("zorb_minty.png".to_owned(), apng(&moving(MINTY, 4))?),
        ("zorb_rosy.webp".to_owned(), encoded(&face(ROSY, 4), ImageFormat::WebP)?),
        ("zorb_tangy.webp".to_owned(), animated_webp(&moving(TANGY, 4))?),
        ("zorb_plummy.gif".to_owned(), gif(moving(PLUMMY, 4), 250)?),
    ];
    for index in 0..FILLERS {
        files.push((
            format!("zz_wave_{index:02}.gif"),
            gif(moving(FILLER, FILLER_FRAMES), 100)?,
        ));
    }
    Ok(files)
}

/// The files the import must skip or pass over.
fn bad_files() -> SandboxResult<Vec<(String, Vec<u8>)>> {
    let mut huge = encoded(&face(SUNNY, 4), ImageFormat::Png)?;
    huge.resize(10 * 1024 * 1024 + 1, 0);
    let giant = RgbaImage::from_pixel(4097, 2, Rgba([1, 2, 3, 255]));
    let many = (0..201_u8)
        .map(|index| RgbaImage::from_pixel(4, 4, Rgba([index, 9, 9, 255])))
        .collect();
    Ok(vec![
        ("broken.png".to_owned(), b"\x89PNG\r\n\x1a\nbroken".to_vec()),
        ("notes.txt".to_owned(), b"not a picture".to_vec()),
        ("huge.png".to_owned(), huge),
        ("giant.png".to_owned(), encoded(&giant, ImageFormat::Png)?),
        ("frames.gif".to_owned(), gif(many, 20)?),
        (".hidden.png".to_owned(), encoded(&face(SUNNY, 4), ImageFormat::Png)?),
        (
            "__MACOSX/._zorb_sunny.png".to_owned(),
            encoded(&face(SUNNY, 4), ImageFormat::Png)?,
        ),
    ])
}

/// What the user imports: three folders and a `.zip` file, in a directory of their own.
pub struct Sources {
    _dir: tempfile::TempDir,
    pub zorblets: PathBuf,
    pub quibbles: PathBuf,
    pub doomed: PathBuf,
    pub spare: PathBuf,
}

impl Sources {
    pub fn make() -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let zorblets = dir.path().join(ZORBLETS);
        let mut files = good_files()?;
        files.extend(bad_files()?);
        write_all(&zorblets, files)?;
        let doomed = dir.path().join(DOOMED);
        write_all(
            &doomed,
            vec![("doomed_tan.png".to_owned(), encoded(&face(TAN, 4), ImageFormat::Png)?)],
        )?;
        let spare = dir.path().join(SPARE);
        write_all(
            &spare,
            vec![("spare_tan.png".to_owned(), encoded(&face(TAN, 4), ImageFormat::Png)?)],
        )?;
        let quibbles = dir.path().join(format!("{QUIBBLES}.zip"));
        write_zip(&quibbles)?;
        Ok(Self {
            _dir: dir,
            zorblets,
            quibbles,
            doomed,
            spare,
        })
    }
}

/// The `.zip` file: a still PNG and an animated GIF in a folder, a Mac's extras, a hidden file and a note.
fn write_zip(path: &Path) -> SandboxResult {
    let entries: Vec<(&str, Vec<u8>)> = vec![
        ("quibbles/", Vec::new()),
        ("quibbles/quib_cyan.png", encoded(&face(CYAN, 4), ImageFormat::Png)?),
        ("quibbles/quib_lime.gif", gif(moving(LIME, 4), 250)?),
        ("__MACOSX/quibbles/._quib_cyan.png", b"resource fork".to_vec()),
        ("quibbles/.DS_Store", b"finder".to_vec()),
        ("quibbles/readme.md", b"# made up".to_vec()),
    ];
    let mut zip = ZipWriter::new(std::fs::File::create(path)?);
    for (name, bytes) in entries {
        if name.ends_with('/') {
            zip.add_directory(name, SimpleFileOptions::default())?;
        } else {
            zip.start_file(name, SimpleFileOptions::default())?;
            zip.write_all(&bytes)?;
        }
    }
    zip.finish()?;
    Ok(())
}
