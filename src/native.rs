//! Win32 owns the tray, global shortcuts and physical-pixel region selector on
//! one blocking message-loop thread. Slint owns the gallery on the main thread.
use crate::{HistoryWindow, StoreCommand, capture, clipboard, status};
use anyhow::{Result, bail};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
        mpsc,
    },
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{LibraryLoader::*, Threading::*},
        UI::{
            Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::*,
};

const WAKE: u32 = WM_APP + 1;
const TRAY: u32 = WM_APP + 2;
pub enum Command {
    Capture,
    History,
    CopyPaste(image::RgbaImage),
    Notify(String),
    ApplyUpdate(velopack::UpdateManager, velopack::VelopackAsset),
    UpdateFailed,
    Options,
    Updates,
    Restart,
    Quit,
}
#[derive(Clone)]
pub struct Bridge {
    tx: mpsc::Sender<Command>,
    hwnd: Arc<AtomicIsize>,
}
impl Bridge {
    pub fn send(&self, command: Command) {
        if self.tx.send(command).is_ok() {
            // SAFETY: handle is published after creation; posting never dereferences it.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.hwnd.load(Ordering::Acquire) as *mut _)),
                    WAKE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
    pub fn notify(&self, text: &str) {
        self.send(Command::Notify(text.into()));
    }
}

pub struct Instance(HANDLE);
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub fn single_instance() -> Result<Option<Instance>> {
    // SAFETY: named kernel mutex is held for the lifetime of Instance.
    unsafe {
        let handle = CreateMutexW(None, false, w!("Local\\SnipTory.Instance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(Instance(handle)))
    }
}
pub fn fatal(message: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("SnipTory"),
            MB_OK | MB_ICONERROR,
        );
    }
}

struct Selection {
    desktop: capture::Desktop,
    anchor: Option<POINT>,
    current: POINT,
}
struct State {
    owner: HWND,
    overlay: HWND,
    icon: HICON,
    ui: slint::Weak<HistoryWindow>,
    store: mpsc::Sender<StoreCommand>,
    commands: mpsc::Receiver<Command>,
    selection: Option<Selection>,
    target: HWND,
    pending_paste: Option<(Instant, HWND)>,
    foreground_hook: HWINEVENTHOOK,
    taskbar_created: u32,
    restarting: bool,
    preferences: crate::settings::Shared,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

pub fn start(
    ui: slint::Weak<HistoryWindow>,
    store: mpsc::Sender<StoreCommand>,
    preferences: crate::settings::Shared,
) -> Result<Bridge> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let hwnd = Arc::new(AtomicIsize::new(0));
    let published = hwnd.clone();
    std::thread::Builder::new().name("sniptory-win32".into()).spawn(move || {
        // SAFETY: all windows, hotkeys, GDI resources and the tray icon below
        // belong to this thread and are destroyed before it exits.
        unsafe {
            let initialized: Result<HWND> = (|| {
                let instance = GetModuleHandleW(None)?;
                let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance.into(),
                    lpszClassName: w!("SnipTory.Native"), hCursor: LoadCursorW(None, IDC_CROSS)?, ..Default::default() };
                if RegisterClassW(&class) == 0 { bail!("Register window class: {}", Error::from_thread()); }
                let owner = CreateWindowExW(WS_EX_TOOLWINDOW, w!("SnipTory.Native"), w!("SnipTory message window"), WS_POPUP,
                    0,0,0,0,None,None,Some(instance.into()),None)?;
                let icon = make_icon()?;
                let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
                STATE.with(|s| *s.borrow_mut() = Some(State { owner, overlay: HWND::default(), icon, ui, store, commands: rx,
                    selection: None, target: GetForegroundWindow(), pending_paste: None, foreground_hook: HWINEVENTHOOK::default(), taskbar_created, restarting: false, preferences }));
                let hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND,EVENT_SYSTEM_FOREGROUND,None,Some(foreground_changed),0,0,WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                STATE.with(|s| s.borrow_mut().as_mut().unwrap().foreground_hook = hook);
                if hook.is_invalid() { notify("Live focus tracking is unavailable. Focus your destination before opening History."); }
                add_tray(owner, icon)?;
                for (id,key,modifiers,label) in [(1,0x53,MOD_ALT | MOD_SHIFT,"Alt+Shift+S"),(2,0x56,MOD_ALT | MOD_SHIFT,"Alt+Shift+V")] {
                    if RegisterHotKey(Some(owner), id, modifiers | MOD_NOREPEAT, key).is_err() {
                        notify(&format!("{label} is already in use. Capture and history remain available from the tray."));
                    }
                }
                Ok(owner)
            })();
            match initialized {
                Ok(owner) => { published.store(owner.0 as isize, Ordering::Release); let _ = ready_tx.send(Ok(())); }
                Err(error) => { let _ = ready_tx.send(Err(format!("{error:#}"))); return; }
            }
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0,0).0 > 0 { let _ = TranslateMessage(&message); DispatchMessageW(&message); }
            let state = STATE.with(|state| state.borrow_mut().take());
            if let Some(state) = state {
                if !state.foreground_hook.is_invalid() { let _ = UnhookWinEvent(state.foreground_hook); }
                let _ = UnregisterHotKey(Some(state.owner),1); let _ = UnregisterHotKey(Some(state.owner),2);
                let mut data = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: state.owner, uID: 1, ..Default::default() };
                let _ = Shell_NotifyIconW(NIM_DELETE, &mut data);
                let _ = DestroyWindow(state.owner); let _ = DestroyIcon(state.icon);
            }
        }
    })?;
    ready_rx.recv()?.map_err(anyhow::Error::msg)?;
    Ok(Bridge { tx, hwnd })
}

fn ui_status(message: impl Into<String>) {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            status(&s.ui, message);
        }
    });
}

fn notify(message: &str) {
    ui_status(message);
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: s.owner,
                uID: 1,
                uFlags: NIF_INFO,
                dwInfoFlags: NIIF_INFO,
                ..Default::default()
            };
            put_text(&mut data.szInfoTitle, "SnipTory");
            put_text(&mut data.szInfo, message);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &mut data);
            }
        }
    });
}

fn remember_target() {
    unsafe {
        remember_window(GetForegroundWindow());
    }
}

fn remember_window(foreground: HWND) {
    unsafe {
        let mut process = 0;
        GetWindowThreadProcessId(foreground, Some(&mut process));
        let mut class = [0u16; 128];
        let length = GetClassNameW(foreground, &mut class);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        let shell = matches!(
            class.as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
        );
        if process != 0 && process != GetCurrentProcessId() && !foreground.is_invalid() && !shell {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.target = foreground;
                }
            });
        }
    }
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // Out-of-context events are dispatched on the native message-loop thread.
    // Track handles only; never inspect or persist another application's content.
    if event == EVENT_SYSTEM_FOREGROUND {
        remember_window(hwnd);
    }
}

fn show_history() {
    if restarting() {
        return;
    }
    remember_target();
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let _ = s.store.send(StoreCommand::Load(60));
            let _ = s.ui.upgrade_in_event_loop(|ui| unsafe {
                ui.set_show_options(false);
                let mut cursor = POINT::default();
                let _ = GetCursorPos(&mut cursor);
                let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let _ = GetMonitorInfoW(monitor, &mut info);
                let mut dpi_x = 96;
                let mut dpi_y = 96;
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                let scale = dpi_x as f32 / 96.;
                let work = info.rcWork;
                let width = (496. * scale).min((work.right - work.left - 16) as f32) as u32;
                let height = (648. * scale).min((work.bottom - work.top - 16) as f32) as u32;
                let x = (cursor.x - width as i32 / 2).clamp(
                    work.left + 8,
                    (work.right - width as i32 - 8).max(work.left + 8),
                );
                let y = (cursor.y - 72).clamp(
                    work.top + 8,
                    (work.bottom - height as i32 - 8).max(work.top + 8),
                );
                ui.window().set_position(slint::PhysicalPosition::new(x, y));
                ui.window()
                    .set_size(slint::PhysicalSize::new(width, height));
                let _ = ui.show();
                ui.invoke_prepare();
                if let Some(handle) = crate::hwnd(&ui) {
                    let hwnd = HWND(handle as *mut _);
                    install_history_title_bar(hwnd);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    let corner: u32 = 2;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_WINDOW_CORNER_PREFERENCE,
                        (&corner as *const u32).cast(),
                        4,
                    );
                    let dark: i32 = 1;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE,
                        (&dark as *const i32).cast(),
                        4,
                    );
                    let _ = SetForegroundWindow(hwnd);
                }
            });
        }
    });
}

fn begin_capture() {
    if restarting() {
        return;
    }
    if STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.selection.is_some())) {
        return;
    }
    remember_target();
    // Hide Slint first on its owning thread; queue capture only once hidden.
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let owner = s.owner.0 as isize;
            let _ = s.ui.upgrade_in_event_loop(move |ui| {
                let handle = crate::hwnd(&ui);
                let _ = ui.hide();
                unsafe {
                    if let Some(handle) = handle {
                        let _ = ShowWindow(HWND(handle as *mut _), SW_HIDE);
                    }
                    let _ = PostMessageW(
                        Some(HWND(owner as *mut _)),
                        WM_APP + 3,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
        }
    });
}

fn create_selection() -> Result<()> {
    if restarting() {
        return Ok(());
    }
    unsafe {
        // Flush DWM after hiding history, then freeze the desktop before creating
        // any selector. The output is cropped from this immutable source.
        let _ = DwmFlush();
        let desktop = capture::desktop()?;
        let (left, top, w, h) = (
            desktop.left,
            desktop.top,
            desktop.pixels.width() as i32,
            desktop.pixels.height() as i32,
        );
        STATE.with(|s| {
            s.borrow_mut().as_mut().unwrap().selection = Some(Selection {
                desktop,
                anchor: None,
                current: POINT::default(),
            })
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            w!("SnipTory.Native"),
            w!("SnipTory — Select a region · Esc to cancel"),
            WS_POPUP,
            left,
            top,
            w,
            h,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        STATE.with(|s| s.borrow_mut().as_mut().unwrap().overlay = hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

fn end_selection(commit: bool) {
    let data = STATE.with(|s| {
        let mut state = s.borrow_mut();
        let s = state.as_mut()?;
        let selection = s.selection.take()?;
        let hwnd = s.overlay;
        s.overlay = HWND::default();
        Some((selection, hwnd, s.owner, s.store.clone()))
    });
    if let Some((selection, hwnd, owner, store)) = data {
        unsafe {
            let _ = ReleaseCapture();
            let _ = DestroyWindow(hwnd);
        }
        if commit {
            if let Some(anchor) = selection.anchor {
                if let Some(pixels) = capture::crop(&selection.desktop, anchor, selection.current) {
                    let copied = clipboard::copy(owner, &pixels);
                    let _ = store.send(StoreCommand::Save(pixels));
                    if let Err(error) = copied {
                        notify(&format!("{error:#}"));
                    }
                }
            }
        }
    }
}

fn copy_paste(pixels: image::RgbaImage) {
    if restarting() {
        return;
    }
    let (owner, target, ui) = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        (s.owner, s.target, s.ui.clone())
    });
    if let Err(error) = clipboard::copy(owner, &pixels) {
        notify(&format!("{error:#}"));
        return;
    }
    let owner_raw = owner.0 as isize;
    ui_status("Copied to clipboard");
    let paste_on_select = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .unwrap()
            .preferences
            .lock()
            .unwrap()
            .paste_on_select
    });
    if !paste_on_select {
        ui_status("Copied to clipboard");
        return;
    }
    let target_raw = target.0 as isize;
    let _ = ui.upgrade_in_event_loop(move |_ui| unsafe {
        let target = HWND(target_raw as *mut _);
        if IsWindow(Some(target)).as_bool() {
            let _ = SetForegroundWindow(target);
        }
        let _ = PostMessageW(
            Some(HWND(owner_raw as *mut _)),
            WM_APP + 4,
            WPARAM(0),
            LPARAM(target_raw),
        );
    });
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Windows invokes this callback on the window-owning thread. All
    // pointers are API-provided; UI state never leaves the owning thread.
    unsafe {
        let overlay = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .is_some_and(|s| s.overlay == hwnd && s.selection.is_some())
        });
        if overlay {
            match message {
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    paint_selection(hwnd);
                    return LRESULT(0);
                }
                WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                    end_selection(false);
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN => {
                    let point = mouse_point(lparam);
                    STATE.with(|s| {
                        if let Some(selection) =
                            s.borrow_mut().as_mut().and_then(|s| s.selection.as_mut())
                        {
                            selection.anchor = Some(point);
                            selection.current = point;
                        }
                    });
                    SetCapture(hwnd);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                WM_MOUSEMOVE => {
                    STATE.with(|s| {
                        if let Some(selection) =
                            s.borrow_mut().as_mut().and_then(|s| s.selection.as_mut())
                        {
                            selection.current = mouse_point(lparam);
                        }
                    });
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                WM_LBUTTONUP => {
                    STATE.with(|s| {
                        if let Some(selection) =
                            s.borrow_mut().as_mut().and_then(|s| s.selection.as_mut())
                        {
                            selection.current = mouse_point(lparam);
                        }
                    });
                    end_selection(true);
                    return LRESULT(0);
                }
                WM_CLOSE => {
                    end_selection(false);
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        match message {
            WAKE => {
                loop {
                    let command = STATE
                        .with(|s| s.borrow().as_ref().and_then(|s| s.commands.try_recv().ok()));
                    match command {
                        Some(Command::Capture) => begin_capture(),
                        Some(Command::History) => show_history(),
                        Some(Command::CopyPaste(pixels)) => copy_paste(pixels),
                        Some(Command::Notify(text)) => notify(&text),
                        Some(Command::ApplyUpdate(manager, asset)) => {
                            STATE.with(|s| s.borrow_mut().as_mut().unwrap().restarting = true);
                            let _ = KillTimer(Some(hwnd), 1);
                            let _ = KillTimer(Some(hwnd), 2);
                            end_selection(false);
                            STATE.with(|s| {
                                if let Some(s) = s.borrow().as_ref() {
                                    let _ = s.store.send(StoreCommand::ApplyUpdate(manager, asset));
                                }
                            });
                        }
                        Some(Command::UpdateFailed) => {
                            STATE.with(|s| s.borrow_mut().as_mut().unwrap().restarting = false);
                        }
                        Some(Command::Options) => {
                            show_history();
                            STATE.with(|s| {
                                if let Some(s) = s.borrow().as_ref() {
                                    let _ =
                                        s.ui.upgrade_in_event_loop(|ui| ui.set_show_options(true));
                                }
                            });
                        }
                        Some(Command::Updates) => {
                            show_history();
                            STATE.with(|s| {
                                if let Some(s) = s.borrow().as_ref() {
                                    let _ =
                                        s.ui.upgrade_in_event_loop(|ui| ui.invoke_check_updates());
                                }
                            });
                        }
                        Some(Command::Restart) => {
                            STATE.with(|s| s.borrow_mut().as_mut().unwrap().restarting = true);
                            let _ = KillTimer(Some(hwnd), 1);
                            let _ = KillTimer(Some(hwnd), 2);
                            end_selection(false);
                            STATE.with(|s| {
                                if let Some(s) = s.borrow().as_ref() {
                                    let _ = s.store.send(StoreCommand::Restart);
                                }
                            });
                        }
                        Some(Command::Quit) => {
                            PostQuitMessage(0);
                            break;
                        }
                        None => break,
                    }
                }
                return LRESULT(0);
            }
            WM_HOTKEY => {
                if wparam.0 == 1 {
                    begin_capture();
                } else {
                    show_history();
                }
                return LRESULT(0);
            }
            m if m == WM_APP + 3 => {
                // Give the desktop compositor a frame to remove history before
                // freezing pixels. This timer exists only during capture startup.
                SetTimer(Some(hwnd), 2, 60, None);
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == 2 => {
                let _ = KillTimer(Some(hwnd), 2);
                if let Err(error) = create_selection() {
                    STATE.with(|s| s.borrow_mut().as_mut().unwrap().selection = None);
                    notify(&format!("Capture failed: {error:#}"));
                }
                return LRESULT(0);
            }
            m if m == WM_APP + 4 => {
                STATE.with(|s| {
                    s.borrow_mut().as_mut().unwrap().pending_paste =
                        Some((Instant::now(), HWND(lparam.0 as *mut _)))
                });
                SetTimer(Some(hwnd), 1, 30, None);
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == 1 => {
                let (ready, expired, target) = STATE.with(|s| {
                    let s = s.borrow();
                    let s = s.as_ref().unwrap();
                    (
                        clipboard::modifiers_released(),
                        s.pending_paste
                            .is_some_and(|(t, _)| t.elapsed().as_secs() >= 2),
                        s.pending_paste
                            .map(|(_, target)| target)
                            .unwrap_or(s.target),
                    )
                });
                let focused = GetForegroundWindow() == target;
                if paste_can_finish(ready, focused, expired) {
                    let _ = KillTimer(Some(hwnd), 1);
                    STATE.with(|s| s.borrow_mut().as_mut().unwrap().pending_paste = None);
                    if !ready || !focused {
                        notify("Image copied. Focus your app and press Ctrl+V to paste.");
                    } else if let Err(error) = clipboard::paste() {
                        notify(&format!("{error:#}"));
                    }
                }
                return LRESULT(0);
            }
            TRAY => {
                let event = lparam.0 as u32 & 0xffff;
                if event == WM_LBUTTONUP || event == NIN_SELECT || event == (NIN_SELECT | 1) {
                    show_history();
                } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                    tray_menu(hwnd);
                }
                return LRESULT(0);
            }
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    10 => show_history(),
                    11 => begin_capture(),
                    12 => {
                        let _ = slint::quit_event_loop();
                        PostQuitMessage(0);
                    }
                    13 => {
                        show_history();
                        STATE.with(|s| {
                            if let Some(s) = s.borrow().as_ref() {
                                let _ = s.ui.upgrade_in_event_loop(|ui| ui.invoke_update_action());
                            }
                        });
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
            _ => {}
        }
        let recreate = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .filter(|s| message == s.taskbar_created)
                .map(|s| s.icon)
        });
        if let Some(icon) = recreate {
            let _ = add_tray(hwnd, icon);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn paste_can_finish(modifiers_released: bool, focus_restored: bool, expired: bool) -> bool {
    (modifiers_released && focus_restored) || expired
}

fn mouse_point(value: LPARAM) -> POINT {
    POINT {
        x: (value.0 & 0xffff) as i16 as i32,
        y: ((value.0 >> 16) & 0xffff) as i16 as i32,
    }
}

fn restarting() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.restarting))
}

pub fn wait_for_restart() -> Result<()> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if let Some(index) = arguments.iter().position(|arg| arg == "--restart-from") {
        let pid: u32 = arguments
            .get(index + 1)
            .ok_or_else(|| anyhow::anyhow!("Missing restart process"))?
            .parse()?;
        // SAFETY: synchronizing with the old instance requires no memory access.
        unsafe {
            if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let result = WaitForSingleObject(process, 60_000);
                let _ = CloseHandle(process);
                if result != WAIT_OBJECT_0 {
                    bail!("Previous SnipTory instance did not exit");
                }
            }
        }
    }
    Ok(())
}

pub fn open_folder(path: &std::path::Path) {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(path.as_os_str()),
            None,
            None,
            SW_SHOW,
        );
    }
}

pub fn show_tray_menu(menu: &crate::TrayMenu) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let mut dpi_x = 96;
        let mut dpi_y = 96;
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x as f32 / 96.;
        let width = (282. * scale) as i32;
        let height = (276. * scale) as i32;
        let work = info.rcWork;
        let x = (point.x - width).clamp(work.left + 6, (work.right - width - 6).max(work.left + 6));
        let y =
            (point.y - height).clamp(work.top + 6, (work.bottom - height - 6).max(work.top + 6));
        menu.window()
            .set_position(slint::PhysicalPosition::new(x, y));
        menu.window()
            .set_size(slint::PhysicalSize::new(width as u32, height as u32));
        let _ = menu.show();
        menu.invoke_prepare();
        if let Ok(handle) = menu.window().window_handle().window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                let hwnd = HWND(handle.hwnd.get() as *mut _);
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    (style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize),
                );
                let corner: u32 = 2;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    (&corner as *const u32).cast(),
                    4,
                );
                let _ = SetForegroundWindow(hwnd);
                MENU.with(|state| {
                    let mut state = state.borrow_mut();
                    if state.is_none() {
                        let old = SetWindowLongPtrW(
                            hwnd,
                            GWLP_WNDPROC,
                            menu_procedure as *const () as isize,
                        );
                        *state = Some((menu.as_weak(), old));
                    }
                });
            }
        }
    }
}

thread_local! { static MENU: RefCell<Option<(slint::Weak<crate::TrayMenu>,isize)>> = const { RefCell::new(None) }; }
unsafe extern "system" fn menu_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let previous = MENU.with(|s| s.borrow().clone());
    if let Some((menu, previous)) = previous {
        if message == WM_ACTIVATE && (wparam.0 & 0xffff) == WA_INACTIVE as usize {
            let _ = menu.upgrade_in_event_loop(|menu| {
                let _ = menu.hide();
            });
        }
        // SAFETY: previous is the WNDPROC returned by subclassing this window.
        unsafe {
            CallWindowProcW(
                std::mem::transmute::<isize, WNDPROC>(previous),
                hwnd,
                message,
                wparam,
                lparam,
            )
        }
    } else {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

unsafe fn paint_selection(hwnd: HWND) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        STATE.with(|s| {
            let state = s.borrow();
            let Some(selection) = state.as_ref().and_then(|s| s.selection.as_ref()) else {
                return;
            };
            let desktop = &selection.desktop;
            let w = desktop.pixels.width() as i32;
            let h = desktop.pixels.height() as i32;
            let info = capture::bitmap_info(w, h);
            StretchDIBits(
                dc,
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                Some(desktop.dimmed.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let mut label = "Drag to capture a region   ·   Esc to cancel".to_string();
            let mut label_x = (w / 2 - 180).max(8);
            let mut label_y = 36;
            if let Some(anchor) = selection.anchor {
                let point = selection.current;
                let x = anchor.x.min(point.x).clamp(0, w);
                let y = anchor.y.min(point.y).clamp(0, h);
                let width = (anchor.x.max(point.x).clamp(0, w) - x).max(0);
                let height = (anchor.y.max(point.y).clamp(0, h) - y).max(0);
                let region = RECT {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                };
                let _ = SaveDC(dc);
                let _ = IntersectClipRect(dc, x, y, x + width, y + height);
                StretchDIBits(
                    dc,
                    0,
                    0,
                    w,
                    h,
                    0,
                    0,
                    w,
                    h,
                    Some(desktop.bgra.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                let _ = RestoreDC(dc, -1);
                let brush = CreateSolidBrush(COLORREF(0x00df9f6e));
                FrameRect(dc, &region, brush);
                let _ = DeleteObject(brush.into());
                label = format!("{width} × {height} px   ·   Release to capture");
                label_x = x.clamp(8, (w - 360).max(8));
                label_y = if y > 40 {
                    y - 36
                } else {
                    (y + height + 8).min(h - 32)
                };
            }
            let rectangle = RECT {
                left: label_x,
                top: label_y,
                right: label_x + 350,
                bottom: label_y + 28,
            };
            let brush = CreateSolidBrush(COLORREF(0x00231c1b));
            FillRect(dc, &rectangle, brush);
            let _ = DeleteObject(brush.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00f4f0ec));
            let font = CreateFontW(
                -14,
                0,
                0,
                0,
                500,
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
            let mut text = label.encode_utf16().collect::<Vec<_>>();
            let mut rect = rectangle;
            DrawTextW(
                dc,
                &mut text,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
        });
        let _ = EndPaint(hwnd, &paint);
    }
}

thread_local! {
    static HISTORY_PROC: RefCell<Option<(isize,isize)>> = const { RefCell::new(None) };
}

fn in_title_bar(x: i32, y: i32, width: i32, dpi: u32) -> bool {
    let scale = dpi.max(96) as f64 / 96.;
    x >= 0 && y >= 0 && (y as f64) < 73. * scale && (x as f64) < width as f64 - 94. * scale
}

unsafe fn install_history_title_bar(hwnd: HWND) {
    let installed = HISTORY_PROC.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|(handle, _)| *handle == hwnd.0 as isize)
    });
    if !installed {
        // SAFETY: subclass only our Slint window, on its owning UI thread.
        let previous = unsafe {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, history_procedure as *const () as isize)
        };
        if previous != 0 {
            HISTORY_PROC.with(|state| *state.borrow_mut() = Some((hwnd.0 as isize, previous)));
        }
    }
}

unsafe extern "system" fn history_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: WM_NCHITTEST coordinates are physical screen pixels under the
    // per-monitor-v2 manifest. Convert to client pixels, then use the window's
    // current DPI so the 73 logical-pixel title bar follows monitor changes.
    unsafe {
        if message == WM_NCHITTEST {
            let mut point = mouse_point(lparam);
            let mut client = RECT::default();
            if ScreenToClient(hwnd, &mut point).as_bool()
                && GetClientRect(hwnd, &mut client).is_ok()
                && in_title_bar(
                    point.x,
                    point.y,
                    client.right - client.left,
                    GetDpiForWindow(hwnd),
                )
            {
                return LRESULT(HTCAPTION as isize);
            }
        }
        let previous = HISTORY_PROC.with(|state| {
            state
                .borrow()
                .as_ref()
                .filter(|(handle, _)| *handle == hwnd.0 as isize)
                .map(|(_, previous)| *previous)
        });
        let result = if let Some(previous) = previous {
            CallWindowProcW(
                std::mem::transmute::<isize, WNDPROC>(previous),
                hwnd,
                message,
                wparam,
                lparam,
            )
        } else {
            DefWindowProcW(hwnd, message, wparam, lparam)
        };
        if message == WM_NCDESTROY {
            HISTORY_PROC.with(|state| *state.borrow_mut() = None);
        }
        result
    }
}

#[cfg(test)]
mod title_bar_tests {
    use super::in_title_bar;
    #[test]
    fn content_and_buttons_are_never_draggable_across_dpi() {
        for dpi in [96, 144, 192] {
            let scale = dpi as f64 / 96.;
            let pixel = |value: f64| (value * scale).ceil() as i32;
            let width = pixel(496.);
            assert!(in_title_bar(pixel(100.), pixel(35.), width, dpi));
            for (x, y) in [
                (432., 35.),
                (466., 35.),
                (100., 73.),
                (200., 300.),
                (200., 620.),
            ] {
                assert!(
                    !in_title_bar(pixel(x), pixel(y), width, dpi),
                    "unexpected drag at {x},{y}, dpi {dpi}"
                );
            }
            assert!(!in_title_bar(-1, 0, width, dpi));
        }
    }
}

#[cfg(test)]
mod paste_focus_tests {
    use super::paste_can_finish;
    #[test]
    fn paste_waits_for_focus_even_after_modifiers_are_released() {
        assert!(!paste_can_finish(true, false, false));
        assert!(!paste_can_finish(false, true, false));
        assert!(paste_can_finish(true, true, false));
        assert!(paste_can_finish(true, false, true));
        assert!(paste_can_finish(false, true, true));
    }
}

fn put_text<const N: usize>(buffer: &mut [u16; N], text: &str) {
    for (to, from) in buffer.iter_mut().take(N - 1).zip(text.encode_utf16()) {
        *to = from;
    }
}

unsafe fn add_tray(hwnd: HWND, icon: HICON) -> Result<()> {
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY,
            hIcon: icon,
            ..Default::default()
        };
        put_text(&mut data.szTip, "SnipTory — Capture. Recall. Paste.");
        if !Shell_NotifyIconW(NIM_ADD, &mut data).as_bool() {
            bail!("Could not create the system tray icon");
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &mut data);
        Ok(())
    }
}

unsafe fn tray_menu(_hwnd: HWND) {
    remember_target();
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let _ = s.ui.upgrade_in_event_loop(|ui| ui.invoke_tray_menu());
        }
    });
}

unsafe fn make_icon() -> Result<HICON> {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0i32..32 {
        for x in 0i32..32 {
            let corner = ((5..=11).contains(&x) || (20..=26).contains(&x)) && (y == 6 || y == 25)
                || ((5..=11).contains(&y) || (20..=26).contains(&y)) && (x == 6 || x == 25);
            let center = (12..=19).contains(&x) && (y == 12 || y == 19)
                || (12..=19).contains(&y) && (x == 12 || x == 19);
            if !corner && !center {
                continue;
            }
            let p = ((31 - y) * 32 + x) as usize * 4;
            pixels[p..p + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mask = [0u8; 128];
    unsafe {
        Ok(CreateIcon(
            None,
            32,
            32,
            1,
            32,
            mask.as_ptr(),
            pixels.as_ptr(),
        )?)
    }
}
