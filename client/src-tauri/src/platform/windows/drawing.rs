//! Draw the existing shared Qt counter tile with Windows' native font rasterizer.
use super::wide;
use std::io;
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
use zerus_client_core::{
    menubar::Face,
    trayicon::{self, Rect, Rgb, Step},
};

pub struct Icon {
    pub handle: HICON,
    pub rgba: Vec<u8>,
    pub size: u32,
}
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            DestroyIcon(self.handle);
        }
    }
}
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    prior: HGDIOBJ,
    pixels: *mut u8,
    size: i32,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.prior);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}
impl Surface {
    fn new(size: i32) -> io::Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = size;
            info.bmiHeader.biHeight = -size;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut pixels = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut pixels,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() {
                DeleteDC(dc);
                return Err(io::Error::last_os_error());
            }
            let prior = SelectObject(dc, bitmap);
            std::ptr::write_bytes(pixels, 0, (size * size * 4) as usize);
            SetBkMode(dc, TRANSPARENT as i32);
            Ok(Self {
                dc,
                bitmap,
                prior,
                pixels: pixels.cast(),
                size,
            })
        }
    }
}
struct Canvas {
    color: Surface,
    coverage: Surface,
}
impl std::ops::Deref for Canvas {
    type Target = Surface;
    fn deref(&self) -> &Surface {
        &self.color
    }
}
impl Canvas {
    fn new(size: i32) -> io::Result<Self> {
        Ok(Self {
            color: Surface::new(size)?,
            coverage: Surface::new(size)?,
        })
    }
    fn color(rgb: Rgb) -> u32 {
        u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16)
    }
    fn fill(&self, rgb: Rgb, draw: impl Fn(HDC)) {
        unsafe {
            // A separate coverage bitmap preserves opaque black text/marks.
            // GDI clears DIB alpha, so RGB alone cannot identify transparency.
            for (dc, color) in [(self.dc, Self::color(rgb)), (self.coverage.dc, 0x00ff_ffff)] {
                let brush = CreateSolidBrush(color);
                let old = SelectObject(dc, brush);
                let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                draw(dc);
                SelectObject(dc, pen);
                SelectObject(dc, old);
                DeleteObject(brush);
            }
        }
    }
    fn rounded(&self, r: Rect, radius: f64, rgb: Rgb) {
        self.fill(rgb, |dc| unsafe {
            RoundRect(
                dc,
                r.x.round() as i32,
                r.y.round() as i32,
                (r.x + r.w).round() as i32,
                (r.y + r.h).round() as i32,
                (radius * 2.).round() as i32,
                (radius * 2.).round() as i32,
            );
        });
    }
    fn text(&self, text: &str, rect: Rect, color: Rgb, pixels: f64) {
        let text = wide(text);
        unsafe {
            let mut size = pixels.round() as i32;
            loop {
                let font = CreateFontW(
                    -size,
                    0,
                    0,
                    0,
                    FW_BOLD as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    ANTIALIASED_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    wide("Segoe UI").as_ptr(),
                );
                let old = SelectObject(self.dc, font);
                let mut extent = SIZE::default();
                GetTextExtentPoint32W(self.dc, text.as_ptr(), text.len() as i32 - 1, &mut extent);
                if f64::from(extent.cx) <= rect.w || size <= 5 {
                    SetTextColor(self.dc, Self::color(color));
                    let mut area = RECT {
                        left: rect.x.round() as i32,
                        top: rect.y.round() as i32,
                        right: (rect.x + rect.w).round() as i32,
                        bottom: (rect.y + rect.h).round() as i32,
                    };
                    DrawTextW(
                        self.dc,
                        text.as_ptr(),
                        text.len() as i32 - 1,
                        &mut area,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    );
                    SelectObject(self.dc, old);
                    DeleteObject(font);
                    break;
                }
                SelectObject(self.dc, old);
                DeleteObject(font);
                size -= 1;
            }
        }
    }
    fn icon(self) -> io::Result<Icon> {
        unsafe {
            GdiFlush();
            let pixels =
                std::slice::from_raw_parts_mut(self.pixels, (self.size * self.size * 4) as usize);
            let coverage = std::slice::from_raw_parts(self.coverage.pixels, pixels.len());
            for (pixel, mask) in pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(coverage.as_chunks::<4>().0)
            {
                pixel[3] = if mask[..3].iter().any(|v| *v != 0) {
                    255
                } else {
                    0
                };
            }
            let mut rgba = pixels.to_vec();
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let mask_bits = vec![0u8; (((self.size + 15) / 16) * 2 * self.size) as usize];
            let mask = CreateBitmap(self.size, self.size, 1, 1, mask_bits.as_ptr().cast());
            let info = ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: self.bitmap,
            };
            let handle = CreateIconIndirect(&info);
            DeleteObject(mask);
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Icon {
                    handle,
                    rgba,
                    size: self.size as u32,
                })
            }
        }
    }
}

pub fn tile(face: &Face, size: i32, foreground: Rgb) -> io::Result<Icon> {
    let size = size.clamp(16, 128);
    let canvas = Canvas::new(size)?;
    let tile = trayicon::tile(face, size as f64, foreground);
    canvas.rounded(tile.upper, tile.radius, tile.surface);
    canvas.rounded(tile.lower, tile.radius, tile.lower_fill);
    if let Some(rect) = tile.mark {
        let point = |x: f64, y: f64| POINT {
            x: (rect.x + x * rect.w / 100.).round() as i32,
            y: (rect.y + y * rect.h / 100.).round() as i32,
        };
        canvas.fill(foreground, |dc| unsafe {
            BeginPath(dc);
            for step in trayicon::mark_path() {
                match step {
                    Step::Move(x, y) => {
                        let p = point(x, y);
                        MoveToEx(dc, p.x, p.y, std::ptr::null_mut());
                    }
                    Step::Curve { c1, c2, to } => {
                        let points = [point(c1.0, c1.1), point(c2.0, c2.1), point(to.0, to.1)];
                        PolyBezierTo(dc, points.as_ptr(), 3);
                    }
                    Step::Close => {
                        CloseFigure(dc);
                    }
                }
            }
            EndPath(dc);
            FillPath(dc);
        });
    }
    for text in [tile.total, tile.attention] {
        canvas.text(&text.text, text.rect, text.color, tile.font_pixels);
    }
    if let Some((x, y, r)) = tile.dot {
        canvas.fill(trayicon::AMBER, |dc| unsafe {
            Ellipse(
                dc,
                (x - r).round() as i32,
                (y - r).round() as i32,
                (x + r).round() as i32,
                (y + r).round() as i32,
            );
        });
    }
    canvas.icon()
}
pub fn badge(text: &str) -> io::Result<Icon> {
    let canvas = Canvas::new(32)?;
    canvas.fill(Rgb::new(210, 35, 35), |dc| unsafe {
        Ellipse(dc, 0, 0, 32, 32);
    });
    canvas.text(
        text,
        Rect {
            x: 1.,
            y: 1.,
            w: 30.,
            h: 30.,
        },
        Rgb::new(255, 255, 255),
        22.,
    );
    canvas.icon()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn black_shapes_remain_opaque_on_a_transparent_canvas() {
        let canvas = Canvas::new(16).unwrap();
        canvas.rounded(
            Rect {
                x: 2.,
                y: 2.,
                w: 12.,
                h: 12.,
            },
            2.,
            Rgb::new(0, 0, 0),
        );
        let icon = canvas.icon().unwrap();
        let center = (8 * 16 + 8) * 4;
        assert_eq!(&icon.rgba[center..center + 4], &[0, 0, 0, 255]);
        assert_eq!(icon.rgba[3], 0);
    }
    #[test]
    fn counter_and_overlay_icons_have_owned_native_handles_and_pixels() {
        for face in [
            Face::counts(23, 4, false),
            Face::counts(0, 0, true),
            Face::error(),
        ] {
            let icon = tile(&face, 32, trayicon::foreground(true, None)).unwrap();
            assert!(!icon.handle.is_null());
            assert_eq!(icon.rgba.len(), 32 * 32 * 4);
            assert!(icon.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0));
        }
        let icon = badge("99+").unwrap();
        assert_eq!(icon.rgba.len(), 32 * 32 * 4);
    }
}
