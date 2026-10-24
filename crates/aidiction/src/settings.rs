//! What this installation remembers: the servers it knows and which one is in
//! use. A GPUI global over `settings.json`, written back on every change.
//!
//! It lives in the platform's standard config directory (`paths`). The file
//! is private to the user (mode 0600) because it holds the servers'
//! tokens; a desktop keyring is the next step (docs/PLAN.md, 6.7).

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsData {
    pub servers: Vec<ServerEntry>,
    pub active: Option<String>,
    pub appearance: Appearance,
}

/// How the window looks ([`crate::appearance`]). Unknown values mean the
/// default, so a file from a later version never stops this one drawing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// `light` or `dark`; anything else follows the system.
    pub mode: Option<String>,
    /// `#rrggbb` the scheme is made from; none follows the system's colour.
    pub color: Option<String>,
    /// The system's colour as last seen, so a launch starts in it.
    pub system_color: Option<String>,
    /// The installed Zed theme worn in light and in dark; none is the scheme.
    pub light_theme: Option<String>,
    pub dark_theme: Option<String>,
}

impl SettingsData {
    pub fn active_server(&self) -> Option<&ServerEntry> {
        let id = self.active.as_deref()?;
        self.servers.iter().find(|server| server.id == id)
    }
}

pub struct Settings(SettingsData);

impl Global for Settings {}

impl Settings {
    pub fn install(cx: &mut App) {
        let path = crate::paths::settings_file();
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!("{} is unreadable, starting afresh: {error}", path.display());
                SettingsData::default()
            }),
            Err(_) => SettingsData::default(),
        };
        cx.set_global(Settings(data));
    }

    pub fn get(cx: &App) -> &SettingsData {
        &cx.global::<Settings>().0
    }

    pub fn update(cx: &mut App, f: impl FnOnce(&mut SettingsData)) {
        f(&mut cx.global_mut::<Settings>().0);
        if let Err(error) = save(&cx.global::<Settings>().0) {
            tracing::error!("could not save the settings: {error}");
        }
    }
}

fn save(data: &SettingsData) -> std::io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = crate::paths::settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(data).expect("settings serialize"))?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}
