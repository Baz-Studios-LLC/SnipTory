//! Physical-pixel desktop capture. Every GDI object is restored before deletion.
use anyhow::{Result, bail};
use image::RgbaImage;
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

pub struct Desktop {
    pub left: i32,
    pub top: i32,
    pub pixels: RgbaImage,
    pub dimmed: Vec<u8>,
    pub bgra: Vec<u8>,
}

pub fn desktop() -> Result<Desktop> {
    // SAFETY: desktop DC and all GDI handles are local to this call. The DIB
    // storage is sized from checked physical dimensions and copied before release.
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        if width <= 0 || height <= 0 || bytes.is_none_or(|n| n > 512 * 1024 * 1024) {
            bail!("Desktop is too large to capture safely");
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            bail!("Could not access the desktop");
        }
        let memory = CreateCompatibleDC(Some(screen));
        if memory.is_invalid() {
            ReleaseDC(None, screen);
            bail!("Could not create capture DC");
        }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            bail!("Could not allocate capture bitmap");
        }
        let old = SelectObject(memory, bitmap.into());
        let capture = BitBlt(
            memory,
            0,
            0,
            width,
            height,
            Some(screen),
            left,
            top,
            SRCCOPY | CAPTUREBLT,
        );
        SelectObject(memory, old);
        let mut bgra = vec![0u8; bytes.unwrap()];
        let mut info = bitmap_info(width, height);
        let lines = if capture.is_ok() {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        capture?;
        if lines != height {
            bail!("Windows could not read the captured desktop");
        }
        let mut rgba = bgra.clone();
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        let mut dimmed = bgra.clone();
        for p in dimmed.chunks_exact_mut(4) {
            p[0] = p[0] / 2 + 8;
            p[1] = p[1] / 2 + 6;
            p[2] = p[2] / 2 + 7;
        }
        Ok(Desktop {
            left,
            top,
            pixels: RgbaImage::from_raw(width as u32, height as u32, rgba).unwrap(),
            bgra,
            dimmed,
        })
    }
}

pub fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn crop(desktop: &Desktop, a: POINT, b: POINT) -> Option<RgbaImage> {
    let w = desktop.pixels.width() as i32;
    let h = desktop.pixels.height() as i32;
    let left = a.x.min(b.x).clamp(0, w);
    let top = a.y.min(b.y).clamp(0, h);
    let right = a.x.max(b.x).clamp(0, w);
    let bottom = a.y.max(b.y).clamp(0, h);
    if right <= left || bottom <= top {
        return None;
    }
    Some(
        image::imageops::crop_imm(
            &desktop.pixels,
            left as u32,
            top as u32,
            (right - left) as u32,
            (bottom - top) as u32,
        )
        .to_image(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reverse_drag_clips_to_physical_desktop() {
        let desktop = Desktop {
            left: -1920,
            top: -200,
            pixels: RgbaImage::new(200, 100),
            dimmed: vec![],
            bgra: vec![],
        };
        let image = crop(&desktop, POINT { x: 220, y: 90 }, POINT { x: -5, y: 10 }).unwrap();
        assert_eq!(image.dimensions(), (200, 80));
        assert!(crop(&desktop, POINT { x: 10, y: 10 }, POINT { x: 10, y: 20 }).is_none());
    }
}
