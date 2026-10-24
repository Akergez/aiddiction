//! Where this installation keeps its files: the platform's standard
//! directories (`aiddiction-paths`), under [`crate::APP_ID`]. On a phone
//! `android_main` points the XDG variables at the application's private
//! directory first.
//!
//! The directory name must never change, or an upgrade loses the settings.

use std::path::PathBuf;

/// `settings.json`: the servers, which one is in use, and the appearance.
pub fn settings_file() -> PathBuf {
    aiddiction_paths::config_dir(crate::APP_ID).join("settings.json")
}

/// What this installation keeps for itself: the demo server's data and the
/// installed themes.
pub fn data_dir() -> PathBuf {
    aiddiction_paths::data_dir(crate::APP_ID)
}

/// The themes installed from Zed's registry, one directory to an extension.
pub fn themes_dir() -> PathBuf {
    data_dir().join("themes")
}
