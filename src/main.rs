#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
include!("windows_app.rs");
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sniptory::{settings, thumbnail};
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod preview;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod updates;
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "linux")]
fn main() {
    velopack::VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .run();
    if let Err(error) = linux::run() {
        eprintln!("SnipTory could not start: {error:#}");
        std::process::exit(1);
    }
}
