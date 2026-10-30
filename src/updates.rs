//! Installer-managed updates. Networking is confined to the public release feed;
//! this module never reads captures, clipboard contents, or the history database.
use crate::{HistoryWindow, native};
use std::{sync::mpsc, time::Duration};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackAsset, sources::HttpSource};

pub const RELEASE: &str = concat!("v", include_str!("../VERSION"));
const FEED: &str = "https://github.com/Baz-Studios-LLC/SnipTory/releases/latest/download";

enum Stage {
    Check,
    Download(Box<UpdateInfo>),
    Restart(VelopackAsset),
}

pub fn start(
    ui: slint::Weak<HistoryWindow>,
    bridge: native::Bridge,
    preferences: crate::settings::Shared,
) -> std::io::Result<mpsc::SyncSender<bool>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("sniptory-updates".into())
        .spawn(move || {
            let source = HttpSource::new_with_options(
                FEED,
                velopack::HttpOptions {
                    TimeoutMilliseconds: 120_000,
                    ..Default::default()
                },
            );
            let manager = UpdateManager::new(source, None, None);
            let mut stage = Stage::Check;
            let mut interval = Duration::from_secs(15);
            loop {
                let (manual, force_check) = match rx.recv_timeout(interval) {
                    Ok(force) => (true, force),
                    Err(mpsc::RecvTimeoutError::Timeout) => (false, false),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                interval = Duration::from_secs(6 * 60 * 60);
                if !manual && !preferences.lock().unwrap().automatic_updates {
                    continue;
                }
                if force_check {
                    stage = Stage::Check;
                }
                let manager = match &manager {
                    Ok(manager) => manager,
                    Err(_) => {
                        if manual {
                            view(
                                &ui,
                                "Check for updates",
                                "Install SnipTory to enable in-app updates.",
                                false,
                            );
                        }
                        continue;
                    }
                };
                // Automatic checks never download or restart without the user's action.
                if !manual && !matches!(stage, Stage::Check) {
                    continue;
                }
                match &stage {
                    Stage::Check => {
                        view(&ui, "Checking…", "Checking for updates…", true);
                        match manager.check_for_updates() {
                            Ok(UpdateCheck::UpdateAvailable(info)) => {
                                let version = release_label(&info.TargetFullRelease.Version);
                                view(
                                    &ui,
                                    &format!("Download {version}"),
                                    &format!("{version} is available"),
                                    false,
                                );
                                stage = Stage::Download(info);
                            }
                            Ok(_) => view(&ui, "Check for updates", "You're up to date", false),
                            Err(_) => view(
                                &ui,
                                "Retry update check",
                                "Couldn't check for updates. Try again later.",
                                false,
                            ),
                        }
                    }
                    Stage::Download(info) => {
                        view(&ui, "Downloading…", "Downloading update…", true);
                        let (progress_tx, progress_rx) = mpsc::channel();
                        let progress_ui = ui.clone();
                        let progress = std::thread::spawn(move || {
                            for percent in progress_rx {
                                view(
                                    &progress_ui,
                                    &format!("Downloading {percent}%"),
                                    "Downloading update…",
                                    true,
                                );
                            }
                        });
                        let result = manager.download_updates(info, Some(progress_tx));
                        let _ = progress.join();
                        match result {
                            Ok(()) => {
                                let asset = info.TargetFullRelease.clone();
                                view(
                                    &ui,
                                    "Restart & update",
                                    "Update ready. Restart when you're ready.",
                                    false,
                                );
                                stage = Stage::Restart(asset);
                            }
                            Err(_) => {
                                // The latest release may have changed since the
                                // offer was shown. Refresh the feed on retry.
                                stage = Stage::Check;
                                view(
                                    &ui,
                                    "Retry update check",
                                    "Download failed. Check the latest release again.",
                                    false,
                                );
                            }
                        }
                    }
                    Stage::Restart(asset) => {
                        view(&ui, "Restarting…", "Finishing saves before updating…", true);
                        bridge.send(native::Command::ApplyUpdate(manager.clone(), asset.clone()));
                    }
                }
            }
        })?;
    Ok(tx)
}

fn view(ui: &slint::Weak<HistoryWindow>, label: &str, detail: &str, busy: bool) {
    let (label, detail) = (label.to_owned(), detail.to_owned());
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_update_label(label.into());
        ui.set_update_detail(detail.into());
        ui.set_update_busy(busy);
    });
}

pub fn release_label(version: &str) -> String {
    // Package tools require semantic versions internally. The public version
    // sequence is always an integer with a minimum of three digits.
    version
        .rsplit('.')
        .next()
        .and_then(|n| n.parse::<u32>().ok())
        .map(|n| format!("v{n:03}"))
        .unwrap_or_else(|| "new release".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_names_never_contain_decimals() {
        assert_eq!(super::release_label("0.0.1"), "v001");
        assert_eq!(super::release_label("0.0.42"), "v042");
        assert_eq!(super::release_label("0.0.1000"), "v1000");
    }
}
