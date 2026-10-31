use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[cfg(windows)]
use windows::{Win32::System::Registry::*, core::*};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub launch_at_sign_in: bool,
    pub automatic_updates: bool,
    pub paste_on_select: bool,
    pub shortcuts: crate::hotkeys::Shortcuts,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            launch_at_sign_in: false,
            automatic_updates: true,
            paste_on_select: true,
            shortcuts: Default::default(),
        }
    }
}
pub type Shared = Arc<Mutex<Settings>>;
pub fn root() -> Result<PathBuf> {
    crate::paths::data_root()
}
pub fn load(root: &Path) -> Settings {
    let mut settings: Settings = fs::read(root.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    settings.shortcuts = settings.shortcuts.normalized().unwrap_or_default();
    settings
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
#[cfg(windows)]
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_preferences_keep_defaults_and_custom_shortcuts_survive_reload() -> Result<()> {
        let old: Settings =
            serde_json::from_str(r#"{"automatic_updates":false,"paste_on_select":false}"#)?;
        assert_eq!(old.shortcuts, crate::hotkeys::Shortcuts::default());
        assert!(!old.automatic_updates);
        let root = std::env::temp_dir().join(format!(
            "sniptory-shortcuts-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let mut custom = old;
        custom.shortcuts.screenshot = "Ctrl+Shift+F12".into();
        custom.shortcuts.history = "Ctrl+Alt+H".into();
        save(&root, &custom)?;
        let restored = load(&root);
        assert_eq!(restored.shortcuts, custom.shortcuts);
        assert!(!restored.paste_on_select);
        assert!(!restored.automatic_updates);
        custom.shortcuts.history = custom.shortcuts.screenshot.clone();
        save(&root, &custom)?;
        assert_eq!(load(&root).shortcuts, crate::hotkeys::Shortcuts::default());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
