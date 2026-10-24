//! Where the server and the client keep their files: the platform's standard
//! directories, each under the program's own name.
//!
//! | | Linux, Android | macOS | Windows |
//! |---|---|---|---|
//! | config | `$XDG_CONFIG_HOME`, `~/.config` | `~/Library/Application Support` | `%APPDATA%` |
//! | data | `$XDG_DATA_HOME`, `~/.local/share` | `~/Library/Application Support` | `%APPDATA%` |
//! | cache | `$XDG_CACHE_HOME`, `~/.cache` | `~/Library/Caches` | `%LOCALAPPDATA%` |
//!
//! A set, non-empty XDG variable wins on every platform: that is how tests
//! and demo runs keep away from a person's real files, and how
//! `android_main` points the client at the application's private directory.

use std::path::PathBuf;

/// `<config>/<name>`: settings a person may edit or carry to another machine.
pub fn config_dir(name: &str) -> PathBuf {
    resolve("XDG_CONFIG_HOME", dirs::config_dir(), ".config").join(name)
}

/// `<data>/<name>`: what the program keeps for itself.
pub fn data_dir(name: &str) -> PathBuf {
    resolve("XDG_DATA_HOME", dirs::data_dir(), ".local/share").join(name)
}

/// `<cache>/<name>`: what may be thrown away.
pub fn cache_dir(name: &str) -> PathBuf {
    resolve("XDG_CACHE_HOME", dirs::cache_dir(), ".cache").join(name)
}

/// The home directory, `/` where there is none.
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// `~` and `~/…` under the home directory; anything else as it is.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if path == "~" => home_dir(),
        None => PathBuf::from(path),
    }
}

fn resolve(variable: &str, standard: Option<PathBuf>, fallback: &str) -> PathBuf {
    resolve_from(std::env::var_os(variable).map(PathBuf::from), standard, dirs::home_dir(), fallback)
}

/// The rule apart from the environment, so it can be tested.
fn resolve_from(variable: Option<PathBuf>, standard: Option<PathBuf>, home: Option<PathBuf>, fallback: &str) -> PathBuf {
    match variable {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => standard.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from("/")).join(fallback)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_wins_then_the_platform_then_home() {
        let home = Some(PathBuf::from("/home/someone"));
        let standard = Some(PathBuf::from("/standard"));
        assert_eq!(resolve_from(Some("/elsewhere".into()), standard.clone(), home.clone(), ".config"), PathBuf::from("/elsewhere"));
        // Unset and empty mean the same thing, as the XDG spec says.
        assert_eq!(resolve_from(Some(PathBuf::new()), standard.clone(), home.clone(), ".config"), PathBuf::from("/standard"));
        assert_eq!(resolve_from(None, None, home, ".local/share"), PathBuf::from("/home/someone/.local/share"));
    }

    #[test]
    fn a_tilde_is_the_home_directory() {
        assert_eq!(expand_home("~/x"), home_dir().join("x"));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
    }
}
