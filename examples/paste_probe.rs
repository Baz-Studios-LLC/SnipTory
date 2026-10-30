//! Local, disposable target for exercising focus restoration and image paste.
//! No user documents are opened or modified. Run with cargo run --example paste_probe.
use std::{cell::RefCell, mem::size_of};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{DataExchange::*, LibraryLoader::*, Memory::*},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::*,
};

thread_local! { static PASTED: RefCell<Option<(BITMAPINFOHEADER, Vec<u8>)>> = const { RefCell::new(None) }; }

fn main() -> Result<()> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module.into(),
            lpszClassName: w!("SnipTory.PasteProbe"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("SnipTory.PasteProbe"),
            w!("SnipTory paste verification — waiting"),
            WS_OVERLAPPEDWINDOW,
            180,
            120,
            880,
            560,
            None,
            None,
            Some(module.into()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let background = CreateSolidBrush(COLORREF(0x00191411));
                FillRect(dc, &client, background);
                let _ = DeleteObject(background.into());
                let card = RECT {
                    left: 30,
                    top: 60,
                    right: 400,
                    bottom: 310,
                };
                let blue = CreateSolidBrush(COLORREF(0x00552f16));
                FillRect(dc, &card, blue);
                let _ = DeleteObject(blue.into());
                let font = CreateFontW(
                    -24,
                    0,
                    0,
                    0,
                    600,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                );
                let old = SelectObject(dc, font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00fff5ee));
                let mut text = "Capture. Recall. Paste.".encode_utf16().collect::<Vec<_>>();
                let mut text_rect = RECT {
                    left: 55,
                    top: 100,
                    right: 390,
                    bottom: 160,
                };
                DrawTextW(dc, &mut text, &mut text_rect, DT_LEFT);
                let mut text = "SnipTory · Local test card"
                    .encode_utf16()
                    .collect::<Vec<_>>();
                text_rect.top = 175;
                DrawTextW(dc, &mut text, &mut text_rect, DT_LEFT);
                let mut label = "SOURCE REGION".encode_utf16().collect::<Vec<_>>();
                let mut label_rect = RECT {
                    left: 30,
                    top: 20,
                    right: 400,
                    bottom: 55,
                };
                DrawTextW(dc, &mut label, &mut label_rect, DT_LEFT);
                label = "PASTE RESULT".encode_utf16().collect();
                label_rect.left = 440;
                label_rect.right = 820;
                DrawTextW(dc, &mut label, &mut label_rect, DT_LEFT);
                PASTED.with(|pasted| {
                    if let Some((header, bytes)) = pasted.borrow().as_ref() {
                        let info = BITMAPINFO {
                            bmiHeader: *header,
                            ..Default::default()
                        };
                        let w = header.biWidth;
                        let h = header.biHeight.abs();
                        let scale = (370.0 / w as f64).min(350.0 / h as f64).min(1.0);
                        StretchDIBits(
                            dc,
                            440,
                            60,
                            (w as f64 * scale) as i32,
                            (h as f64 * scale) as i32,
                            0,
                            0,
                            w,
                            h,
                            Some(bytes.as_ptr().cast()),
                            &info,
                            DIB_RGB_COLORS,
                            SRCCOPY,
                        );
                    }
                });
                SelectObject(dc, old);
                let _ = DeleteObject(font.into());
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_KEYDOWN if wparam.0 == 0x56 && GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 => {
                SendMessageW(hwnd, WM_PASTE, None, None);
                return LRESULT(0);
            }
            WM_PASTE => {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if let Ok(handle) = GetClipboardData(8) {
                        let pointer = GlobalLock(HGLOBAL(handle.0)).cast::<u8>();
                        if !pointer.is_null() {
                            let header =
                                std::ptr::read_unaligned(pointer.cast::<BITMAPINFOHEADER>());
                            if header.biBitCount == 32 && header.biWidth > 0 && header.biHeight != 0
                            {
                                let length = header.biWidth as usize
                                    * header.biHeight.unsigned_abs() as usize
                                    * 4;
                                if length <= 512 * 1024 * 1024
                                    && size_of::<BITMAPINFOHEADER>() + length
                                        <= GlobalSize(HGLOBAL(handle.0))
                                {
                                    let bytes = std::slice::from_raw_parts(
                                        pointer.add(header.biSize as usize),
                                        length,
                                    )
                                    .to_vec();
                                    let _ = SetWindowTextW(
                                        hwnd,
                                        &HSTRING::from(format!(
                                            "SnipTory paste verification — received {} × {} image",
                                            header.biWidth,
                                            header.biHeight.abs()
                                        )),
                                    );
                                    PASTED.with(|p| *p.borrow_mut() = Some((header, bytes)));
                                    let _ = InvalidateRect(Some(hwnd), None, false);
                                }
                            }
                            let _ = GlobalUnlock(HGLOBAL(handle.0));
                        }
                    }
                    let _ = CloseClipboard();
                }
                return LRESULT(0);
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}
