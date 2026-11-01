//! Win32 owns the tray, global shortcuts and physical-pixel region selector on
//! one blocking message-loop thread. Slint owns the gallery on the main thread.
use crate::{HistoryWindow, StoreCommand, capture, clipboard, status};
use anyhow::{Context, Result, bail};
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
    ViewCapture(std::path::PathBuf),
    ConfigureShortcuts(
        crate::hotkeys::Shortcuts,
        mpsc::Sender<std::result::Result<(), String>>,
    ),
    Capture,
    FullCapture,
    RecordGif,
    BeginRecording(RECT),
    StopRecording,
    CancelRecording,
    RecordingFinished(Result<Option<crate::recording::RecordedGif>, String>),
    CopyGif(std::path::PathBuf, bool),
    Exit,
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
    pub fn configure_shortcuts(&self, shortcuts: &crate::hotkeys::Shortcuts) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.send(Command::ConfigureShortcuts(shortcuts.clone(), tx));
        rx.recv()?.map_err(anyhow::Error::msg)
    }
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
    kind: CaptureKind,
    desktop: Option<capture::Desktop>,
    bounds: RECT,
    paint: PaintBuffer,
    anchor: Option<POINT>,
    current: POINT,
}

/// Reused, selection-lifetime GDI back buffer; nothing is allocated while idle.
struct PaintBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
}
impl PaintBuffer {
    fn new(width: i32, height: i32) -> Result<Self> {
        // SAFETY: all objects are created, selected, and destroyed on the native
        // message-loop thread. Restore the original bitmap before deleting ours.
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                bail!("Could not access selection display");
            }
            let dc = CreateCompatibleDC(Some(screen));
            if dc.is_invalid() {
                ReleaseDC(None, screen);
                bail!("Could not create selection buffer");
            }
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                Some(screen),
                &capture::bitmap_info(width, height),
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            );
            ReleaseDC(None, screen);
            let bitmap = match bitmap {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    bail!("Selection buffer is unavailable");
                }
                Err(error) => {
                    let _ = DeleteDC(dc);
                    return Err(error.into());
                }
            };
            let previous = SelectObject(dc, bitmap.into());
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
            })
        }
    }
}
impl Drop for PaintBuffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Region,
    Full,
    Gif,
}
struct RecordingSession {
    control: crate::recording::Control,
    started: Instant,
    stopped_elapsed: Option<std::time::Duration>,
}
struct State {
    owner: HWND,
    overlay: HWND,
    icon: HICON,
    ui: slint::Weak<HistoryWindow>,
    store: mpsc::Sender<StoreCommand>,
    commands: mpsc::Receiver<Command>,
    selection: Option<Selection>,
    capture_request: Option<CaptureKind>,
    target: HWND,
    pending_paste: Option<(Instant, HWND)>,
    foreground_hook: HWINEVENTHOOK,
    taskbar_created: u32,
    restarting: bool,
    preferences: crate::settings::Shared,
    shortcuts: crate::hotkeys::Shortcuts,
    recording_bar: slint::Weak<crate::RecordingBar>,
    recording: Option<RecordingSession>,
    recording_pending: bool,
    quit_after_recording: bool,
    bridge: Bridge,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

pub fn start(
    ui: slint::Weak<HistoryWindow>,
    recording_bar: slint::Weak<crate::RecordingBar>,
    store: mpsc::Sender<StoreCommand>,
    preferences: crate::settings::Shared,
) -> Result<Bridge> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let hwnd = Arc::new(AtomicIsize::new(0));
    let published = hwnd.clone();
    let thread_bridge = Bridge {
        tx: tx.clone(),
        hwnd: hwnd.clone(),
    };
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
                let shortcuts = preferences.lock().unwrap().shortcuts.clone();
                STATE.with(|s| *s.borrow_mut() = Some(State { owner, overlay: HWND::default(), icon, ui, store, commands: rx,
                    selection: None, capture_request: None, target: GetForegroundWindow(), pending_paste: None, foreground_hook: HWINEVENTHOOK::default(), taskbar_created, restarting: false, preferences, shortcuts:shortcuts.clone(), recording_bar, recording:None, recording_pending:false, quit_after_recording:false, bridge:thread_bridge }));
                let hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND,EVENT_SYSTEM_FOREGROUND,None,Some(foreground_changed),0,0,WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                STATE.with(|s| s.borrow_mut().as_mut().unwrap().foreground_hook = hook);
                if hook.is_invalid() { notify("Live focus tracking is unavailable. Focus your destination before opening History."); }
                add_tray(owner, icon)?;
                for (index,binding) in shortcuts.bindings()?.iter().enumerate() {
                    if RegisterHotKey(Some(owner), index as i32 + 1, HOT_KEY_MODIFIERS(binding.modifiers) | MOD_NOREPEAT, binding.key).is_err() {
                        notify(&format!("{} is already in use. Capture and history remain available from the tray.",binding.label));
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
                let _ = UnregisterHotKey(Some(state.owner),3);
                let _ = UnregisterHotKey(Some(state.owner),4);
                if let Some(recording) = state.recording { recording.control.cancel(); }
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
            let recording = s.recording.is_some() || s.recording_pending;
            let _ = s.ui.upgrade_in_event_loop(move |ui| unsafe {
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
                    let _ = SetWindowDisplayAffinity(
                        hwnd,
                        if recording {
                            WDA_EXCLUDEFROMCAPTURE
                        } else {
                            WDA_NONE
                        },
                    );
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
    prepare_capture(CaptureKind::Region);
}

fn prepare_capture(kind: CaptureKind) {
    if restarting() {
        return;
    }
    if recording_busy() {
        notify("Stop the current GIF before starting another capture.");
        return;
    }
    if STATE.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|s| s.selection.is_some() || s.capture_request.is_some())
    }) {
        return;
    }
    remember_target();
    STATE.with(|s| s.borrow_mut().as_mut().unwrap().capture_request = Some(kind));
    // Screenshot the visible desktop, including History when it is open.
    // Hiding it here prevents users from capturing the gallery itself.
    if !matches!(kind, CaptureKind::Gif) {
        STATE.with(|s| {
            if let Some(s) = s.borrow().as_ref() {
                unsafe {
                    let _ = PostMessageW(Some(s.owner), WM_APP + 3, WPARAM(0), LPARAM(0));
                }
            }
        });
        return;
    }
    // GIF selection still hides History before opening its live overlay.
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

fn create_selection(kind: CaptureKind) -> Result<()> {
    if restarting() {
        return Ok(());
    }
    unsafe {
        // Screenshots use an immutable source. GIF selection is a translucent
        // surface over the live desktop and never captures a frozen background.
        let _ = DwmFlush();
        let desktop = if matches!(kind, CaptureKind::Gif) {
            None
        } else {
            Some(capture::desktop()?)
        };
        let (left, top, w, h) = if let Some(desktop) = desktop.as_ref() {
            (
                desktop.left,
                desktop.top,
                desktop.pixels.width() as i32,
                desktop.pixels.height() as i32,
            )
        } else {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        anyhow::ensure!(
            w > 0 && h > 0 && w as u64 * h as u64 * 4 <= 512 * 1024 * 1024,
            "Desktop is too large to select safely"
        );
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let current = POINT {
            x: (cursor.x - left).clamp(0, w - 1),
            y: (cursor.y - top).clamp(0, h - 1),
        };
        let paint = PaintBuffer::new(w, h)?;
        STATE.with(|s| {
            s.borrow_mut().as_mut().unwrap().selection = Some(Selection {
                kind,
                desktop,
                bounds: RECT {
                    left,
                    top,
                    right: left + w,
                    bottom: top + h,
                },
                paint,
                anchor: None,
                current,
            })
        });
        let hwnd = CreateWindowExW(
            if matches!(kind, CaptureKind::Gif) {
                WS_EX_TOPMOST | WS_EX_LAYERED
            } else {
                WS_EX_TOPMOST
            },
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
        if matches!(kind, CaptureKind::Gif) {
            paint_selection(hwnd);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

fn full_screenshot() -> Result<()> {
    // The same hide/compositor step as region capture runs before this function.
    unsafe {
        let _ = DwmFlush();
    }
    let pixels = capture::desktop()?.pixels;
    let (owner, store) = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        (s.owner, s.store.clone())
    });
    let copied = clipboard::copy(owner, &pixels);
    store.send(StoreCommand::Save(pixels))?;
    copied?;
    Ok(())
}

fn recording_busy() -> bool {
    STATE.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|s| s.recording_pending || s.recording.is_some())
    })
}

fn prepare_recording(region: RECT) {
    let (bar, bridge) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut().unwrap();
        s.recording_pending = true;
        (s.recording_bar.clone(), s.bridge.clone())
    });
    let _ = bar.upgrade_in_event_loop(move |bar| {
        let result: Result<()> = (|| {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            unsafe {
                let monitor = MonitorFromRect(&region, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                GetMonitorInfoW(monitor, &mut info).ok()?;
                let mut dx = 96;
                let mut dy = 96;
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
                let scale = dx as f32 / 96.;
                let width = (312. * scale) as u32;
                let height = (52. * scale) as u32;
                bar.set_finishing(false);
                bar.set_elapsed("0:00".into());
                bar.window()
                    .set_size(slint::PhysicalSize::new(width, height));
                bar.window().set_position(slint::PhysicalPosition::new(
                    info.rcWork.left + (info.rcWork.right - info.rcWork.left - width as i32) / 2,
                    info.rcWork.bottom - height as i32 - (24. * scale) as i32,
                ));
                bar.show()?;
                let window_handle = bar.window().window_handle();
                let handle = window_handle.window_handle()?;
                let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                    bail!("Recording controls require Windows");
                };
                let hwnd = HWND(handle.hwnd.get() as *mut _);
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    (style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize),
                );
                SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
                    .context("Cannot exclude recording controls from capture")?;
                let corner: u32 = 2;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    (&corner as *const u32).cast(),
                    4,
                );
                let _ = DwmFlush();
            }
            Ok(())
        })();
        match result {
            Ok(()) => bridge.send(Command::BeginRecording(region)),
            Err(error) => {
                let _ = bar.hide();
                bridge.send(Command::RecordingFinished(Err(format!("{error:#}"))));
            }
        }
    });
}

fn start_recording(region: RECT) {
    let (bridge, ui, owner) = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        (s.bridge.clone(), s.ui.clone(), s.owner)
    });
    let result = crate::settings::root().and_then(|root| {
        let folder = crate::settings::load(&root).capture_root(&root);
        let root = crate::settings::prepare_capture_folder(&folder, false)?;
        crate::recording::start(region, root, move |result| {
            bridge.send(Command::RecordingFinished(result))
        })
    });
    match result {
        Ok(control) => {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let s = s.as_mut().unwrap();
                s.recording_pending = false;
                if s.quit_after_recording {
                    control.stop();
                }
                s.recording = Some(RecordingSession {
                    control,
                    started: Instant::now(),
                    stopped_elapsed: s.quit_after_recording.then_some(std::time::Duration::ZERO),
                });
            });
            unsafe {
                SetTimer(Some(owner), 3, 250, None);
            }
            let _ = ui.upgrade_in_event_loop(|ui| {
                if let Some(handle) = crate::hwnd(&ui) {
                    unsafe {
                        let _ = SetWindowDisplayAffinity(
                            HWND(handle as *mut _),
                            WDA_EXCLUDEFROMCAPTURE,
                        );
                    }
                }
            });
            ui_status("Recording GIF");
        }
        Err(error) => finish_recording(Err(format!("{error:#}"))),
    }
}

fn stop_recording(cancel: bool) {
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            if let Some(recording) = s.recording.as_mut() {
                recording
                    .stopped_elapsed
                    .get_or_insert_with(|| recording.started.elapsed());
                if cancel {
                    recording.control.cancel();
                } else {
                    recording.control.stop();
                }
                let _ = s
                    .recording_bar
                    .upgrade_in_event_loop(|bar| bar.set_finishing(true));
            }
        }
    });
}

fn finish_recording(mut result: Result<Option<crate::recording::RecordedGif>, String>) {
    let cancelled = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|s| s.recording.as_ref())
            .is_some_and(|s| s.control.cancelled())
    });
    if cancelled {
        if let Ok(Some(recording)) = &result {
            let _ = std::fs::remove_file(&recording.path);
        }
        result = Ok(None);
    }
    let (bar, ui, store, quit, owner) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut().unwrap();
        s.recording = None;
        s.recording_pending = false;
        (
            s.recording_bar.clone(),
            s.ui.clone(),
            s.store.clone(),
            s.quit_after_recording,
            s.owner,
        )
    });
    unsafe {
        let _ = KillTimer(Some(owner), 3);
    }
    let _ = bar.upgrade_in_event_loop(|bar| {
        let _ = bar.hide();
    });
    let _ = ui.upgrade_in_event_loop(|ui| {
        if let Some(handle) = crate::hwnd(&ui) {
            unsafe {
                let _ = SetWindowDisplayAffinity(HWND(handle as *mut _), WDA_NONE);
            }
        }
    });
    match result {
        Ok(Some(recording)) => {
            let _ = store.send(StoreCommand::SaveGif(recording));
        }
        Ok(None) => ui_status("GIF recording cancelled"),
        Err(error) => notify(&format!("Recording failed: {error}")),
    }
    if quit {
        let _ = store.send(StoreCommand::Exit);
    }
}

fn request_exit() {
    let busy = recording_busy();
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            if busy {
                s.quit_after_recording = true;
            } else {
                let _ = s.store.send(StoreCommand::Exit);
            }
        }
    });
    if busy {
        stop_recording(false);
    }
}

fn copy_gif(path: std::path::PathBuf, paste: bool) {
    let (owner, target, ui, automatic) = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        (
            s.owner,
            s.target,
            s.ui.clone(),
            s.preferences.lock().unwrap().paste_on_select,
        )
    });
    if let Err(error) = clipboard::copy_gif(owner, &path) {
        notify(&format!("{error:#}"));
        return;
    }
    ui_status("GIF file copied · animation depends on the destination");
    if paste && automatic {
        restore_paste_target(owner.0 as isize, target.0 as isize, ui);
    }
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
        if matches!(selection.kind, CaptureKind::Gif) {
            let region = selection.anchor.and_then(|anchor| {
                let w = selection.bounds.right - selection.bounds.left;
                let h = selection.bounds.bottom - selection.bounds.top;
                let left = anchor.x.min(selection.current.x).clamp(0, w);
                let top = anchor.y.min(selection.current.y).clamp(0, h);
                let right = anchor.x.max(selection.current.x).clamp(0, w);
                let bottom = anchor.y.max(selection.current.y).clamp(0, h);
                (right > left && bottom > top).then_some(RECT {
                    left: left + selection.bounds.left,
                    top: top + selection.bounds.top,
                    right: right + selection.bounds.left,
                    bottom: bottom + selection.bounds.top,
                })
            });
            drop(selection);
            if commit {
                if let Some(region) = region {
                    prepare_recording(region);
                }
            }
            return;
        }
        if commit {
            if let Some(anchor) = selection.anchor {
                if let Some(pixels) = selection
                    .desktop
                    .as_ref()
                    .and_then(|desktop| capture::crop(desktop, anchor, selection.current))
                {
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
    restore_paste_target(owner_raw, target_raw, ui);
}

fn restore_paste_target(owner_raw: isize, target_raw: isize, ui: slint::Weak<HistoryWindow>) {
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
                        Some(Command::FullCapture) => prepare_capture(CaptureKind::Full),
                        Some(Command::RecordGif) => prepare_capture(CaptureKind::Gif),
                        Some(Command::BeginRecording(region)) => start_recording(region),
                        Some(Command::StopRecording) => stop_recording(false),
                        Some(Command::CancelRecording) => stop_recording(true),
                        Some(Command::RecordingFinished(result)) => finish_recording(result),
                        Some(Command::ViewCapture(path)) => {
                            if let Err(error) = view_capture(&path) {
                                notify(&format!("{error:#}"));
                            }
                        }
                        Some(Command::ConfigureShortcuts(shortcuts, reply)) => {
                            let _ = reply
                                .send(configure_shortcuts(shortcuts).map_err(|e| format!("{e:#}")));
                        }
                        Some(Command::CopyGif(path, paste)) => copy_gif(path, paste),
                        Some(Command::Exit) => request_exit(),
                        Some(Command::History) => show_history(),
                        Some(Command::CopyPaste(pixels)) => copy_paste(pixels),
                        Some(Command::Notify(text)) => notify(&text),
                        Some(Command::ApplyUpdate(manager, asset)) => {
                            if recording_busy() {
                                notify("Stop the GIF before restarting to update.");
                                STATE.with(|s| {
                                    if let Some(s) = s.borrow().as_ref() {
                                        let _ = s.ui.upgrade_in_event_loop(|ui| {
                                            ui.set_update_busy(false);
                                            ui.set_update_label("Update".into());
                                            ui.set_update_detail("Stop the GIF recording, then click Update to continue.".into());
                                        });
                                    }
                                });
                                continue;
                            }
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
                            if recording_busy() {
                                notify("Stop the GIF before restarting SnipTory.");
                                continue;
                            }
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
                match wparam.0 {
                    1 => begin_capture(),
                    2 => show_history(),
                    3 => prepare_capture(CaptureKind::Full),
                    4 => prepare_capture(CaptureKind::Gif),
                    _ => {}
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
                let request =
                    STATE.with(|s| s.borrow_mut().as_mut().unwrap().capture_request.take());
                let result = match request {
                    Some(CaptureKind::Region) => create_selection(CaptureKind::Region),
                    Some(CaptureKind::Gif) => create_selection(CaptureKind::Gif),
                    Some(CaptureKind::Full) => full_screenshot(),
                    None => Ok(()),
                };
                if let Err(error) = result {
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
            WM_TIMER if wparam.0 == 3 => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow().as_ref() {
                        if let Some(recording) = s.recording.as_ref() {
                            let elapsed = recording
                                .stopped_elapsed
                                .unwrap_or_else(|| recording.started.elapsed())
                                .as_secs();
                            let finishing = recording.control.stopping();
                            let _ = s.recording_bar.upgrade_in_event_loop(move |bar| {
                                bar.set_elapsed(
                                    format!("{}:{:02}", elapsed / 60, elapsed % 60).into(),
                                );
                                bar.set_finishing(finishing);
                            });
                        }
                    }
                });
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
                        request_exit();
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

fn configure_shortcuts(shortcuts: crate::hotkeys::Shortcuts) -> Result<()> {
    let bindings = shortcuts.bindings()?;
    let (owner, old) = STATE.with(|s| {
        let state = s.borrow();
        let state = state.as_ref().unwrap();
        (state.owner, state.shortcuts.clone())
    });
    if shortcuts == old {
        return Ok(());
    }
    unsafe {
        for id in 1..=4 {
            let _ = UnregisterHotKey(Some(owner), id);
        }
        for (index, binding) in bindings.iter().enumerate() {
            if RegisterHotKey(
                Some(owner),
                index as i32 + 1,
                HOT_KEY_MODIFIERS(binding.modifiers) | MOD_NOREPEAT,
                binding.key,
            )
            .is_err()
            {
                for id in 1..=4 {
                    let _ = UnregisterHotKey(Some(owner), id);
                }
                let mut restored = true;
                for (index, binding) in old.bindings()?.iter().enumerate() {
                    restored &= RegisterHotKey(
                        Some(owner),
                        index as i32 + 1,
                        HOT_KEY_MODIFIERS(binding.modifiers) | MOD_NOREPEAT,
                        binding.key,
                    )
                    .is_ok();
                }
                if !restored {
                    bail!(
                        "{} is unavailable. Some previous shortcuts could not be restored; choose another combination or use the tray menu.",
                        binding.label
                    );
                }
                bail!(
                    "{} is already in use. Your previous shortcuts are unchanged.",
                    binding.label
                );
            }
        }
    }
    STATE.with(|s| s.borrow_mut().as_mut().unwrap().shortcuts = shortcuts);
    Ok(())
}

fn view_capture(path: &std::path::Path) -> Result<()> {
    anyhow::ensure!(path.is_file(), "This capture is no longer available");
    anyhow::ensure!(
        path.extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("png") || s.eq_ignore_ascii_case("gif")),
        "Unsupported capture type"
    );
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(path.as_os_str()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    anyhow::ensure!(
        result.0 as isize > 32,
        "Could not open the image viewer. Check your Windows image app association."
    );
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
        let height = (304. * scale) as i32;
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
        let output = BeginPaint(hwnd, &mut paint);
        STATE.with(|s| {
            let state = s.borrow();
            let Some(selection) = state.as_ref().and_then(|s| s.selection.as_ref()) else {
                return;
            };
            let dc = selection.paint.dc;
            let w = selection.bounds.right - selection.bounds.left;
            let h = selection.bounds.bottom - selection.bounds.top;
            let info = capture::bitmap_info(w, h);
            let accent = if matches!(selection.kind, CaptureKind::Gif) {
                COLORREF(0x00786aef)
            } else {
                COLORREF(0x00df9f6e)
            };
            if let Some(desktop) = selection.desktop.as_ref() {
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
            } else {
                std::ptr::write_bytes(selection.paint.bits, 0, w as usize * h as usize * 4);
            }
            let mut label = if matches!(selection.kind, CaptureKind::Gif) {
                "Drag to record a GIF   ·   Esc to cancel"
            } else {
                "Drag to capture a region   ·   Esc to cancel"
            }
            .to_string();
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
                if let Some(desktop) = selection.desktop.as_ref() {
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
                }
                let brush = CreateSolidBrush(accent);
                FrameRect(dc, &region, brush);
                let _ = DeleteObject(brush.into());
                label = format!(
                    "{width} × {height} px   ·   Release to {}",
                    if matches!(selection.kind, CaptureKind::Gif) {
                        "record"
                    } else {
                        "capture"
                    }
                );
                label_x = x.clamp(8, (w - 360).max(8));
                label_y = if y > 40 {
                    y - 36
                } else {
                    (y + height + 8).min(h - 32)
                };
            }
            // One physical-pixel guide in each direction, drawn only into the
            // selector. Saved captures crop the untouched frozen desktop pixels.
            let cursor_x = selection.current.x.clamp(0, w - 1);
            let cursor_y = selection.current.y.clamp(0, h - 1);
            let pen = CreatePen(PS_SOLID, 1, accent);
            let previous_pen = SelectObject(dc, pen.into());
            let _ = MoveToEx(dc, 0, cursor_y, None);
            let _ = LineTo(dc, w, cursor_y);
            let _ = MoveToEx(dc, cursor_x, 0, None);
            let _ = LineTo(dc, cursor_x, h);
            SelectObject(dc, previous_pen);
            let _ = DeleteObject(pen.into());

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
            // Present the completed frame once, rather than visibly dimming and
            // restoring the selected interior on every mouse movement.
            if selection.desktop.is_some() {
                let _ = BitBlt(output, 0, 0, w, h, Some(dc), 0, 0, SRCCOPY);
            } else {
                let _ = GdiFlush();
                let pixels = std::slice::from_raw_parts_mut(
                    selection.paint.bits,
                    w as usize * h as usize * 4,
                );
                let selected = selection.anchor.map(|anchor| RECT {
                    left: anchor.x.min(selection.current.x),
                    top: anchor.y.min(selection.current.y),
                    right: anchor.x.max(selection.current.x),
                    bottom: anchor.y.max(selection.current.y),
                });
                apply_live_alpha(pixels, w as usize, selected);
                let destination = POINT {
                    x: selection.bounds.left,
                    y: selection.bounds.top,
                };
                let source = POINT::default();
                let size = SIZE { cx: w, cy: h };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&destination),
                    Some(&size),
                    Some(dc),
                    Some(&source),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        });
        let _ = EndPaint(hwnd, &paint);
    }
}

fn apply_live_alpha(pixels: &mut [u8], width: usize, selected: Option<RECT>) {
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        // Nonzero alpha keeps the otherwise clear interior hit-testable. It is
        // visually transparent, while DWM continues composing live video below.
        pixel[3] = if pixel[..3].iter().any(|value| *value != 0) {
            255
        } else {
            let x = (index % width) as i32;
            let y = (index / width) as i32;
            if selected.is_some_and(|rect| {
                x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
            }) {
                1
            } else {
                32
            }
        };
    }
}

#[cfg(test)]
mod live_selection_tests {
    use super::*;
    #[test]
    fn live_background_is_translucent_and_guides_are_opaque() {
        let mut pixels = vec![0; 4 * 4];
        pixels[0] = 100;
        apply_live_alpha(
            &mut pixels,
            2,
            Some(RECT {
                left: 1,
                top: 0,
                right: 2,
                bottom: 1,
            }),
        );
        assert_eq!(pixels[3], 255);
        assert_eq!(pixels[7], 1);
        assert_eq!(pixels[11], 32);
        assert_eq!(pixels[15], 32);
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
