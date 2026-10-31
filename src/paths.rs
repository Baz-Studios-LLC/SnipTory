//! Persistent application data, separate from installed binaries on every OS.
use anyhow::{Context, Result};
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_root() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return Ok(PathBuf::from(
            std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?,
        )
        .join("Sniptory"));
    }
    #[cfg(target_os = "macos")]
    {
        return macos_root(std::env::var_os("HOME"));
    }
    #[cfg(target_os = "linux")]
    {
        return linux_root(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("This operating system is not supported");
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_root(home: Option<OsString>) -> Result<PathBuf> {
    Ok(PathBuf::from(home.context("HOME is unavailable")?)
        .join("Library/Application Support/SnipTory"))
}

#[cfg(any(target_os = "linux", test))]
fn linux_root(xdg: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(path) = xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Ok(path.join("sniptory"));
    }
    Ok(PathBuf::from(home.context("HOME is unavailable")?).join(".local/share/sniptory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_data_uses_application_support() {
        let home = PathBuf::from("home").join("Brett");
        assert_eq!(
            macos_root(Some(home.clone().into_os_string())).unwrap(),
            home.join("Library/Application Support/SnipTory")
        );
        assert!(macos_root(None).is_err());
    }
    #[test]
    fn linux_data_honors_absolute_xdg_and_ignores_empty_or_relative_paths() {
        let absolute = std::env::temp_dir().join("sniptory-path-test");
        assert_eq!(
            linux_root(Some(absolute.clone().into_os_string()), None).unwrap(),
            absolute.join("sniptory")
        );
        let home = PathBuf::from("home").join("Brett");
        for xdg in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            assert_eq!(
                linux_root(xdg, Some(home.clone().into_os_string())).unwrap(),
                home.join(".local/share/sniptory")
            );
        }
        assert!(linux_root(None, None).is_err());
    }
}
