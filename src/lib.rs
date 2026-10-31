//! Platform-independent SnipTory data and preferences.
//!
//! Native capture, clipboard, windows, and application updates belong to each
//! desktop frontend. This crate can be tested without a desktop session.
pub mod history;
pub mod hotkeys;
pub mod media;
pub mod paths;
pub mod settings;
pub mod thumbnail;
