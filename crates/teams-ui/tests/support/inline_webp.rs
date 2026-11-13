//! Synthetic lossless WebP entropy images select arbitrary group indices, including in compressed alpha.

use std::sync::Arc;

use teams_proto::{PastedFormat, PastedPicture};
use teams_ui::inline;

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
        // Synthetic 2x2 black VP8 frame, encoded with ffmpeg/libwebp.
        chunk(
            &mut bytes,
            *b"VP8 ",
            &[
                48, 1, 0, 157, 1, 42, 2, 0, 2, 0, 2, 0, 52, 37, 164, 0, 3, 112, 0, 254, 251, 253, 80, 0,
            ],
        );
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
