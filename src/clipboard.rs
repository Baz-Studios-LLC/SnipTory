//! Clipboard ownership transfers to Windows only after SetClipboardData succeeds.
use anyhow::{Result, bail};
use image::RgbaImage;
use std::{mem::size_of, time::Duration};
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{DataExchange::*, Memory::*},
    UI::Input::KeyboardAndMouse::*,
};

pub fn copy(owner: HWND, pixels: &RgbaImage) -> Result<()> {
    // SAFETY: CF_DIB is a movable, top-down 32-bit bitmap. Unlock before transfer;
    // free on failure, and never free after Windows takes ownership.
    unsafe {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: pixels.width() as i32,
            biHeight: -(pixels.height() as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let allocation = GlobalAlloc(
            GMEM_MOVEABLE,
            size_of::<BITMAPINFOHEADER>() + pixels.as_raw().len(),
        )?;
        let pointer = GlobalLock(allocation).cast::<u8>();
        if pointer.is_null() {
            let _ = GlobalFree(Some(allocation));
            bail!("Could not allocate clipboard image");
        }
        std::ptr::copy_nonoverlapping(
            (&header as *const BITMAPINFOHEADER).cast::<u8>(),
            pointer,
            size_of::<BITMAPINFOHEADER>(),
        );
        let bytes = std::slice::from_raw_parts_mut(
            pointer.add(size_of::<BITMAPINFOHEADER>()),
            pixels.as_raw().len(),
        );
        for (out, src) in bytes
            .chunks_exact_mut(4)
            .zip(pixels.as_raw().chunks_exact(4))
        {
            out.copy_from_slice(&[src[2], src[1], src[0], 255]);
        }
        let _ = GlobalUnlock(allocation);
        let mut opened = false;
        for _ in 0..12 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if !opened {
            let _ = GlobalFree(Some(allocation));
            bail!("Clipboard is busy. Capture saved; try again from history.");
        }
        let result = EmptyClipboard()
            .and_then(|_| SetClipboardData(8, Some(HANDLE(allocation.0))).map(|_| ()));
        let _ = CloseClipboard();
        if result.is_err() {
            let _ = GlobalFree(Some(allocation));
        }
        result?;
        Ok(())
    }
}

pub fn modifiers_released() -> bool {
    // SAFETY: querying key state has no pointers or ownership requirements.
    unsafe {
        [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .all(|key| GetAsyncKeyState(key.0 as i32) >= 0)
    }
}

pub fn copy_gif(owner: HWND, path: &std::path::Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::DROPFILES;
    // CF_HDROP preserves the original animated file. Do not add a flattened DIB
    // fallback which could make a destination silently paste a still image.
    unsafe {
        let path = path.canonicalize()?;
        let names = path
            .as_os_str()
            .encode_wide()
            .chain([0, 0])
            .collect::<Vec<_>>();
        let header = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            fWide: true.into(),
            ..Default::default()
        };
        let allocation = GlobalAlloc(GMEM_MOVEABLE, size_of::<DROPFILES>() + names.len() * 2)?;
        let pointer = GlobalLock(allocation).cast::<u8>();
        if pointer.is_null() {
            let _ = GlobalFree(Some(allocation));
            bail!("Could not allocate GIF clipboard data");
        }
        std::ptr::copy_nonoverlapping(
            (&header as *const DROPFILES).cast::<u8>(),
            pointer,
            size_of::<DROPFILES>(),
        );
        std::ptr::copy_nonoverlapping(
            names.as_ptr().cast::<u8>(),
            pointer.add(size_of::<DROPFILES>()),
            names.len() * 2,
        );
        let _ = GlobalUnlock(allocation);
        let mut opened = false;
        for _ in 0..12 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if !opened {
            let _ = GlobalFree(Some(allocation));
            bail!("Clipboard is busy. GIF saved in History.");
        }
        let result = EmptyClipboard()
            .and_then(|_| SetClipboardData(15, Some(HANDLE(allocation.0))).map(|_| ()));
        let _ = CloseClipboard();
        if result.is_err() {
            let _ = GlobalFree(Some(allocation));
        }
        result?;
    }
    Ok(())
}

pub fn paste() -> Result<()> {
    // SAFETY: all four INPUTs are fully initialized and sized for this process.
    unsafe {
        let key = |vk, flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        };
        let inputs = [
            key(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
            key(VIRTUAL_KEY(0x56), KEYBD_EVENT_FLAGS(0)),
            key(VIRTUAL_KEY(0x56), KEYEVENTF_KEYUP),
            key(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        if SendInput(&inputs, size_of::<INPUT>() as i32) != inputs.len() as u32 {
            bail!("Windows blocked paste. Image remains on the clipboard.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{Win32::UI::WindowsAndMessaging::*, core::w};

    #[test]
    #[ignore = "requires an unlocked Windows desktop and replaces the clipboard"]
    fn desktop_pixels_roundtrip_through_windows_clipboard() -> Result<()> {
        let desktop = crate::capture::desktop()?;
        let pixels =
            crate::capture::crop(&desktop, POINT { x: 0, y: 0 }, POINT { x: 32, y: 16 }).unwrap();
        unsafe {
            let owner = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("SnipTory clipboard verification"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )?;
            copy(owner, &pixels)?;
            OpenClipboard(Some(owner))?;
            let handle = GetClipboardData(8)?;
            let pointer = GlobalLock(HGLOBAL(handle.0)).cast::<u8>();
            assert!(!pointer.is_null());
            let header = std::ptr::read_unaligned(pointer.cast::<BITMAPINFOHEADER>());
            assert_eq!(
                (header.biWidth, header.biHeight, header.biBitCount),
                (32, -16, 32)
            );
            let bytes = std::slice::from_raw_parts(
                pointer.add(size_of::<BITMAPINFOHEADER>()),
                pixels.as_raw().len(),
            );
            for (dib, pixel) in bytes.chunks_exact(4).zip(pixels.as_raw().chunks_exact(4)) {
                assert_eq!(dib, &[pixel[2], pixel[1], pixel[0], 255]);
            }
            let _ = GlobalUnlock(HGLOBAL(handle.0));
            CloseClipboard()?;
            DestroyWindow(owner)?;
        }
        Ok(())
    }
}
