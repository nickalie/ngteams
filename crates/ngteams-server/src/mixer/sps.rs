//! The picture size an H.264 keyframe carries: its sequence parameter set's width and height in macroblocks of 16 by
//! 16 pixels, as a source request's `max-fs` counts them. The mixer forwards a stream unscaled, so this is what a
//! receiving line's limit is held to (`super::video`).

/// The NAL unit type of a sequence parameter set.
const SPS: u8 = 7;
/// The profiles whose sequence parameter set names its chroma format and bit depths.
const HIGH_PROFILES: [u8; 14] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135, 144];

/// The macroblocks of each picture of the stream whose keyframe is `data` (Annex B: NAL units after start codes), from
/// its first sequence parameter set; none when it carries none or one this reads no size of (a scaling matrix).
pub(crate) fn macroblocks(data: &[u8]) -> Option<u32> {
    nal_units(data)
        .find(|unit| unit.first().is_some_and(|header| header & 0x1F == SPS))
        .and_then(|unit| read_sps(&unescaped(unit.get(1..)?)))
}

/// The NAL units of an Annex B stream, each after its start code.
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at + 3 <= data.len() {
        if data[at..at + 3] == [0, 0, 1] {
            starts.push(at + 3);
            at += 3;
        } else {
            at += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|next| {
            let end = next - 3;
            if end > 0 && data[end - 1] == 0 { end - 1 } else { end }
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .filter_map(move |(start, end)| data.get(start..end))
}

/// The unit's bytes without its emulation prevention bytes (each `3` after two zeros).
fn unescaped(unit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(unit.len());
    let mut zeros = 0;
    for &byte in unit {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// A sequence parameter set's payload (after its NAL header) read up to its picture size.
fn read_sps(payload: &[u8]) -> Option<u32> {
    let mut bits = Bits::new(payload);
    let profile = u8::try_from(bits.read(8)?).ok()?;
    bits.read(16)?;
    bits.golomb()?;
    if HIGH_PROFILES.contains(&profile) {
        skip_formats(&mut bits)?;
    }
    bits.golomb()?;
    skip_picture_order(&mut bits)?;
    bits.golomb()?;
    bits.read(1)?;
    let width = bits.golomb()?.checked_add(1)?;
    let height = bits.golomb()?.checked_add(1)?;
    let frames_only = bits.read(1)?;
    width.checked_mul(height)?.checked_mul(2 - frames_only)
}

/// A high profile's chroma format and bit depths; none past a scaling matrix, which isn't read.
fn skip_formats(bits: &mut Bits<'_>) -> Option<()> {
    if bits.golomb()? == 3 {
        bits.read(1)?;
    }
    bits.golomb()?;
    bits.golomb()?;
    bits.read(1)?;
    (bits.read(1)? == 0).then_some(())
}

/// The picture order count's type and what that type names.
fn skip_picture_order(bits: &mut Bits<'_>) -> Option<()> {
    match bits.golomb()? {
        0 => {
            bits.golomb()?;
        }
        1 => {
            bits.read(1)?;
            bits.golomb()?;
            bits.golomb()?;
            for _ in 0..bits.golomb()? {
                bits.golomb()?;
            }
        }
        _ => {}
    }
    Some(())
}

/// A reader of a payload's bits, the most significant first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    /// The next `count` bits (at most 32) as a number.
    fn read(&mut self, count: u32) -> Option<u32> {
        let mut value: u32 = 0;
        for _ in 0..count {
            let byte = self.data.get(self.at / 8)?;
            let bit = (byte >> (7 - self.at % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.at += 1;
        }
        Some(value)
    }

    /// The next unsigned Exp-Golomb number (a signed one reads the same bits).
    fn golomb(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1_u32 << zeros) - 1 + self.read(zeros)?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes bits, the most significant first, as an encoder writes a sequence parameter set.
    #[derive(Default)]
    struct Writer {
        bits: Vec<u8>,
    }

    impl Writer {
        fn put(&mut self, value: u32, count: u32) -> &mut Self {
            for at in (0..count).rev() {
                self.bits.push(u8::from((value >> at) & 1 == 1));
            }
            self
        }

        fn golomb(&mut self, value: u32) -> &mut Self {
            let coded = value + 1;
            let width = 32 - coded.leading_zeros();
            self.put(0, width - 1).put(coded, width)
        }

        fn bytes(&self) -> Vec<u8> {
            let mut bits = self.bits.clone();
            bits.push(1);
            bits.resize(bits.len().div_ceil(8) * 8, 0);
            bits.chunks(8)
                .map(|byte| byte.iter().fold(0, |out, bit| (out << 1) | bit))
                .collect()
        }
    }

    /// A synthetic keyframe as libwebrtc's encoder sends one: a sequence parameter set (`profile`, `width` by `height`
    /// pixels, `order` its picture order count type), a picture parameter set and an IDR slice.
    pub(crate) fn keyframe(profile: u8, (width, height): (u32, u32), order: u32) -> Vec<u8> {
        let mut sps = Writer::default();
        sps.put(u32::from(profile), 8).put(0xC0, 8).put(42, 8).golomb(0);
        if HIGH_PROFILES.contains(&profile) {
            sps.golomb(1).golomb(0).golomb(0).put(0, 1).put(0, 1);
        }
        sps.golomb(0).golomb(order);
        match order {
            0 => {
                sps.golomb(2);
            }
            1 => {
                sps.put(0, 1).golomb(0).golomb(0).golomb(2).golomb(1).golomb(3);
            }
            _ => {}
        }
        sps.golomb(1).put(0, 1);
        sps.golomb(width.div_ceil(16) - 1)
            .golomb(height.div_ceil(16) - 1)
            .put(1, 1);
        let mut data = vec![0, 0, 0, 1, 0x67];
        data.extend(sps.bytes());
        data.extend([0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80, 0, 0, 1, 0x65, 0x88, 0x84]);
        data
    }

    #[test]
    fn a_keyframe_tells_its_picture_size_in_macroblocks() {
        // 1080p is 120 by 68 macroblocks: a share as Teams sends it
        assert_eq!(macroblocks(&keyframe(66, (1920, 1080), 2)), Some(8160));
        assert_eq!(macroblocks(&keyframe(66, (1280, 720), 0)), Some(3600));
        assert_eq!(macroblocks(&keyframe(100, (640, 360), 1)), Some(920));
        assert_eq!(macroblocks(&keyframe(42, (2560, 1440), 2)), Some(14_400));
        assert_eq!(macroblocks(&keyframe(135, (1920, 1080), 0)), Some(8160));
    }

    #[test]
    fn a_frame_without_a_readable_sequence_parameter_set_tells_no_size() {
        assert_eq!(macroblocks(&[0, 0, 0, 1, 0x65, 0x88]), None);
        assert_eq!(macroblocks(&[]), None);
        assert_eq!(macroblocks(&[0, 0, 1, 0x67]), None, "cut short");
        assert_eq!(
            macroblocks(&[0, 0, 1, 0x67, 66, 0, 0, 3, 0]),
            None,
            "escaped, then cut short"
        );
        // a high profile's scaling matrix is not read
        let mut scaled = Writer::default();
        scaled
            .put(100, 8)
            .put(0, 8)
            .put(40, 8)
            .golomb(0)
            .golomb(1)
            .golomb(0)
            .golomb(0)
            .put(0, 1)
            .put(1, 1);
        let mut data = vec![0, 0, 1, 0x67];
        data.extend(scaled.bytes());
        assert_eq!(macroblocks(&data), None);
        let mut endless = vec![0, 0, 1, 0x67, 66, 0, 40];
        endless.extend([0; 8]);
        assert_eq!(macroblocks(&endless), None, "no Exp-Golomb number ends");
    }

    #[test]
    fn emulation_prevention_bytes_are_taken_out() {
        assert_eq!(unescaped(&[1, 0, 0, 3, 1, 0, 0, 3, 0, 3]), [1, 0, 0, 1, 0, 0, 0, 3]);
    }
}
