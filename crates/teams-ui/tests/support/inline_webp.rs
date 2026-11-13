//! Synthetic WebP sources exercise entropy storage and container/frame dimension mismatches.

use std::sync::Arc;

use teams_proto::{PastedFormat, PastedPicture};
use teams_ui::inline;

// Synthetic 2x2 black VP8 frame, encoded with ffmpeg/libwebp.
const VP8: &[u8] = &[
    48, 1, 0, 157, 1, 42, 2, 0, 2, 0, 2, 0, 52, 37, 164, 0, 3, 112, 0, 254, 251, 253, 80, 0,
];

#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    offset: usize,
}

impl Bits {
    fn push(&mut self, value: u32, count: usize) {
        for index in 0..count {
            if self.offset.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let last = self.bytes.len() - 1;
            self.bytes[last] |= u8::try_from((value >> index) & 1).unwrap() << (self.offset % 8);
            self.offset += 1;
        }
    }

    fn single(&mut self, symbol: u8) {
        self.push(1, 1);
        self.push(0, 1);
        self.push(1, 1);
        self.push(u32::from(symbol), 8);
    }

    fn lookup_table(&mut self) {
        self.push(0, 1);
        self.push(15, 4);
        for symbol in [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            self.push(u32::from(symbol < 16) * 4, 3);
        }
        self.push(1, 1);
        self.push(1, 3);
        self.push(14, 4);
        // A complete tree with lengths 1..14,15,15 needs a lookup table and tail nodes.
        for length in [1_u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 15] {
            self.push(length.reverse_bits() >> 28, 4);
        }
    }
}

fn lossless(groups: u32, tables: bool) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.push(0x2f, 8);
    bits.push(1, 14);
    bits.push(1, 14);
    bits.push(1, 1);
    bits.push(0, 3);
    bits.push(0, 1);
    bits.push(0, 1);
    bits.push(1, 1);
    bits.push(0, 3);
    bits.push(0, 1);
    let index = groups - 1;
    for symbol in [index & 255, index >> 8, 0, 255, 0] {
        bits.single(u8::try_from(symbol).unwrap());
    }
    for _ in 0..groups {
        for _ in 0..5 {
            if tables {
                bits.lookup_table();
            } else {
                bits.single(0);
            }
        }
    }
    if tables {
        bits.push(0, 16);
    }
    bits.bytes
}

fn chunk(bytes: &mut Vec<u8>, tag: [u8; 4], data: &[u8]) {
    bytes.extend(tag);
    bytes.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    bytes.extend(data);
    if !data.len().is_multiple_of(2) {
        bytes.push(0);
    }
}

fn picture(groups: u32, tables: bool, alpha: bool) -> teams_core::PictureBytes {
    let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
    let lossless = lossless(groups, tables);
    if alpha {
        chunk(&mut bytes, *b"VP8X", &[0x10, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
        let mut data = vec![1];
        data.extend(&lossless[5..]);
        chunk(&mut bytes, *b"ALPH", &data);
        chunk(&mut bytes, *b"VP8 ", VP8);
    } else {
        chunk(&mut bytes, *b"VP8L", &lossless);
    }
    let length = u32::try_from(bytes.len() - 8).unwrap();
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    Arc::new(PastedPicture {
        format: PastedFormat::Webp,
        bytes,
    })
}

fn lossy_picture(vp8: &[u8], flags: u8) -> teams_core::PictureBytes {
    let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
    let animated = flags & 2 != 0;
    let side = if animated { 3 } else { 1 };
    chunk(&mut bytes, *b"VP8X", &[flags, 0, 0, 0, side, 0, 0, side, 0, 0]);
    let mut frame = Vec::new();
    if flags & 0x10 != 0 {
        chunk(&mut frame, *b"ALPH", &[0, 255, 255, 255, 255]);
    }
    chunk(&mut frame, *b"VP8 ", vp8);
    if animated {
        chunk(&mut bytes, *b"ANIM", &[0; 6]);
        let mut data = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 2];
        data.extend(frame);
        chunk(&mut bytes, *b"ANMF", &data);
    } else {
        bytes.extend(frame);
    }
    let length = u32::try_from(bytes.len() - 8).unwrap();
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    Arc::new(PastedPicture {
        format: PastedFormat::Webp,
        bytes,
    })
}

#[test]
fn lossy_frames_match_their_container_before_decoding_even_when_truncated() {
    for flags in [0, 0x10, 2, 0x12] {
        let valid = lossy_picture(VP8, flags);
        let expected = if flags & 2 != 0 { (4, 4) } else { (2, 2) };
        assert_eq!(inline::decode(&valid).unwrap().1, expected);
        for length in [10, VP8.len()] {
            for (width, height) in [
                (16383_u16, 16383_u16),
                (16383, 2),
                (2, 16383),
                (1, 2),
                (2, 1),
                (0, 2),
                (2, 0),
            ] {
                let mut vp8 = VP8[..length].to_vec();
                vp8[6..8].copy_from_slice(&width.to_le_bytes());
                vp8[8..10].copy_from_slice(&height.to_le_bytes());
                let picture = lossy_picture(&vp8, flags);
                let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(&picture.bytes)).unwrap();
                let mut output = vec![0; decoder.output_buffer_size().unwrap()];
                assert!(matches!(
                    decoder.read_image(&mut output),
                    Err(image_webp::DecodingError::InconsistentImageSizes)
                ));
                if flags & 2 != 0 {
                    assert!(matches!(
                        decoder.read_frame(&mut output),
                        Err(image_webp::DecodingError::InconsistentImageSizes)
                    ));
                }
                assert!(inline::decode(&picture).is_none());
                assert!(inline::decode_within(&picture, teams_ui::picture_viewer::MAX_VIEWER_SIDE).is_none());
            }
        }
    }
}

#[test]
fn lossless_huffman_groups_and_tables_are_bounded_in_pictures_and_compressed_alpha() {
    for alpha in [false, true] {
        for tables in [false, true] {
            let small = picture(2, tables, alpha);
            let (_, natural) = inline::decode(&small).unwrap();
            assert_eq!(natural, (2, 2));
            let groups = if tables { 1024 } else { 65536 };
            let large = picture(groups, tables, alpha);
            let error = image::load_from_memory_with_format(&large.bytes, image::ImageFormat::WebP).unwrap_err();
            assert!(error.to_string().contains("Memory limit exceeded"), "{error}");
            assert!(inline::decode(&large).is_none());
            assert!(inline::decode_within(&large, teams_ui::picture_viewer::MAX_VIEWER_SIDE).is_none());
        }
    }
}
