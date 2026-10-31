//! macOS frontend. AppKit/menu/hotkeys stay on the main thread; capture,
//! decoding, clipboard conversion and durable saves use one serialized worker.
use crate::HistoryWindow;
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use sniptory::{
    history::{Entry, History},
    hotkeys::Shortcuts,
    settings::{self, Settings},
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub enum Command {
    ApplyUpdate(velopack::UpdateManager, velopack::VelopackAsset),
    Action(Action),
    Gallery(Vec<Entry>, Vec<image::RgbaImage>, bool),
    SavedSettings(Settings, Option<String>),
    Status(String),
    CaptureFinished,
    PasteReady(i32),
    Hotkey(u32),
    ReadyToQuit,
    ReadyToRestart,
}
#[derive(Clone, Copy)]
pub enum Action {
    History,
    Options,
    Capture,
    Full,
    Folder,
    Restart,
    Quit,
}
#[derive(Clone)]
pub struct Bridge;
impl Bridge {
    pub fn send(&self, command: Command) {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = APP.with(|a| a.borrow().clone()) {
                if let Err(error) = app.handle(command) {
                    app.status(format!("{error:#}"));
                }
            }
        });
    }
}
enum Job {
    Load(usize),
    Capture(bool),
    Paste(PathBuf, i32, bool),
    Delete(String),
    Clear,
    Settings(Settings),
    Apply(velopack::UpdateManager, velopack::VelopackAsset),
    Quit,
    Restart,
}
struct App {
    ui: slint::Weak<HistoryWindow>,
    jobs: mpsc::Sender<Job>,
    root: PathBuf,
    preferences: settings::Shared,
    entries: RefCell<Vec<Entry>>,
    limit: Cell<usize>,
    manager: GlobalHotKeyManager,
    keys: RefCell<Vec<HotKey>>,
    menu_history: MenuItem,
    menu_capture: MenuItem,
    menu_full: MenuItem,
    _tray: TrayIcon,
    capture_busy: Cell<bool>,
    exiting: Cell<bool>,
    target: Cell<i32>,
    focus_timer: slint::Timer,
    paste_timer: slint::Timer,
    preview: mpsc::Sender<crate::preview::Request>,
    poster: RefCell<Option<(usize, String, slint::Image)>>,
}
thread_local! {static APP: RefCell<Option<Rc<App>>> = const {RefCell::new(None)};}

fn load_gallery(history: &History, limit: usize) -> Result<()> {
    let all = history.list(limit + 1)?;
    let has_more = all.len() > limit;
    let mut entries = Vec::new();
    let mut previews = Vec::new();
    for entry in all.into_iter().take(limit) {
        if entry.path.is_file() {
            if let Ok(image) = history.preview(&entry) {
                entries.push(entry);
                previews.push(image);
            }
        }
    }
    Bridge.send(Command::Gallery(entries, previews, has_more));
    Ok(())
}
fn capture(history: &History, root: &Path, full: bool) -> Result<Option<String>> {
    let path = root.join(format!(
        "capture-{}.png",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| -> Result<Option<String>> {
        let mut command = Process::new("/usr/sbin/screencapture");
        command.args(["-x", "-t", "png"]);
        if full {
            command.arg("-m");
        } else {
            command.arg("-i");
        }
        command.arg(&path);
        let output = command
            .output()
            .context("Could not start macOS screen capture")?;
        if !path.is_file() {
            if output.status.success() {
                return Ok(None);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Capture cancelled or unavailable. Allow SnipTory in System Settings → Privacy & Security → Screen Recording. {}",
                stderr.trim()
            );
        }
        let image = image::open(&path)?.to_rgba8();
        history.save(&image)?;
        match set_image(&image) {
            Ok(()) => Ok(Some("Screenshot saved and copied to clipboard".into())),
            Err(error) => Ok(Some(format!(
                "Screenshot saved; clipboard unavailable: {error}"
            ))),
        }
    })();
    let _ = fs::remove_file(path);
    result
}
fn set_image(image: &image::RgbaImage) -> Result<()> {
    arboard::Clipboard::new()?.set_image(arboard::ImageData {
        width: image.width() as usize,
        height: image.height() as usize,
        bytes: Cow::Borrowed(image.as_raw()),
    })?;
    Ok(())
}
fn copy_capture(path: &Path) -> Result<()> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        let quoted = serde_json::to_string(&path.to_string_lossy())?;
        let output = Process::new("/usr/bin/osascript")
            .arg("-e")
            .arg(format!("set the clipboard to (POSIX file {quoted})"))
            .output()?;
        ensure!(
            output.status.success(),
            "Could not copy the GIF file: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    } else {
        set_image(&image::open(path)?.to_rgba8())
    }
}
fn apply_preferences(ui: &HistoryWindow, settings: &Settings) {
    ui.set_launch_at_sign_in(settings.launch_at_sign_in);
    ui.set_automatic_updates(settings.automatic_updates);
    ui.set_paste_on_select(settings.paste_on_select);
    ui.set_screenshot_hotkey(settings.shortcuts.screenshot.clone().into());
    ui.set_history_hotkey(settings.shortcuts.history.clone().into());
    ui.set_full_hotkey(settings.shortcuts.full.clone().into());
    ui.set_gif_hotkey(settings.shortcuts.gif.clone().into());
}
fn bundle_path() -> Result<PathBuf> {
    std::env::current_exe()?
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .context("Run SnipTory from its .app bundle")
}
fn startup(enabled: bool) -> Result<()> {
    let folder = PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?)
        .join("Library/LaunchAgents");
    let path = folder.join("com.bazstudios.sniptory.plist");
    if !enabled {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => return Ok(()),
        }
    }
    let bundle = bundle_path()?
        .to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    fs::create_dir_all(&folder)?;
    let temp = folder.join("com.bazstudios.sniptory.plist.tmp");
    fs::write(
        &temp,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>com.bazstudios.sniptory</string><key>ProgramArguments</key><array><string>/usr/bin/open</string><string>-a</string><string>{bundle}</string><string>--args</string><string>--background</string></array><key>RunAtLoad</key><true/></dict></plist>"#
        ),
    )?;
    fs::rename(temp, path)?;
    Ok(())
}
fn tray_glyph() -> Result<tray_icon::Icon> {
    let size = 32usize;
    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let outer = ((x == 5 || x == 26) && (5..=12).contains(&y))
                || ((y == 5 || y == 26) && (5..=12).contains(&x))
                || ((x == 5 || x == 26) && (19..=26).contains(&y))
                || ((y == 5 || y == 26) && (19..=26).contains(&x));
            let inner = ((x == 11 || x == 20) && (11..=20).contains(&y))
                || ((y == 11 || y == 20) && (11..=20).contains(&x));
            if outer || inner {
                pixels[(y * size + x) * 4..(y * size + x) * 4 + 4].copy_from_slice(&[255; 4]);
            }
        }
    }
    Ok(tray_icon::Icon::from_rgba(
        pixels,
        size as u32,
        size as u32,
    )?)
}

// Main-thread AppKit bridge. Casts match these exact Objective-C signatures;
// no Objective-C object or NSWindow pointer is sent to a worker thread.
use std::ffi::{CStr, c_char, c_void};
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventSourceFlagsState(state: i32) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSAppearanceNameDarkAqua: *mut c_void;
}

fn native_window(ui: &HistoryWindow) -> Option<*mut c_void> {
    let raw = ui.window().window_handle().window_handle().ok()?.as_raw();
    if let RawWindowHandle::AppKit(handle) = raw {
        let window = unsafe { object(handle.ns_view.as_ptr(), c"window") };
        if !window.is_null() {
            return Some(window);
        }
    }
    None
}

fn configure_window_chrome(ui: &HistoryWindow) {
    let Some(window) = native_window(ui) else {
        return;
    };
    // Native title-bar controls retain AppKit's accessibility, hover states,
    // minimize, and full-screen behavior. The Slint content stays below them.
    unsafe {
        let set_bool: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as *const ());
        set_bool(
            window,
            sel_registerName(c"setTitlebarAppearsTransparent:".as_ptr()),
            true,
        );
        let set_integer: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) =
            std::mem::transmute(objc_msgSend as *const ());
        set_integer(window, sel_registerName(c"setTitleVisibility:".as_ptr()), 1);
        let send_object: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
        let appearance = send_object(
            objc_getClass(c"NSAppearance".as_ptr()),
            sel_registerName(c"appearanceNamed:".as_ptr()),
            NSAppearanceNameDarkAqua,
        );
        let set_object: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        if !appearance.is_null() {
            set_object(
                window,
                sel_registerName(c"setAppearance:".as_ptr()),
                appearance,
            );
        }
        let color: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            f64,
            f64,
            f64,
            f64,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
        let background = color(
            objc_getClass(c"NSColor".as_ptr()),
            sel_registerName(c"colorWithSRGBRed:green:blue:alpha:".as_ptr()),
            17.0 / 255.0,
            20.0 / 255.0,
            25.0 / 255.0,
            1.0,
        );
        set_object(
            window,
            sel_registerName(c"setBackgroundColor:".as_ptr()),
            background,
        );
    }
}

fn restore_window(ui: &HistoryWindow) {
    if let Some(window) = native_window(ui) {
        unsafe {
            let send: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(objc_msgSend as *const ());
            send(
                window,
                sel_registerName(c"deminiaturize:".as_ptr()),
                std::ptr::null_mut(),
            );
        }
    }
}
unsafe fn object(receiver: *mut c_void, selector: &CStr) -> *mut c_void {
    unsafe {
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        send(receiver, sel_registerName(selector.as_ptr()))
    }
}
fn frontmost_pid() -> i32 {
    unsafe {
        let workspace = object(objc_getClass(c"NSWorkspace".as_ptr()), c"sharedWorkspace");
        let app = object(workspace, c"frontmostApplication");
        if app.is_null() {
            return 0;
        }
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(objc_msgSend as *const ());
        send(app, sel_registerName(c"processIdentifier".as_ptr()))
    }
}
fn activate(pid: i32) {
    unsafe {
        let lookup: unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let app = lookup(
            objc_getClass(c"NSRunningApplication".as_ptr()),
            sel_registerName(c"runningApplicationWithProcessIdentifier:".as_ptr()),
            pid,
        );
        let activate: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> bool =
            std::mem::transmute(objc_msgSend as *const ());
        let _ = activate(app, sel_registerName(c"activateWithOptions:".as_ptr()), 2);
    }
}
fn accessory_app() {
    unsafe {
        let app = object(
            objc_getClass(c"NSApplication".as_ptr()),
            c"sharedApplication",
        );
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> bool =
            std::mem::transmute(objc_msgSend as *const ());
        let _ = send(app, sel_registerName(c"setActivationPolicy:".as_ptr()), 1);
    }
}
fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}
fn modifiers_released() -> bool {
    unsafe { CGEventSourceFlagsState(0) & 0x1e0000 == 0 }
}
fn send_paste() -> bool {
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 9, true);
        let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 9, false);
        if down.is_null() || up.is_null() {
            if !down.is_null() {
                CFRelease(down);
            }
            if !up.is_null() {
                CFRelease(up);
            }
            return false;
        }
        CGEventSetFlags(down, 0x100000);
        CGEventSetFlags(up, 0x100000);
        CGEventPost(0, down);
        CGEventPost(0, up);
        CFRelease(down);
        CFRelease(up);
        true
    }
}
fn drag_window(ui: &HistoryWindow) {
    unsafe {
        if let Ok(handle) = ui.window().window_handle().window_handle() {
            if let RawWindowHandle::AppKit(handle) = handle.as_raw() {
                let window = object(handle.ns_view.as_ptr(), c"window");
                let app = object(
                    objc_getClass(c"NSApplication".as_ptr()),
                    c"sharedApplication",
                );
                let event = object(app, c"currentEvent");
                if !event.is_null() {
                    let send: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                        std::mem::transmute(objc_msgSend as *const ());
                    send(
                        window,
                        sel_registerName(c"performWindowDragWithEvent:".as_ptr()),
                        event,
                    );
                }
            }
        }
    }
}
pub fn fatal(message: &str) {
    eprintln!("{message}");
    if let Ok(quoted) = serde_json::to_string(message) {
        let _ = Process::new("/usr/bin/osascript")
            .arg("-e")
            .arg(format!(
                "display alert \"SnipTory\" message {quoted} as critical"
            ))
            .status();
    }
}

fn wire(ui: &HistoryWindow, app: &Rc<App>) -> Result<()> {
    let a = app.clone();
    ui.on_capture(move || {
        let _ = a.handle(Command::Action(Action::Capture));
    });
    let a = app.clone();
    ui.on_dismiss(move || a.hide());
    let a = app.clone();
    ui.window().on_close_requested(move || {
        a.hide();
        slint::CloseRequestResponse::KeepWindowShown
    });
    let a = app.clone();
    ui.on_preview(move |index| a.preview(index));
    let a = app.clone();
    ui.on_paste(move |index| {
        if let Some(entry) = a.entries.borrow().get(index as usize) {
            let paste = a.preferences.lock().unwrap().paste_on_select;
            let _ = a
                .jobs
                .send(Job::Paste(entry.path.clone(), a.target.get(), paste));
        }
    });
    let a = app.clone();
    ui.on_view_capture(move |index| {
        if let Some(entry) = a.entries.borrow().get(index as usize) {
            let _ = Process::new("/usr/bin/open").arg(&entry.path).spawn();
        }
    });
    let a = app.clone();
    ui.on_delete_entry(move |id| {
        let _ = a.jobs.send(Job::Delete(id.to_string()));
    });
    let a = app.clone();
    ui.on_clear_history(move || {
        let _ = a.jobs.send(Job::Clear);
    });
    let a = app.clone();
    ui.on_more(move || {
        a.limit.set(a.limit.get() + 60);
        let _ = a.jobs.send(Job::Load(a.limit.get()));
    });
    let a = app.clone();
    ui.on_navigate(move |direction| {
        if let Some(ui) = a.ui.upgrade() {
            let count = ui.get_captures().row_count() as i32;
            if count > 0 {
                let index = ui.get_selected();
                let next = if index < 0 {
                    0
                } else {
                    index
                        + match direction.as_str() {
                            "left" => -1,
                            "right" => 1,
                            "up" => -3,
                            _ => 3,
                        }
                };
                ui.set_selected(next.clamp(0, count - 1));
            }
        }
    });
    let a = app.clone();
    ui.on_open_folder(move || {
        let _ = a.handle(Command::Action(Action::Folder));
    });
    let a = app.clone();
    ui.on_restart_app(move || {
        let _ = a.handle(Command::Action(Action::Restart));
    });
    let a = app.clone();
    ui.on_setting_changed(move |key, value| {
        let mut settings = a.preferences.lock().unwrap().clone();
        match key.as_str() {
            "startup" => settings.launch_at_sign_in = value,
            "updates" => settings.automatic_updates = value,
            "paste" => settings.paste_on_select = value,
            _ => return,
        };
        let _ = a.jobs.send(Job::Settings(settings));
    });
    let a = app.clone();
    ui.on_configure_shortcuts(move |s, h, f, g| {
        let result = (|| -> Result<()> {
            let shortcuts = Shortcuts {
                screenshot: s.into(),
                history: h.into(),
                full: f.into(),
                gif: g.into(),
            }
            .normalized()?;
            a.configure_keys(&shortcuts)?;
            let mut settings = a.preferences.lock().unwrap().clone();
            settings.shortcuts = shortcuts;
            a.jobs.send(Job::Settings(settings))?;
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(ui) = a.ui.upgrade() {
                ui.set_shortcuts_busy(false);
                ui.set_shortcut_detail(error.to_string().into());
            }
        }
    });
    let updater = crate::updates::start(ui.as_weak(), Bridge, app.preferences.clone())?;
    let tx = updater.clone();
    ui.on_check_updates(move || {
        let _ = tx.try_send(true);
    });
    ui.on_update_action(move || {
        let _ = updater.try_send(false);
    });
    let w = ui.as_weak();
    ui.on_drag_window(move || {
        if let Some(ui) = w.upgrade() {
            drag_window(&ui);
        }
    });
    ui.on_tray_menu(|| {});
    Ok(())
}

impl App {
    fn status(&self, text: impl Into<String>) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_status(text.into().into());
        }
    }
    fn show(self: &Rc<Self>, options: bool) -> Result<()> {
        let pid = frontmost_pid();
        if pid > 0 && pid != std::process::id() as i32 {
            self.target.set(pid);
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_show_options(options);
            ui.invoke_prepare();
            ui.show()?;
            configure_window_chrome(&ui);
            restore_window(&ui);
            activate(std::process::id() as i32);
        }
        let weak = Rc::downgrade(self);
        self.focus_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(150),
            move || {
                if let Some(app) = weak.upgrade() {
                    if !app.capture_busy.get() {
                        let pid = frontmost_pid();
                        if pid > 0 && pid != std::process::id() as i32 {
                            app.target.set(pid);
                        }
                    }
                }
            },
        );
        self.jobs.send(Job::Load(self.limit.get()))?;
        Ok(())
    }
    fn hide(&self) {
        self.preview(-1);
        self.focus_timer.stop();
        if let Some(ui) = self.ui.upgrade() {
            let _ = ui.hide();
        }
    }
    fn preview(&self, index: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_hovered(index);
        let previous = self.poster.borrow_mut().take();
        if let Some((index, id, image)) = previous {
            let model = ui.get_captures();
            if let Some(mut row) = model.row_data(index) {
                if row.id.as_str() == id {
                    row.preview = image;
                    model.set_row_data(index, row);
                }
            }
        }
        let entry = self.entries.borrow().get(index as usize).cloned();
        if let Some(entry) = entry {
            if entry.duration_ms.is_some() {
                if let Some(row) = ui.get_captures().row_data(index as usize) {
                    *self.poster.borrow_mut() =
                        Some((index as usize, entry.id.clone(), row.preview));
                    let _ = self.preview.send(crate::preview::Request::Play {
                        index,
                        id: entry.id,
                        path: entry.path,
                    });
                    return;
                }
            }
        }
        let _ = self.preview.send(crate::preview::Request::Stop);
    }
    fn configure_keys(&self, shortcuts: &Shortcuts) -> Result<()> {
        let next = shortcuts
            .bindings()?
            .into_iter()
            .take(3)
            .map(|b| {
                b.label
                    .replace("Win+", "Meta+")
                    .parse::<HotKey>()
                    .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()?;
        let old = self.keys.borrow().clone();
        if next == old {
            return Ok(());
        }
        self.manager.unregister_all(&old)?;
        for key in &next {
            if let Err(error) = self.manager.register(*key) {
                for key in &next {
                    let _ = self.manager.unregister(*key);
                }
                if let Err(restore) = self.manager.register_all(&old) {
                    bail!(
                        "Shortcut conflict: {error}. Previous bindings could not be restored: {restore}"
                    );
                }
                bail!("Shortcut unavailable: {error}. Previous bindings restored.");
            }
        }
        *self.keys.borrow_mut() = next;
        Ok(())
    }
    fn update_menu(&self) {
        let settings = self.preferences.lock().unwrap();
        self.menu_history
            .set_text(format!("Open history    {}", settings.shortcuts.history));
        self.menu_capture
            .set_text(format!("Screenshot    {}", settings.shortcuts.screenshot));
        self.menu_full
            .set_text(format!("Full screenshot    {}", settings.shortcuts.full));
    }
    fn handle(self: &Rc<Self>, command: Command) -> Result<()> {
        match command {
            Command::Action(action) => {
                if self.exiting.get() {
                    return Ok(());
                }
                match action {
                    Action::History => self.show(false)?,
                    Action::Options => self.show(true)?,
                    Action::Capture | Action::Full => {
                        if self.capture_busy.replace(true) {
                            return Ok(());
                        }
                        self.status("Select a region · Escape to cancel");
                        self.jobs
                            .send(Job::Capture(matches!(action, Action::Full)))?;
                    }
                    Action::Folder => {
                        Process::new("/usr/bin/open")
                            .arg(self.root.join("captures"))
                            .spawn()?;
                    }
                    Action::Quit | Action::Restart => {
                        if self.capture_busy.get() {
                            self.status("Finish or cancel the current capture first");
                            return Ok(());
                        }
                        self.exiting.set(true);
                        self.jobs.send(if matches!(action, Action::Restart) {
                            Job::Restart
                        } else {
                            Job::Quit
                        })?;
                    }
                }
            }
            Command::Hotkey(id) => {
                let index = self.keys.borrow().iter().position(|key| key.id() == id);
                if let Some(index) = index {
                    self.handle(Command::Action(
                        [Action::Capture, Action::History, Action::Full][index],
                    ))?;
                }
            }
            Command::Gallery(entries, previews, has_more) => {
                self.preview(-1);
                let model = VecModel::default();
                for (entry, image) in entries.iter().zip(previews) {
                    let (w, h) = image.dimensions();
                    let date = chrono::DateTime::from_timestamp(entry.timestamp, 0)
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%b %-d · %-I:%M %p")
                                .to_string()
                        })
                        .unwrap_or_else(|| "Capture".into());
                    model.push(crate::CaptureItem {
                        id: entry.id.clone().into(),
                        preview: slint::Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                            image.as_raw(),
                            w,
                            h,
                        )),
                        label: date.into(),
                        dimensions: format!(
                            "{} × {} · {}",
                            entry.width,
                            entry.height,
                            if entry.duration_ms.is_some() {
                                "GIF"
                            } else {
                                "PNG"
                            }
                        )
                        .into(),
                        animated: entry.duration_ms.is_some(),
                    });
                }
                *self.entries.borrow_mut() = entries;
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_captures(ModelRc::new(model));
                    ui.set_selected(-1);
                    ui.set_has_more(has_more);
                    ui.set_history_busy(false);
                }
            }
            Command::Status(message) => self.status(message),
            Command::CaptureFinished => {
                self.capture_busy.set(false);
                self.status("Ready");
            }
            Command::SavedSettings(settings, error) => {
                if let Err(error) = self.configure_keys(&settings.shortcuts) {
                    self.status(format!("{error:#}"));
                }
                self.update_menu();
                if let Some(ui) = self.ui.upgrade() {
                    apply_preferences(&ui, &settings);
                    ui.set_shortcuts_busy(false);
                    ui.set_shortcut_detail(
                        error.unwrap_or_else(|| "Preferences saved".into()).into(),
                    );
                }
            }
            Command::PasteReady(target) => self.paste_back(target),
            Command::ApplyUpdate(manager, asset) => {
                if self.capture_busy.get() {
                    if let Some(ui) = self.ui.upgrade() {
                        ui.set_update_busy(false);
                        ui.set_update_label("Update".into());
                        ui.set_update_detail(
                            "Finish or cancel the current capture, then click Update.".into(),
                        );
                    }
                    return Ok(());
                }
                self.exiting.set(true);
                self.jobs.send(Job::Apply(manager, asset))?;
            }
            Command::ReadyToQuit => {
                self.hide();
                slint::quit_event_loop()?;
            }
            Command::ReadyToRestart => {
                let bundle = bundle_path()?;
                Process::new("/bin/sh")
                    .args([
                        "-c",
                        "sleep 1; exec /usr/bin/open -n \"$1\"",
                        "sniptory-restart",
                    ])
                    .arg(bundle)
                    .spawn()?;
                self.hide();
                slint::quit_event_loop()?;
            }
        }
        Ok(())
    }
    fn paste_back(self: &Rc<Self>, target: i32) {
        if target <= 0 || target == std::process::id() as i32 || !accessibility_trusted() {
            self.status("Copied. Press ⌘V in your app, or allow SnipTory in Accessibility for automatic paste.");
            return;
        }
        activate(target);
        let started = Instant::now();
        let weak = Rc::downgrade(self);
        self.paste_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(30),
            move || {
                if let Some(app) = weak.upgrade() {
                    let focused = frontmost_pid() == target;
                    if focused && modifiers_released() {
                        app.paste_timer.stop();
                        if send_paste() {
                            app.status("Pasted to your app");
                        } else {
                            app.status("Copied. Press ⌘V in your app.");
                        }
                    } else if started.elapsed() > Duration::from_secs(2) {
                        app.paste_timer.stop();
                        app.status("Copied. Focus your app and press ⌘V.");
                    }
                }
            },
        );
    }
}

pub fn run() -> Result<()> {
    let root = settings::root()?;
    fs::create_dir_all(&root)?;
    let instance = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("instance.lock"))?;
    if instance.try_lock_exclusive().is_err() {
        let _ = std::os::unix::net::UnixStream::connect(root.join("instance.sock"));
        return Ok(());
    }
    let _ = fs::remove_file(root.join("instance.sock"));
    let listener = std::os::unix::net::UnixListener::bind(root.join("instance.sock"))?;
    std::thread::Builder::new()
        .name("sniptory-instance".into())
        .spawn(move || {
            for connection in listener.incoming() {
                if connection.is_ok() {
                    Bridge.send(Command::Action(Action::History));
                } else {
                    break;
                }
            }
        })?;
    let preferences = Arc::new(Mutex::new(settings::load(&root)));
    let ui = HistoryWindow::new()?;
    ui.set_release_version(crate::updates::RELEASE.trim().into());
    ui.set_ui_font_family(".AppleSystemUIFont".into());
    ui.set_native_title_drag(true);
    ui.set_native_window_controls(true);
    ui.set_gif_recording_supported(false);
    ui.set_shortcut_help("Type a combination, e.g. Cmd+Shift+S.\nUse Control, Option or Command with a letter, number or F1–F24.".into());
    apply_preferences(&ui, &preferences.lock().unwrap());
    let (jobs, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker_root = root.clone();
    let worker_prefs = preferences.clone();
    std::thread::Builder::new()
        .name("sniptory-storage".into())
        .spawn(move || {
            let history = match History::open(worker_root.clone()) {
                Ok(h) => {
                    let _ = ready_tx.send(Ok(()));
                    h
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e:#}")));
                    return;
                }
            };
            while let Ok(job) = rx.recv() {
                let result: Result<()> = (|| {
                    match job {
                        Job::Load(limit) => load_gallery(&history, limit)?,
                        Job::Capture(full) => {
                            let result = capture(&history, &worker_root, full);
                            Bridge.send(Command::CaptureFinished);
                            if let Some(message) = result? {
                                Bridge.send(Command::Status(message));
                                load_gallery(&history, 60)?;
                            }
                        }
                        Job::Paste(path, target, paste) => {
                            copy_capture(&path)?;
                            if paste {
                                Bridge.send(Command::PasteReady(target));
                            } else {
                                Bridge.send(Command::Status("Copied to clipboard".into()));
                            }
                        }
                        Job::Delete(id) => {
                            let result = history.delete(&id);
                            let refresh = load_gallery(&history, 60);
                            result?;
                            refresh?;
                            Bridge.send(Command::Status("Capture deleted".into()));
                        }
                        Job::Clear => {
                            let result = history.clear();
                            let refresh = load_gallery(&history, 60);
                            let count = result?;
                            refresh?;
                            Bridge.send(Command::Status(format!(
                                "History cleared · {count} captures deleted"
                            )));
                        }
                        Job::Settings(settings) => {
                            let old = settings::load(&worker_root);
                            match startup(settings.launch_at_sign_in)
                                .and_then(|_| settings::save(&worker_root, &settings))
                            {
                                Ok(()) => {
                                    *worker_prefs.lock().unwrap() = settings.clone();
                                    Bridge.send(Command::SavedSettings(settings, None));
                                }
                                Err(error) => {
                                    let _ = startup(old.launch_at_sign_in);
                                    *worker_prefs.lock().unwrap() = old.clone();
                                    Bridge.send(Command::SavedSettings(
                                        old,
                                        Some(format!("{error:#}")),
                                    ));
                                }
                            }
                        }
                        Job::Apply(manager, asset) => {
                            match manager.wait_exit_then_apply_updates(
                                &asset,
                                true,
                                true,
                                Vec::<String>::new(),
                            ) {
                                Ok(()) => Bridge.send(Command::ReadyToQuit),
                                Err(error) => {
                                    let detail = format!("Could not apply update: {error}");
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(app) = APP.with(|a| a.borrow().clone()) {
                                            app.exiting.set(false);
                                            if let Some(ui) = app.ui.upgrade() {
                                                ui.set_update_busy(false);
                                                ui.set_update_label("Retry update".into());
                                                ui.set_update_detail(detail.into());
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        Job::Quit => Bridge.send(Command::ReadyToQuit),
                        Job::Restart => Bridge.send(Command::ReadyToRestart),
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    Bridge.send(Command::Status(format!("{error:#}")));
                    let _ = slint::invoke_from_event_loop(|| {
                        if let Some(app) = APP.with(|a| a.borrow().clone()) {
                            if let Some(ui) = app.ui.upgrade() {
                                ui.set_history_busy(false);
                            }
                        }
                    });
                }
            }
        })?;
    ready_rx.recv()?.map_err(anyhow::Error::msg)?;
    let initial_target = frontmost_pid();
    ui.show()?;
    configure_window_chrome(&ui);
    accessory_app();
    let menu = Menu::new();
    let menu_history = MenuItem::with_id("history", "Open history", true, None);
    let menu_capture = MenuItem::with_id("capture", "Screenshot", true, None);
    let menu_full = MenuItem::with_id("full", "Full screenshot", true, None);
    let options = MenuItem::with_id("options", "Options", true, None);
    let quit = MenuItem::with_id("quit", "Quit SnipTory", true, None);
    menu.append_items(&[
        &menu_history,
        &menu_capture,
        &menu_full,
        &options,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("SnipTory")
        .with_icon_templated(tray_glyph()?)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    let app = Rc::new(App {
        ui: ui.as_weak(),
        jobs,
        root: root.clone(),
        preferences: preferences.clone(),
        entries: RefCell::new(Vec::new()),
        limit: Cell::new(60),
        manager: GlobalHotKeyManager::new()?,
        keys: RefCell::new(Vec::new()),
        menu_history,
        menu_capture,
        menu_full,
        _tray: tray,
        capture_busy: Cell::new(false),
        exiting: Cell::new(false),
        target: Cell::new(initial_target),
        focus_timer: slint::Timer::default(),
        paste_timer: slint::Timer::default(),
        preview: crate::preview::start(ui.as_weak())?,
        poster: RefCell::new(None),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    if let Err(error) = app.configure_keys(&preferences.lock().unwrap().shortcuts) {
        app.status(format!("{error:#}"));
    }
    app.update_menu();
    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Released {
            Bridge.send(Command::Hotkey(event.id));
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            Bridge.send(Command::Action(Action::History));
        }
    }));
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        let action = match event.id.as_ref() {
            "history" => Action::History,
            "capture" => Action::Capture,
            "full" => Action::Full,
            "options" => Action::Options,
            "quit" => Action::Quit,
            _ => return,
        };
        Bridge.send(Command::Action(action));
    }));
    wire(&ui, &app)?;
    app.jobs.send(Job::Load(60))?;
    if std::env::args().any(|a| a == "--background") {
        ui.hide()?;
    } else {
        app.show(false)?;
    }
    slint::run_event_loop_until_quit()?;
    app.focus_timer.stop();
    app.paste_timer.stop();
    let _ = app.preview.send(crate::preview::Request::Stop);
    let _ = app.manager.unregister_all(&app.keys.borrow());
    APP.with(|a| a.borrow_mut().take());
    drop(instance);
    Ok(())
}
