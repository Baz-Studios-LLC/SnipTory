#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
include!("windows_app.rs");
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(target_os = "macos")]
use sniptory::{settings, thumbnail};
#[cfg(target_os = "macos")]
mod preview;
#[cfg(target_os = "macos")]
mod updates;
#[cfg(target_os = "macos")]
slint::include_modules!();
#[cfg(target_os = "macos")]
fn main() {
    velopack::VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .run();
    if let Err(error) = macos::run() {
        macos::fatal(&format!("SnipTory could not start.\n\n{error:#}"));
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!(
        "The Linux desktop frontend is still in development. Use cargo test --lib for the shared core."
    );
    std::process::exit(1);
}
