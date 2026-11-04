//! Bounded intrinsic image geometry shared by draft and Activity previews.
use serde::{Deserialize, Serialize};

pub const HEADER_BYTES: usize = 256 * 1024;
pub const MAX_PIXELS: u64 = 64_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// Header dimensions in the same orientation the browser displays. No image
/// decoding or filesystem I/O; all offsets remain inside a bounded header.
pub fn dimensions(bytes: &[u8]) -> Option<Size> {
    let head = &bytes[..bytes.len().min(HEADER_BYTES)];
    let (mut width, mut height) = raw_size(head)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    if head.starts_with(&[0xff, 0xd8]) && orientation(head).is_some_and(|n| (5..=8).contains(&n)) {
        std::mem::swap(&mut width, &mut height);
    }
    Some(Size { width, height })
}

fn orientation(head: &[u8]) -> Option<u16> {
    let mut at = 2usize;
    while at + 4 <= head.len() {
        if head[at] != 0xff {
            return None;
        }
        let marker = head[at + 1];
        if marker == 0xff {
            at += 1;
            continue;
        }
        if matches!(marker, 0xd8 | 0x01 | 0xd0..=0xd7) {
            at += 2;
            continue;
        }
        if matches!(marker, 0xd9 | 0xda) {
            return None;
        }
        let len = usize::from(u16::from_be_bytes(
            head.get(at + 2..at + 4)?.try_into().ok()?,
        ));
        if len < 2 {
            return None;
        }
        let segment = head.get(at + 4..at + 2 + len)?;
        if marker == 0xe1 && segment.starts_with(b"Exif\0\0") {
            return tiff_orientation(&segment[6..]);
        }
        at = at.checked_add(2 + len)?;
    }
    None
}
fn tiff_orientation(data: &[u8]) -> Option<u16> {
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| {
        let b = data.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| {
        let b = data.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let at = usize::try_from(u32_at(4)?).ok()?;
    let count = usize::from(u16_at(at)?);
    if count > 4096 {
        return None;
    }
    for index in 0..count {
        let entry = at.checked_add(2 + index * 12)?;
        if u16_at(entry)? == 0x112 && u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1 {
            let n = u16_at(entry + 8)?;
            return (1..=8).contains(&n).then_some(n);
        }
    }
    None
}
fn raw_size(head: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(head.get(at..at + 2)?.try_into().ok()?));
    let le16 = |at: usize| Some(u16::from_le_bytes(head.get(at..at + 2)?.try_into().ok()?));
    let le24 = |at: usize| {
        let b = head.get(at..at + 3)?;
        Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
    };
    let le32 = |at: usize| Some(u32::from_le_bytes(head.get(at..at + 4)?.try_into().ok()?));
    let be32 = |at: usize| Some(u32::from_be_bytes(head.get(at..at + 4)?.try_into().ok()?));
    if head.starts_with(b"\x89PNG\r\n\x1a\n") && head.get(12..16) == Some(b"IHDR") {
        return Some((be32(16)?, be32(20)?));
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some((u32::from(le16(6)?), u32::from(le16(8)?)));
    }
    if head.starts_with(b"BM") {
        let width = le32(18)? as i32;
        let height = le32(22)? as i32;
        return Some((width.unsigned_abs(), height.unsigned_abs()));
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return match head.get(12..16)? {
            b"VP8 " => Some((u32::from(le16(26)? & 0x3fff), u32::from(le16(28)? & 0x3fff))),
            b"VP8L" => {
                let bits = le32(21)?;
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            _ => None,
        };
    }
    if head.starts_with(&[0xff, 0xd8]) {
        // Walk the segments to the first frame header.
        let mut at = 2;
        loop {
            while head.get(at) == Some(&0xff) && head.get(at + 1) == Some(&0xff) {
                at += 1;
            }
            if head.get(at) != Some(&0xff) {
                return None;
            }
            let marker = *head.get(at + 1)?;
            match marker {
                0xd8 | 0x01 | 0xd0..=0xd7 => at += 2,
                0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                    return Some((u32::from(be16(at + 7)?), u32::from(be16(at + 5)?)));
                }
                0xd9 | 0xda => return None,
                _ => {
                    let len = usize::from(be16(at + 2)?);
                    if len < 2 {
                        return None;
                    }
                    at = at.checked_add(2 + len)?;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b
    }
    fn jpeg(orientation: u16, little: bool) -> Vec<u8> {
        let mut exif = b"Exif\0\0".to_vec();
        if little {
            exif.extend(b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0");
            exif.extend(orientation.to_le_bytes());
        } else {
            exif.extend(b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
            exif.extend(orientation.to_be_bytes());
        }
        exif.extend([0, 0]);
        let mut b = vec![0xff, 0xd8, 0xff, 0xe1];
        b.extend(((exif.len() + 2) as u16).to_be_bytes());
        b.extend(exif);
        b.extend([0xff, 0xc0, 0, 11, 8, 3, 32, 4, 176, 1, 1, 0x11, 0]);
        b
    }
    #[test]
    fn reserves_all_real_send_shapes() {
        for (width, height) in [(2560, 720), (720, 2560), (80, 50), (2400, 1200)] {
            assert_eq!(
                dimensions(&png(width, height)),
                Some(Size { width, height })
            );
        }
    }
    #[test]
    fn exif_orientation_is_applied_for_both_byte_orders() {
        for little in [false, true] {
            for n in 1..=8 {
                let (width, height) = if n >= 5 { (800, 1200) } else { (1200, 800) };
                assert_eq!(dimensions(&jpeg(n, little)), Some(Size { width, height }));
            }
        }
    }
    #[test]
    fn broken_headers_and_pixel_bombs_are_bounded() {
        assert_eq!(dimensions(&png(0, 100)), None);
        assert_eq!(dimensions(&png(100_000, 100_000)), None);
        assert_eq!(dimensions(&[0xff, 0xd8, 0xff, 0xe1, 0, 0]), None);
        let image = jpeg(6, true);
        for end in 0..image.len() {
            let _ = dimensions(&image[..end]);
        }
        let mut invalid = jpeg(6, true);
        invalid[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            dimensions(&invalid),
            Some(Size {
                width: 1200,
                height: 800
            })
        );
    }
}
