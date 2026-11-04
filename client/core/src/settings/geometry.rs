//! The main window's logical desktop coordinates. Qt is read only at import;
//! subsequent moves use the ordinary Next settings writer.
use serde::{Deserialize, Serialize};

pub const KEY: &str = "client/windowGeometry";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub version: u8,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl Geometry {
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 512 {
            return None;
        }
        let value: Self = serde_json::from_str(text).ok()?;
        value.valid().then_some(value)
    }

    pub fn valid(&self) -> bool {
        self.version == 1
            && self.x.unsigned_abs() <= 1_000_000
            && self.y.unsigned_abs() <= 1_000_000
            && (1..=65536).contains(&self.width)
            && (1..=65536).contains(&self.height)
    }

    /// Keep the title bar and normal bounds on an available monitor, including
    /// after a monitor is unplugged or its scale changes. Small displays may
    /// be smaller than the ordinary minimum window size.
    pub fn fit(mut self, screen: (i32, i32, u32, u32)) -> Self {
        let (x, y, width, height) = screen;
        self.width = self.width.max(900).min(width.max(1));
        self.height = self.height.max(560).min(height.max(1));
        self.x = (self.x as i64).clamp(x as i64, x as i64 + width.saturating_sub(self.width) as i64)
            as i32;
        self.y = (self.y as i64).clamp(
            y as i64,
            y as i64 + height.saturating_sub(self.height) as i64,
        ) as i32;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removed_display_and_changed_scale_stay_visible() {
        let old = Geometry {
            version: 1,
            x: -3000,
            y: -40,
            width: 1600,
            height: 900,
            maximized: true,
            fullscreen: false,
        };
        let fitted = old.fit((0, 20, 1280, 720));
        assert_eq!(
            (fitted.x, fitted.y, fitted.width, fitted.height),
            (0, 20, 1280, 720)
        );
        assert!(fitted.maximized);
        assert_eq!(old.fit((-1920, 0, 1920, 1080)).x, -1920);
    }
    #[test]
    fn unsupported_and_malformed_next_geometry_is_not_restored() {
        let value = Geometry {
            version: 1,
            x: 20,
            y: 30,
            width: 1200,
            height: 800,
            maximized: false,
            fullscreen: false,
        };
        assert_eq!(
            Geometry::parse(&serde_json::to_string(&value).unwrap()),
            Some(value)
        );
        assert!(Geometry::parse("null").is_none());
        assert!(Geometry::parse(
            &serde_json::to_string(&Geometry {
                version: 2,
                ..value
            })
            .unwrap()
        )
        .is_none());
        assert!(Geometry::parse(&" ".repeat(513)).is_none());
    }
}
