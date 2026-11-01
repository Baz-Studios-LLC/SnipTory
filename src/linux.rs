//! GNOME/Wayland frontend: portal capture and shortcuts; no X11 input injection.
use crate::HistoryWindow;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use sniptory::{
    history::{Entry, History},
    hotkeys::Shortcuts,
    settings::{self, Settings},
};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

pub enum Command {
    ApplyUpdate(velopack::UpdateManager, velopack::VelopackAsset),
    Action(String),
}
#[derive(Clone)]
pub struct Bridge {
    jobs: mpsc::Sender<Job>,
}
impl Bridge {
    pub fn send(&self, command: Command) {
        match command {
            Command::ApplyUpdate(manager, asset) => {
                let _ = self.jobs.send(Job::Apply(manager, asset));
            }
            Command::Action(action) => {
                let _ = slint::invoke_from_event_loop(move || {
                    APP.with(|app| {
                        if let Some(app) = app.borrow().as_ref() {
                            app.action(&action);
                        }
                    })
                });
            }
        }
    }
}
enum Job {
    Load(usize),
    Capture(bool),
    Copy(PathBuf),
    Delete(String),
    Clear,
    Settings(Settings),
    Folder(Option<PathBuf>),
    Apply(velopack::UpdateManager, velopack::VelopackAsset),
    Restart,
}
struct App {
    ui: slint::Weak<HistoryWindow>,
    jobs: mpsc::Sender<Job>,
    entries: RefCell<Vec<Entry>>,
    limit: Cell<usize>,
    capture_busy: Cell<bool>,
    preview: mpsc::Sender<crate::preview::Request>,
    poster: RefCell<Option<(usize, slint::Image)>>,
}
thread_local! {static APP:RefCell<Option<Rc<App>>>=const{RefCell::new(None)};}
fn view(callback: impl FnOnce(&HistoryWindow) + 'static + Send) {
    let _ = slint::invoke_from_event_loop(move || {
        APP.with(|app| {
            if let Some(ui) = app.borrow().as_ref().and_then(|a| a.ui.upgrade()) {
                callback(&ui);
            }
        })
    });
}
fn status(message: impl Into<String>) {
    let message = message.into();
    view(move |ui| ui.set_status(message.into()));
}
fn preferences(ui: &HistoryWindow, p: &Settings, root: &Path) {
    ui.set_launch_at_sign_in(p.launch_at_sign_in);
    ui.set_automatic_updates(p.automatic_updates);
    ui.set_paste_on_select(false);
    ui.set_capture_folder(p.capture_root(root).to_string_lossy().as_ref().into());
    ui.set_capture_folder_custom(p.capture_folder.is_some());
    ui.set_screenshot_hotkey(p.shortcuts.screenshot.clone().into());
    ui.set_history_hotkey(p.shortcuts.history.clone().into());
    ui.set_full_hotkey(p.shortcuts.full.clone().into());
    ui.set_gif_hotkey(p.shortcuts.gif.clone().into());
}
fn executable() -> Result<PathBuf> {
    Ok(std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?))
}
fn open(path: &Path) -> Result<()> {
    ensure!(path.exists(), "This file or folder is unavailable");
    Process::new("xdg-open").arg(path).spawn()?;
    Ok(())
}
fn copy(path: &Path) -> Result<()> {
    let bundled = std::env::current_exe()?
        .parent()
        .context("Missing app directory")?
        .join("wl-copy");
    let tool = if bundled.is_file() {
        bundled
    } else {
        PathBuf::from("wl-copy")
    };
    let mime = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        "image/gif"
    } else {
        "image/png"
    };
    let mut child = Process::new(tool)
        .args(["--type", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start Wayland clipboard helper")?;
    let bytes = fs::read(path)?;
    child
        .stdin
        .take()
        .context("Clipboard pipe unavailable")?
        .write_all(&bytes)?;
    ensure!(
        child.wait()?.success(),
        "Could not copy image to the Wayland clipboard"
    );
    Ok(())
}
fn install_launcher() -> Result<()> {
    let Some(appimage) = std::env::var_os("APPIMAGE") else {
        return Ok(());
    };
    let exe = PathBuf::from(appimage)
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    ensure!(!exe.contains(['\n', '\r']), "Unsupported application path");
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(
            PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
                .join(".local/share"),
        );
    let apps = base.join("applications");
    let icons = base.join("icons/hicolor/256x256/apps");
    fs::create_dir_all(&apps)?;
    fs::create_dir_all(&icons)?;
    fs::write(
        icons.join("com.bazstudios.Sniptory.png"),
        include_bytes!("../assets/sniptory.png"),
    )?;
    fs::write(
        apps.join("com.bazstudios.Sniptory.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=SnipTory\nComment=Capture. Recall. Paste.\nExec=env APPIMAGE_EXTRACT_AND_RUN=1 \"{exe}\" --history\nIcon=com.bazstudios.Sniptory\nTerminal=false\nCategories=Utility;Graphics;\n"
        ),
    )?;
    Ok(())
}
fn startup(enabled: bool) -> Result<()> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(
            PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?).join(".config"),
        );
    let folder = config.join("autostart");
    let path = folder.join("com.bazstudios.Sniptory.desktop");
    if enabled {
        let exe = executable()?
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%");
        ensure!(!exe.contains(['\n', '\r']), "Unsupported application path");
        fs::create_dir_all(folder)?;
        fs::write(
            path,
            format!(
                "[Desktop Entry]\nType=Application\nName=SnipTory\nExec=env APPIMAGE_EXTRACT_AND_RUN=1 \"{exe}\" --background\nTerminal=false\n"
            ),
        )?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
async fn screenshot(history: &History, full: bool) -> Result<()> {
    let response = ashpd::desktop::screenshot::Screenshot::request()
        .interactive(!full)
        .modal(false)
        .send()
        .await?
        .response()?;
    let path = response
        .uri()
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("Screenshot portal returned a non-local image"))?;
    let pixels = image::open(&path)?.to_rgba8();
    history.save(&pixels)?;
    // Clipboard conversion uses our durable copy; the portal owns its source URI.
    let entry = history.list(1)?.remove(0);
    copy(&entry.path)?;
    Ok(())
}
fn gallery(history: &History, limit: usize) -> Result<()> {
    let all = history.list(limit + 1)?;
    let more = all.len() > limit;
    let items = all
        .into_iter()
        .take(limit)
        .filter_map(|e| {
            if e.path.is_file() {
                history.preview(&e).ok().map(|image| (e, image))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let _ = slint::invoke_from_event_loop(move || {
        APP.with(|app| {
            let binding = app.borrow();
            let Some(app) = binding.as_ref() else { return };
            let Some(ui) = app.ui.upgrade() else { return };
            app.stop_preview();
            let model = VecModel::default();
            let mut entries = Vec::new();
            for (e, image) in items {
                let label = chrono::DateTime::from_timestamp(e.timestamp, 0)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%b %-d · %-I:%M %p")
                            .to_string()
                    })
                    .unwrap_or_else(|| "Capture".into());
                model.push(crate::CaptureItem {
                    id: e.id.clone().into(),
                    preview: slint::Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                        image.as_raw(),
                        image.width(),
                        image.height(),
                    )),
                    label: label.into(),
                    dimensions: format!(
                        "{} × {} · {}",
                        e.width,
                        e.height,
                        if e.duration_ms.is_some() {
                            "GIF"
                        } else {
                            "PNG"
                        }
                    )
                    .into(),
                    animated: e.duration_ms.is_some(),
                });
                entries.push(e);
            }
            *app.entries.borrow_mut() = entries;
            ui.set_captures(ModelRc::new(model));
            ui.set_selected(-1);
            ui.set_has_more(more);
            ui.set_history_busy(false);
        })
    });
    Ok(())
}
impl App {
    fn stop_preview(&self) {
        let _ = self.preview.send(crate::preview::Request::Stop);
        if let Some((index, image)) = self.poster.borrow_mut().take() {
            if let Some(ui) = self.ui.upgrade() {
                let model = ui.get_captures();
                if let Some(mut item) = model.row_data(index) {
                    item.preview = image;
                    model.set_row_data(index, item);
                }
            }
        }
    }
    fn action(&self, action: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        match action {
            "capture" | "full" => {
                if self.capture_busy.replace(true) {
                    return;
                }
                self.stop_preview();
                ui.set_status("Select a capture in GNOME’s screenshot picker".into());
                let _ = self.jobs.send(Job::Capture(action == "full"));
            }
            "quit" => {
                let _ = slint::quit_event_loop();
            }
            "restart" => {
                let _ = self.jobs.send(Job::Restart);
            }
            _ => {
                ui.set_show_options(action == "options");
                ui.invoke_prepare();
                let _ = ui.show();
                let _ = self.jobs.send(Job::Load(self.limit.get()));
            }
        }
    }
}
fn trigger(text: &str) -> Result<String> {
    let parsed = sniptory::hotkeys::parse(text)?;
    Ok(parsed
        .label
        .replace("Ctrl+", "CTRL+")
        .replace("Alt+", "ALT+")
        .replace("Shift+", "SHIFT+")
        .replace("Win+", "LOGO+"))
}
async fn shortcuts(keys: Shortcuts, bridge: Bridge) -> Result<()> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(Default::default()).await?;
    let mut events = portal.receive_activated().await?;
    let descriptors = [
        ("capture", "Screenshot", keys.screenshot),
        ("history", "Open history", keys.history),
        ("full", "Full screenshot", keys.full),
    ]
    .into_iter()
    .map(|(id, label, key)| {
        Ok(NewShortcut::new(id, label).preferred_trigger(trigger(&key)?.as_str()))
    })
    .collect::<Result<Vec<_>>>()?;
    portal
        .bind_shortcuts(&session, &descriptors, None, Default::default())
        .await?
        .response()?;
    view(|ui| {
        ui.set_shortcut_detail(
            "Shortcuts registered with GNOME. GNOME controls shortcut approval and remapping."
                .into(),
        )
    });
    while let Some(event) = events.next().await {
        bridge.send(Command::Action(event.shortcut_id().into()));
    }
    Ok(())
}
pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    if std::env::args().any(|a| a == "--smoke-test") {
        println!(
            "SnipTory {} Linux frontend loaded",
            crate::updates::RELEASE.trim()
        );
        return Ok(());
    }
    install_launcher()?;
    let root = settings::root()?;
    fs::create_dir_all(&root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("instance.lock"))?;
    let action = if std::env::args().any(|a| a == "--capture") {
        "capture"
    } else if std::env::args().any(|a| a == "--full") {
        "full"
    } else {
        "history"
    };
    let socket = root.join("instance.sock");
    let mut acquired = lock.try_lock_exclusive().is_ok();
    if !acquired && std::env::args().any(|a| a == "--restarting") {
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            if lock.try_lock_exclusive().is_ok() {
                acquired = true;
                break;
            }
        }
    }
    if !acquired {
        let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
        stream.write_all(action.as_bytes())?;
        return Ok(());
    }
    let _ = fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let prefs = Arc::new(Mutex::new(settings::load(&root)));
    let ui = HistoryWindow::new()?;
    ui.set_ui_font_family("Cantarell".into());
    ui.set_native_window_controls(true);
    ui.set_gif_recording_supported(false);
    ui.set_paste_supported(false);
    ui.set_show_quit(true);
    ui.set_release_version(crate::updates::RELEASE.trim().into());
    ui.set_startup_detail("Ready when you sign in to GNOME".into());
    ui.set_shortcut_help("GNOME asks you to approve global shortcuts.\nChanges take effect after restarting SnipTory.".into());
    preferences(&ui, &prefs.lock().unwrap(), &root);
    let (jobs, rx) = mpsc::channel();
    let bridge = Bridge { jobs: jobs.clone() };
    let app = Rc::new(App {
        ui: ui.as_weak(),
        jobs: jobs.clone(),
        entries: RefCell::new(Vec::new()),
        limit: Cell::new(60),
        capture_busy: Cell::new(false),
        preview: crate::preview::start(ui.as_weak())?,
        poster: RefCell::new(None),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    let b = bridge.clone();
    std::thread::spawn(move || {
        for mut connection in listener.incoming().flatten() {
            let mut bytes = [0u8; 32];
            if let Ok(count) = connection.read(&mut bytes) {
                let value = String::from_utf8_lossy(&bytes[..count]);
                b.send(Command::Action(
                    if ["capture", "full"].contains(&value.as_ref()) {
                        value.into_owned()
                    } else {
                        "history".into()
                    },
                ));
            }
        }
    });
    let worker_root = root.clone();
    let worker_prefs = prefs.clone();
    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name("sniptory-storage".into())
        .spawn(move || {
            let history = match History::open(worker_root.clone()) {
                Ok(h) => h,
                Err(e) => {
                    status(format!("Could not open history: {e:#}"));
                    return;
                }
            };
            while let Ok(job) = rx.recv() {
                let result: Result<()> = (|| {
                    match job {
                        Job::Load(limit) => gallery(&history, limit)?,
                        Job::Capture(full) => {
                            let result = handle.block_on(screenshot(&history, full));
                            let _ = slint::invoke_from_event_loop(|| {
                                APP.with(|a| {
                                    if let Some(app) = a.borrow().as_ref() {
                                        app.capture_busy.set(false)
                                    }
                                })
                            });
                            gallery(&history, 60)?;
                            result?;
                            status("Screenshot saved and copied");
                        }
                        Job::Copy(path) => {
                            copy(&path)?;
                            status("Copied to clipboard · paste into your app");
                        }
                        Job::Delete(id) => {
                            let result = history.delete(&id);
                            gallery(&history, 60)?;
                            result?;
                            status("Capture deleted");
                        }
                        Job::Clear => {
                            let result = history.clear();
                            gallery(&history, 60)?;
                            result?;
                            status("History cleared");
                        }
                        Job::Settings(mut p) => {
                            let old = settings::load(&worker_root);
                            p.capture_folder = old.capture_folder;
                            p.paste_on_select = false;
                            let result = startup(p.launch_at_sign_in)
                                .and_then(|_| settings::save(&worker_root, &p));
                            let detail = match result {
                                Ok(()) => {
                                    *worker_prefs.lock().unwrap() = p.clone();
                                    "Preferences saved. Restart to apply shortcut changes."
                                        .to_string()
                                }
                                Err(error) => {
                                    let _ = startup(old.launch_at_sign_in);
                                    p = old;
                                    format!("{error:#}")
                                }
                            };
                            let root = worker_root.clone();
                            view(move |ui| {
                                preferences(ui, &p, &root);
                                ui.set_shortcuts_busy(false);
                                ui.set_shortcut_detail(detail.into());
                            });
                        }
                        Job::Folder(folder) => {
                            let result = history.change_capture_folder(folder);
                            let root = worker_root.clone();
                            if let Ok(p) = &result {
                                *worker_prefs.lock().unwrap() = p.clone();
                            }
                            view(move |ui| {
                                ui.set_folder_busy(false);
                                match result {
                                    Ok(p) => {
                                        preferences(ui, &p, &root);
                                        ui.set_folder_detail(
                                            "Save location updated. Existing files stay in place."
                                                .into(),
                                        );
                                    }
                                    Err(e) => ui.set_folder_detail(format!("{e:#}").into()),
                                }
                            });
                        }
                        Job::Apply(manager, asset) => {
                            match manager.wait_exit_then_apply_updates(
                                &asset,
                                true,
                                true,
                                Vec::<String>::new(),
                            ) {
                                Ok(()) => {
                                    let _ = slint::invoke_from_event_loop(|| {
                                        let _ = slint::quit_event_loop();
                                    });
                                }
                                Err(e) => view(move |ui| {
                                    ui.set_update_busy(false);
                                    ui.set_update_label("Retry update".into());
                                    ui.set_update_detail(
                                        format!("Could not apply update: {e}").into(),
                                    );
                                }),
                            }
                        }
                        Job::Restart => {
                            Process::new(executable()?).arg("--restarting").spawn()?;
                            let _ = slint::invoke_from_event_loop(|| {
                                let _ = slint::quit_event_loop();
                            });
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    status(format!("{e:#}"));
                    view(|ui| ui.set_history_busy(false));
                }
            }
        })?;
    let b = bridge.clone();
    let keys = prefs.lock().unwrap().shortcuts.clone();
    runtime.spawn(async move{if let Err(e)=shortcuts(keys,b).await{view(move|ui|ui.set_shortcut_detail(format!("GNOME shortcuts unavailable: {e}. Use the app launcher, or add custom shortcuts for --capture, --history and --full in GNOME Settings.").into()));}});
    let a = app.clone();
    ui.on_capture(move || a.action("capture"));
    let a = app.clone();
    ui.on_refresh_gallery(move || {
        let _ = a.jobs.send(Job::Load(a.limit.get()));
    });
    let a = app.clone();
    ui.on_more(move || {
        a.limit.set(a.limit.get() + 60);
        let _ = a.jobs.send(Job::Load(a.limit.get()));
    });
    let a = app.clone();
    ui.on_paste(move |index| {
        if let Some(e) = a.entries.borrow().get(index as usize) {
            let _ = a.jobs.send(Job::Copy(e.path.clone()));
        }
    });
    let a = app.clone();
    ui.on_view_capture(move |index| {
        if let Some(e) = a.entries.borrow().get(index as usize) {
            if let Err(error) = open(&e.path) {
                status(error.to_string());
            }
        }
    });
    let a = app.clone();
    ui.on_delete_entry(move |id| {
        a.stop_preview();
        let _ = a.jobs.send(Job::Delete(id.into()));
    });
    let a = app.clone();
    ui.on_clear_history(move || {
        a.stop_preview();
        let _ = a.jobs.send(Job::Clear);
    });
    let a = app.clone();
    ui.on_preview(move |index| {
        a.stop_preview();
        if index >= 0 {
            if let (Some(e), Some(ui)) = (a.entries.borrow().get(index as usize), a.ui.upgrade()) {
                if e.duration_ms.is_some() {
                    if let Some(item) = ui.get_captures().row_data(index as usize) {
                        *a.poster.borrow_mut() = Some((index as usize, item.preview));
                        let _ = a.preview.send(crate::preview::Request::Play {
                            index,
                            id: e.id.clone(),
                            path: e.path.clone(),
                        });
                    }
                }
            }
        }
    });
    let w = ui.as_weak();
    ui.on_navigate(move |direction| {
        if let Some(ui) = w.upgrade() {
            let n = ui.get_captures().row_count() as i32;
            if n > 0 {
                let step = match direction.as_str() {
                    "left" => -1,
                    "up" => -3,
                    "down" => 3,
                    _ => 1,
                };
                ui.set_selected((ui.get_selected() + step).clamp(0, n - 1));
            }
        }
    });
    let w = ui.as_weak();
    ui.on_dismiss(move || {
        if let Some(ui) = w.upgrade() {
            let _ = ui.hide();
        }
    });
    let a = app.clone();
    ui.on_restart_app(move || a.action("restart"));
    ui.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });
    let tx = jobs.clone();
    let p = prefs.clone();
    ui.on_setting_changed(move |key, value| {
        let mut p = p.lock().unwrap().clone();
        match key.as_str() {
            "startup" => p.launch_at_sign_in = value,
            "updates" => p.automatic_updates = value,
            _ => return,
        };
        let _ = tx.send(Job::Settings(p));
    });
    let tx = jobs.clone();
    let p = prefs.clone();
    ui.on_configure_shortcuts(move |s, h, f, g| {
        match (Shortcuts {
            screenshot: s.into(),
            history: h.into(),
            full: f.into(),
            gif: g.into(),
        })
        .normalized()
        {
            Ok(keys) => {
                let mut p = p.lock().unwrap().clone();
                p.shortcuts = keys;
                let _ = tx.send(Job::Settings(p));
            }
            Err(e) => view(move |ui| {
                ui.set_shortcuts_busy(false);
                ui.set_shortcut_detail(e.to_string().into());
            }),
        }
    });
    let r = root.clone();
    ui.on_open_folder(move || {
        if let Err(e) = open(&settings::load(&r).capture_root(&r)) {
            status(e.to_string());
        }
    });
    let r = root.clone();
    let tx = jobs.clone();
    let rt = runtime.handle().clone();
    ui.on_choose_capture_folder(move || {
        let folder = settings::load(&r).capture_root(&r);
        let tx = tx.clone();
        rt.spawn(async move {
            let chosen = rfd::AsyncFileDialog::new()
                .set_title("Choose capture folder")
                .set_directory(folder)
                .pick_folder()
                .await;
            if let Some(folder) = chosen {
                let _ = tx.send(Job::Folder(Some(folder.path().to_path_buf())));
            } else {
                view(|ui| ui.set_folder_busy(false));
            }
        });
    });
    let tx = jobs.clone();
    ui.on_reset_capture_folder(move || {
        let _ = tx.send(Job::Folder(None));
    });
    let updater = crate::updates::start(ui.as_weak(), bridge, prefs)?;
    let tx = updater.clone();
    ui.on_check_updates(move || {
        let _ = tx.try_send(true);
    });
    ui.on_update_action(move || {
        let _ = updater.try_send(false);
    });
    let w = ui.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(ui) = w.upgrade() {
            let _ = ui.hide();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    jobs.send(Job::Load(60))?;
    // Construct the window even for background mode so IPC can reopen it.
    ui.show()?;
    if std::env::args().any(|a| a == "--background") {
        ui.hide()?;
    } else {
        app.action(action);
    }
    slint::run_event_loop_until_quit()?;
    app.stop_preview();
    APP.with(|a| a.borrow_mut().take());
    drop(lock);
    let _ = fs::remove_file(socket);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_shortcut_notation() {
        assert_eq!(trigger("Alt+Shift+S").unwrap(), "ALT+SHIFT+S");
        assert_eq!(trigger("Ctrl+Win+F12").unwrap(), "CTRL+LOGO+F12");
        assert!(trigger("Shift+S").is_err());
    }
}
