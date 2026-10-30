use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use windows::{Win32::System::Registry::*, core::*};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub launch_at_sign_in: bool,
    pub automatic_updates: bool,
    pub paste_on_select: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            launch_at_sign_in: false,
            automatic_updates: true,
            paste_on_select: true,
        }
    }
}
pub type Shared = Arc<Mutex<Settings>>;
pub fn root() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("Sniptory"),
    )
}
pub fn load(root: &Path) -> Settings {
    fs::read(root.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
pub fn save(root: &Path, settings: &Settings) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = root.join("settings.json");
    let temp = root.join("settings.json.tmp");
    let mut file = fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(settings)?)?;
    file.sync_all()?;
    drop(file);
    // All writes are serialized by the storage worker. Windows rename replaces
    // the old settings file atomically on the same volume.
    fs::rename(temp, path)?;
    Ok(())
}
pub fn startup(enabled: bool) -> Result<()> {
    // SAFETY: only this app's per-user Run value is changed. Windows owns the
    // opened key; it is closed on both success and failure.
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = if enabled {
            let command = format!("\"{}\" --background", std::env::current_exe()?.display());
            let bytes = command
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            RegSetValueExW(key, w!("SnipTory"), Some(0), REG_SZ, Some(&bytes)).ok()
        } else {
            let status = RegDeleteValueW(key, w!("SnipTory"));
            if status == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                status.ok()
            }
        };
        let _ = RegCloseKey(key);
        result?;
    }
    Ok(())
}
