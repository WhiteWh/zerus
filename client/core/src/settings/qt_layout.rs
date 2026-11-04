//! Bounded decoding of QSplitter::saveState and QWidget::saveGeometry, as
//! written by Qt 5/6. No Qt runtime or source mutation is involved.
//! Formats: qtbase src/widgets/{widgets/qsplitter,kernel/qwidget}.cpp.
//! https://github.com/qt/qtbase/blob/v6.10.3/src/widgets/widgets/qsplitter.cpp
//! https://github.com/qt/qtbase/blob/v6.10.3/src/widgets/kernel/qwidget.cpp
use super::{geometry::Geometry, QtValue};

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.0.split_at_checked(N)?;
        self.0 = tail;
        head.try_into().ok()
    }
    fn int(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take()?))
    }
    fn byte(&mut self) -> Option<u8> {
        Some(self.take::<1>()?[0])
    }
    fn boolean(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn rect(&mut self) -> Option<(i32, i32, i32, i32)> {
        Some((self.int()?, self.int()?, self.int()?, self.int()?))
    }
}

fn bytes(value: &QtValue) -> Option<&[u8]> {
    match value {
        QtValue::Bytes(bytes) if bytes.len() <= 128 => Some(bytes),
        _ => None,
    }
}

pub(super) fn splitter(value: &QtValue, orientation: i32) -> Option<u32> {
    let mut r = Reader(bytes(value)?);
    if r.int()? != 0xff {
        return None;
    }
    let version = r.int()?;
    if !(0..=1).contains(&version) || r.int()? != 2 {
        return None;
    }
    let first = r.int()?;
    let second = r.int()?;
    r.boolean()?;
    let handle = r.int()?;
    r.boolean()?;
    if r.int()? != orientation {
        return None;
    }
    if version == 1 {
        r.boolean()?;
    }
    (r.0.is_empty()
        && (1..=65536).contains(&first)
        && (1..=65536).contains(&second)
        && (-1..=64).contains(&handle))
    .then_some(first as u32)
}

pub(super) fn geometry(value: &QtValue) -> Option<Geometry> {
    let mut r = Reader(bytes(value)?);
    if r.int()? != 0x01d9d0cb {
        return None;
    }
    let version = u16::from_be_bytes(r.take()?);
    if !(1..=3).contains(&version) || u16::from_be_bytes(r.take()?) != 0 {
        return None;
    }
    let frame = r.rect()?;
    let normal = r.rect()?;
    let screen = r.int()?;
    let maximized = match r.byte()? {
        0 => false,
        2 => true,
        _ => return None,
    };
    let fullscreen = match r.byte()? {
        0 => false,
        4 => true,
        _ => return None,
    };
    if version >= 2 && !(1..=65536).contains(&r.int()?) {
        return None;
    }
    let current = if version == 3 { r.rect()? } else { frame };
    if !r.0.is_empty() || !(-1..=256).contains(&screen) {
        return None;
    }
    let (x, y, right, bottom) = if normal.2 >= normal.0 && normal.3 >= normal.1 {
        normal
    } else {
        current
    };
    let result = Geometry {
        version: 1,
        x,
        y,
        width: u32::try_from((right as i64) - (x as i64) + 1).ok()?,
        height: u32::try_from((bottom as i64) - (y as i64) + 1).ok()?,
        maximized,
        fullscreen,
    };
    result.valid().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ints(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }
    fn split() -> Vec<u8> {
        let mut bytes = ints(&[0xff, 1, 2, 340, 800]);
        bytes.push(0);
        bytes.extend(ints(&[8]));
        bytes.push(1);
        bytes.extend(ints(&[2]));
        bytes.push(1);
        bytes
    }
    #[test]
    fn splitter_requires_the_matching_two_pane_orientation_and_complete_format() {
        let bytes = split();
        assert_eq!(splitter(&QtValue::Bytes(bytes.clone()), 2), Some(340));
        assert!(splitter(&QtValue::Bytes(bytes.clone()), 1).is_none());
        for n in 0..bytes.len() {
            assert!(splitter(&QtValue::Bytes(bytes[..n].to_vec()), 2).is_none());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(splitter(&QtValue::Bytes(trailing), 2).is_none());
        let mut huge = bytes;
        huge[8..12].copy_from_slice(&i32::MAX.to_be_bytes());
        assert!(splitter(&QtValue::Bytes(huge), 2).is_none());
    }
    #[test]
    fn geometry_preserves_normal_bounds_while_maximized_and_checks_versions() {
        for version in 1u16..=3 {
            let mut bytes = ints(&[0x01d9d0cb]);
            bytes.extend(version.to_be_bytes());
            bytes.extend(0u16.to_be_bytes());
            bytes.extend(ints(&[0, 0, 1919, 1079, -1400, 50, -201, 849, 1]));
            bytes.extend([2, 0]);
            if version >= 2 {
                bytes.extend(ints(&[1920]));
            }
            if version == 3 {
                bytes.extend(ints(&[0, 0, 1919, 1079]));
            }
            let value = geometry(&QtValue::Bytes(bytes.clone())).unwrap();
            assert_eq!(
                (value.x, value.y, value.width, value.height, value.maximized),
                (-1400, 50, 1200, 800, true)
            );
            for n in 0..bytes.len() {
                assert!(geometry(&QtValue::Bytes(bytes[..n].to_vec())).is_none());
            }
            bytes[5] = 4;
            assert!(geometry(&QtValue::Bytes(bytes)).is_none());
        }
    }
}
