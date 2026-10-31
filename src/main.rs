#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod history;
mod native;
mod preview;
mod recording;
mod settings;
mod thumbnail;
mod updates;

use anyhow::Result;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc};

slint::include_modules!();

thread_local! {
    static STATUS_RESET: slint::Timer = slint::Timer::default();
}

pub enum StoreCommand {
    Save(image::RgbaImage),
    SaveGif(recording::RecordedGif),
    Load(usize),
    Delete(String),
    ClearHistory,
    Paste(PathBuf),
    ApplyUpdate(velopack::UpdateManager, velopack::VelopackAsset),
    Settings(settings::Settings),
    Restart,
    Exit,
}

pub fn status(ui: &slint::Weak<HistoryWindow>, message: impl Into<String>) {
    let message = message.into();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        STATUS_RESET.with(|timer| timer.stop());
        let temporary = message == "Preferences saved";
        ui.set_status(message.into());
        if temporary {
            let weak = ui.as_weak();
            STATUS_RESET.with(|timer| {
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_secs(2),
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            // A newer capture or paste message must not be overwritten.
                            if ui.get_status() == "Preferences saved" {
                                ui.set_status("Ready".into());
                            }
                        }
                    },
                )
            });
        }
    });
}

fn main() {
    velopack::VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .on_before_uninstall_fast_callback(|_| {
            let _ = settings::startup(false);
        })
        .run();
    if let Err(error) = run() {
        native::fatal(&format!("SnipTory could not start.\n\n{error:#}"));
    }
}

fn run() -> Result<()> {
    native::wait_for_restart()?;
    let Some(_instance) = native::single_instance()? else {
        return Ok(());
    };
    let ui = HistoryWindow::new()?;
    ui.set_release_version(updates::RELEASE.trim().into());
    let root = settings::root()?;
    let preferences = std::sync::Arc::new(std::sync::Mutex::new(settings::load(&root)));
    apply_preferences(&ui, &preferences.lock().unwrap());
    if preferences.lock().unwrap().launch_at_sign_in {
        settings::startup(true)?;
    }
    let weak = ui.as_weak();
    let (store_tx, store_rx) = mpsc::channel();
    let recording_bar = RecordingBar::new()?;
    let bridge = native::start(
        weak.clone(),
        recording_bar.as_weak(),
        store_tx.clone(),
        preferences.clone(),
    )?;
    let b = bridge.clone();
    recording_bar.on_stop(move || b.send(native::Command::StopRecording));
    let b = bridge.clone();
    recording_bar.on_cancel(move || b.send(native::Command::CancelRecording));
    let b = bridge.clone();
    recording_bar.window().on_close_requested(move || {
        b.send(native::Command::CancelRecording);
        slint::CloseRequestResponse::KeepWindowShown
    });
    let entries = Rc::new(RefCell::new(Vec::<history::Entry>::new()));
    let entries_for_worker = entries.clone();
    let storage_root = root.clone();
    let storage_preferences = preferences.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker_ui = weak.clone();
    let worker_bridge = bridge.clone();
    let worker_store = store_tx.clone();
    // UI-only state is carried through a Send wrapper supported by Slint Weak;
    // the actual gallery data is updated in the UI event loop below.
    let (gallery_tx, gallery_rx) =
        mpsc::channel::<(Vec<history::Entry>, Vec<(Vec<u8>, u32, u32)>, bool)>();
    std::thread::Builder::new()
        .name("sniptory-storage".into())
        .spawn(move || {
            let history = match history::History::open(root.clone()) {
                Ok(history) => {
                    let _ = ready_tx.send(Ok(()));
                    history
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                    return;
                }
            };
            while let Ok(command) = store_rx.recv() {
                let result: Result<()> = (|| {
                    match command {
                        StoreCommand::Save(pixels) => {
                            history.save(&pixels)?;
                            status(&worker_ui, "Saved locally. Ready when you are.");
                            let _ = worker_store.send(StoreCommand::Load(60));
                        }
                        StoreCommand::SaveGif(recording) => {
                            history.save_gif(&recording)?;
                            worker_bridge.send(native::Command::CopyGif(recording.path, false));
                            status(&worker_ui, "GIF saved to History");
                            let _ = worker_store.send(StoreCommand::Load(60));
                        }
                        StoreCommand::Delete(id) => {
                            let result = history.delete(&id);
                            let _ = worker_store.send(StoreCommand::Load(60));
                            let _ =
                                worker_ui.upgrade_in_event_loop(|ui| ui.set_history_busy(false));
                            result?;
                            status(&worker_ui, "Capture deleted");
                        }
                        StoreCommand::ClearHistory => {
                            let result = history.clear();
                            let _ = worker_store.send(StoreCommand::Load(60));
                            let _ =
                                worker_ui.upgrade_in_event_loop(|ui| ui.set_history_busy(false));
                            let count = result?;
                            status(
                                &worker_ui,
                                format!("History cleared · {count} capture(s) deleted"),
                            );
                        }
                        StoreCommand::Load(limit) => {
                            let mut loaded = Vec::new();
                            let mut previews = Vec::new();
                            let mut skipped = 0;
                            let all = history.list(limit + 1)?;
                            let has_more = all.len() > limit;
                            for entry in all.into_iter().take(limit) {
                                if !entry.path.is_file() {
                                    skipped += 1;
                                    continue;
                                }
                                match history.preview(&entry) {
                                    Ok(image) => {
                                        let (w, h) = image.dimensions();
                                        previews.push((image.into_raw(), w, h));
                                        loaded.push(entry);
                                    }
                                    Err(_) => {
                                        skipped += 1;
                                    }
                                }
                            }
                            gallery_tx.send((loaded, previews, has_more))?;
                            let _ =
                                worker_ui.upgrade_in_event_loop(|ui| ui.invoke_refresh_gallery());
                            if skipped > 0 {
                                status(
                                    &worker_ui,
                                    format!("{skipped} unavailable capture(s) skipped."),
                                );
                            }
                        }
                        StoreCommand::Paste(path) => {
                            if path
                                .extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
                            {
                                worker_bridge.send(native::Command::CopyGif(path, true));
                            } else {
                                let pixels = image::open(path)?.to_rgba8();
                                worker_bridge.send(native::Command::CopyPaste(pixels));
                            }
                        }
                        StoreCommand::Exit => {
                            worker_bridge.send(native::Command::Quit);
                            let _ = slint::quit_event_loop();
                        }
                        StoreCommand::ApplyUpdate(manager, asset) => {
                            // This command follows all prior saves in the same
                            // queue. Each PNG and database transaction is durable
                            // before the updater is allowed to replace binaries.
                            match manager.wait_exit_then_apply_updates(
                                &asset,
                                true,
                                true,
                                Vec::<String>::new(),
                            ) {
                                Ok(()) => {
                                    worker_bridge.send(native::Command::Quit);
                                    let _ = slint::quit_event_loop();
                                }
                                Err(error) => {
                                    worker_bridge.send(native::Command::UpdateFailed);
                                    let message = format!("Couldn't apply update: {error}");
                                    let _ = worker_ui.upgrade_in_event_loop(move |ui| {
                                        ui.set_update_busy(false);
                                        ui.set_update_label("Retry update".into());
                                        ui.set_update_detail(message.into());
                                    });
                                }
                            }
                        }
                        StoreCommand::Settings(new_settings) => {
                            let old = settings::load(&root);
                            let saved = settings::startup(new_settings.launch_at_sign_in)
                                .and_then(|_| settings::save(&root, &new_settings));
                            if let Err(error) = saved {
                                let _ = settings::startup(old.launch_at_sign_in);
                                *storage_preferences.lock().unwrap() = old.clone();
                                let _ = worker_ui
                                    .upgrade_in_event_loop(move |ui| apply_preferences(&ui, &old));
                                return Err(error);
                            }
                            status(&worker_ui, "Preferences saved");
                        }
                        StoreCommand::Restart => {
                            match std::process::Command::new(std::env::current_exe()?)
                                .args(["--restart-from", &std::process::id().to_string()])
                                .spawn()
                            {
                                Ok(_) => {
                                    worker_bridge.send(native::Command::Quit);
                                    let _ = slint::quit_event_loop();
                                }
                                Err(error) => {
                                    worker_bridge.send(native::Command::UpdateFailed);
                                    return Err(error.into());
                                }
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    status(&worker_ui, format!("{error:#}"));
                    worker_bridge.notify(&format!("{error:#}"));
                }
            }
        })?;
    ready_rx.recv()?.map_err(anyhow::Error::msg)?;
    let previewer = preview::start(ui.as_weak())?;
    let hover_entries = entries.clone();
    let poster = Rc::new(RefCell::new(None::<(usize, String, slint::Image)>));
    let w = ui.as_weak();
    ui.on_preview(move |index| {
        let Some(ui) = w.upgrade() else {
            return;
        };
        ui.set_hovered(index);
        let model = ui.get_captures();
        let previous = poster.borrow_mut().take();
        if let Some((old_index, id, image)) = previous {
            if let Some(mut row) = model.row_data(old_index) {
                if row.id.as_str() == id {
                    row.preview = image;
                    model.set_row_data(old_index, row);
                }
            }
        }
        if index >= 0 {
            if let Some(entry) = hover_entries.borrow().get(index as usize) {
                if entry.duration_ms.is_some() {
                    if let Some(row) = model.row_data(index as usize) {
                        *poster.borrow_mut() =
                            Some((index as usize, entry.id.clone(), row.preview));
                        let _ = previewer.send(preview::Request::Play {
                            index,
                            id: entry.id.clone(),
                            path: entry.path.clone(),
                        });
                        return;
                    }
                }
            }
        }
        let _ = previewer.send(preview::Request::Stop);
    });
    let tx = store_tx.clone();
    ui.on_delete_entry(move |id| {
        let _ = tx.send(StoreCommand::Delete(id.to_string()));
    });
    let tx = store_tx.clone();
    ui.on_clear_history(move || {
        let _ = tx.send(StoreCommand::ClearHistory);
    });
    let updater = updates::start(ui.as_weak(), bridge.clone(), preferences.clone())?;
    let check_sender = updater.clone();
    let w = ui.as_weak();
    ui.on_check_updates(move || {
        if w.upgrade().is_some_and(|ui| !ui.get_update_busy()) {
            let _ = check_sender.try_send(true);
        }
    });
    let w = ui.as_weak();
    ui.on_update_action(move || {
        if w.upgrade().is_some_and(|ui| !ui.get_update_busy()) {
            let _ = updater.try_send(false);
        }
    });
    let tx = store_tx.clone();
    ui.on_setting_changed(move |key, value| {
        let mut settings = preferences.lock().unwrap();
        match key.as_str() {
            "startup" => settings.launch_at_sign_in = value,
            "updates" => settings.automatic_updates = value,
            "paste" => settings.paste_on_select = value,
            _ => return,
        };
        let _ = tx.send(StoreCommand::Settings(settings.clone()));
    });
    ui.on_open_folder(move || {
        native::open_folder(&storage_root.join("captures"));
    });
    let b = bridge.clone();
    ui.on_restart_app(move || {
        b.send(native::Command::Restart);
    });
    let menu = TrayMenu::new()?;
    menu.set_release_version(updates::RELEASE.trim().into());
    let menu_weak = menu.as_weak();
    ui.on_tray_menu(move || {
        if let Some(menu) = menu_weak.upgrade() {
            native::show_tray_menu(&menu);
        }
    });
    let menu_weak = menu.as_weak();
    menu.on_dismiss(move || {
        if let Some(menu) = menu_weak.upgrade() {
            let _ = menu.hide();
        }
    });
    let b = bridge.clone();
    let menu_weak = menu.as_weak();
    menu.on_action(move |action| {
        if let Some(menu) = menu_weak.upgrade() {
            let _ = menu.hide();
        }
        match action.as_str() {
            "history" => b.send(native::Command::History),
            "capture" => b.send(native::Command::Capture),
            "full-capture" => b.send(native::Command::FullCapture),
            "gif" => b.send(native::Command::RecordGif),
            "options" => b.send(native::Command::Options),
            "updates" => b.send(native::Command::Updates),
            "exit" => {
                b.send(native::Command::Exit);
            }
            _ => {}
        }
    });
    ui.on_refresh_gallery(move || {
        if let Ok((loaded, previews, has_more)) = gallery_rx.try_recv() {
            let model = VecModel::default();
            for (entry, (bytes, w, h)) in loaded.iter().zip(previews) {
                let pixels = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bytes, w, h);
                let date = chrono::DateTime::from_timestamp(entry.timestamp, 0)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%b %-d · %-I:%M %p")
                            .to_string()
                    })
                    .unwrap_or_else(|| "Capture".into());
                model.push(CaptureItem {
                    id: entry.id.clone().into(),
                    preview: slint::Image::from_rgba8(pixels),
                    label: date.into(),
                    dimensions: match entry.duration_ms {
                        Some(duration) => format!(
                            "{} × {} · {:.1}s",
                            entry.width,
                            entry.height,
                            duration as f64 / 1000.
                        ),
                        None => format!("{} × {} · PNG", entry.width, entry.height),
                    }
                    .into(),
                    animated: entry.duration_ms.is_some(),
                });
            }
            *entries_for_worker.borrow_mut() = loaded;
            if let Some(ui) = weak.upgrade() {
                ui.invoke_preview(-1);
                ui.set_captures(ModelRc::new(model));
                ui.set_selected(-1);
                ui.set_has_more(has_more);
            }
        }
    });
    let b = bridge.clone();
    ui.on_capture(move || {
        b.send(native::Command::Capture);
    });
    let w = ui.as_weak();
    ui.on_dismiss(move || {
        if let Some(ui) = w.upgrade() {
            ui.invoke_preview(-1);
            let _ = ui.hide();
        }
    });
    let w = ui.as_weak();
    let tx = store_tx.clone();
    ui.on_paste(move |index| {
        if let Some(entry) = entries.borrow().get(index as usize) {
            if let Some(ui) = w.upgrade() {
                ui.set_selected(index);
                ui.set_status("Preparing image…".into());
            }
            let _ = tx.send(StoreCommand::Paste(entry.path.clone()));
        }
    });
    let page_limit = Rc::new(RefCell::new(60usize));
    let tx = store_tx.clone();
    ui.on_more(move || {
        let mut limit = page_limit.borrow_mut();
        *limit += 60;
        let _ = tx.send(StoreCommand::Load(*limit));
    });
    let w = ui.as_weak();
    ui.on_navigate(move |direction| {
        if let Some(ui) = w.upgrade() {
            let count = ui.get_captures().row_count() as i32;
            if count == 0 {
                return;
            }
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
    });
    let w = ui.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(ui) = w.upgrade() {
            ui.invoke_preview(-1);
            let _ = ui.hide();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    store_tx.send(StoreCommand::Load(60))?;
    if !std::env::args().any(|arg| arg == "--background") {
        bridge.send(native::Command::History);
    }
    slint::run_event_loop_until_quit()?;
    bridge.send(native::Command::Quit);
    Ok(())
}

fn apply_preferences(ui: &HistoryWindow, settings: &settings::Settings) {
    ui.set_launch_at_sign_in(settings.launch_at_sign_in);
    ui.set_automatic_updates(settings.automatic_updates);
    ui.set_paste_on_select(settings.paste_on_select);
}

pub fn hwnd(ui: &HistoryWindow) -> Option<isize> {
    match ui.window().window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}
