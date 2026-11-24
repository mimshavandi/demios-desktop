//! Per-OS app-data directory (ADR-0007). No extra dependency: the three conventions are
//! small enough to spell out.
//!
//! | OS | Directory |
//! |---|---|
//! | macOS | `~/Library/Application Support/<app_id>` |
//! | Windows | `%LOCALAPPDATA%\<app_id>` (not roaming; matches Tauri `app_local_data_dir`) |
//! | Linux / other Unix | `$XDG_DATA_HOME/<app_id>` or `~/.local/share/<app_id>` |

use std::path::PathBuf;

/// Reverse-DNS id the desktop app uses for its data directory and bundle.
pub const APP_ID: &str = dn_core::APP_ID;

/// File name of the main database inside the app-data directory.
pub const DB_FILE: &str = "demios.sqlite3";

/// The app-data directory for `app_id`, or `None` if the OS gives no home directory.
pub fn app_data_dir(app_id: &str) -> Option<PathBuf> {
    base_data_dir().map(|d| d.join(app_id))
}

/// `<app-data>/demios.sqlite3` for [`APP_ID`].
pub fn default_db_path() -> Option<PathBuf> {
    app_data_dir(APP_ID).map(|d| d.join(DB_FILE))
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(target_os = "macos")]
fn base_data_dir() -> Option<PathBuf> {
    env_dir("HOME").map(|h| h.join("Library").join("Application Support"))
}

#[cfg(windows)]
fn base_data_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn base_data_dir() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local").join("share")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_ends_with_app_id() {
        let dir = app_data_dir("com.example.test").expect("home dir in test env");
        assert!(dir.is_absolute());
        assert!(dir.ends_with("com.example.test"));
        assert_eq!(
            default_db_path().unwrap().file_name().unwrap(),
            std::ffi::OsStr::new(DB_FILE)
        );
    }
}
