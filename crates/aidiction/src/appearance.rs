//! Light or dark, in which colours: the engine money-manager-ng and
//! matterfast use, `gpui-adaptive-colors` and `gpui-zed-themes`. This module
//! only remembers the choices in the settings and hands them to it.
//!
//! - The colours are a Material You scheme made from the system's colour or
//!   one picked here (`set_source`).
//! - Instead of the scheme a theme for the Zed editor can be worn, one for
//!   light and one for dark (`wear` / `wear_scheme`), installed from Zed's
//!   registry with the library's `Browser` into [`crate::paths::themes_dir`].
//! - Light or dark is the toolkit's own `Theme::change`, following the
//!   system unless one is chosen.

use std::rc::Rc;

use gpui_adaptive_colors::{AdaptiveColors, Color, Options, Seed, Source};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Global, SharedString, Window};
use gpui_zed_themes::{Registry, Store};

use crate::settings::Settings;

/// The colours offered beside the system's.
pub const SEEDS: [(&str, &str); 6] =
    [("#0b57d0", "Blue"), ("#00838f", "Teal"), ("#2fb380", "Green"), ("#6750a4", "Violet"), ("#e91e63", "Pink"), ("#c88800", "Amber")];

/// What the scheme is called in a list of themes.
pub const SCHEME: &str = "Color scheme";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    System,
    Light,
    Dark,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::System, Mode::Light, Mode::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Mode::System => "System",
            Mode::Light => "Light",
            Mode::Dark => "Dark",
        }
    }
}

/// The installed Zed themes, read at startup and after an install.
struct Installed(Vec<Rc<ThemeConfig>>);

impl Global for Installed {}

pub fn store() -> Store {
    Store::new(crate::paths::themes_dir())
}

/// Zed's registry, or `AIDICTION_THEMES_API` in its place for tests.
pub fn registry() -> Registry {
    let registry = Registry::new(concat!("aidiction/", env!("CARGO_PKG_VERSION")));
    match std::env::var("AIDICTION_THEMES_API") {
        Ok(api) if !api.is_empty() => registry.at(api),
        _ => registry,
    }
}

/// Starts the engine with the stored choices. Once, before the first window.
pub fn init(cx: &mut App) {
    let appearance = Settings::get(cx).appearance.clone();
    let remembered = appearance.system_color.as_deref().and_then(|seed| seed.parse::<Seed>().ok());
    gpui_adaptive_colors::init(Options::new(Color::from_u32(0x0b57d0)).source(source(cx)).remembered(remembered), cx);
    // The system's colour arrives after the first frame; kept, it is what the
    // next launch starts with.
    cx.observe_global::<AdaptiveColors>(|cx| {
        let said = AdaptiveColors::global(cx).system_seed().map(|seed| seed.to_string());
        if Settings::get(cx).appearance.system_color != said {
            Settings::update(cx, |settings| settings.appearance.system_color = said);
        }
    })
    .detach();
    reload_themes(cx);
}

/// `AIDICTION_COLOR=#rrggbb` pins the colour, so a screenshot does not
/// depend on the desktop's.
fn source(cx: &App) -> Source {
    std::env::var("AIDICTION_COLOR")
        .ok()
        .or_else(|| Settings::get(cx).appearance.color.clone())
        .and_then(|stored| Color::parse_hex(&stored))
        .map_or(Source::System, Source::from)
}

/// The colour chosen, or `None` for the system's.
pub fn color(cx: &App) -> Option<Color> {
    Settings::get(cx).appearance.color.as_deref().and_then(Color::parse_hex)
}

pub fn set_color(color: Option<Color>, cx: &mut App) {
    Settings::update(cx, |settings| settings.appearance.color = color.map(|color| color.to_hex()));
    gpui_adaptive_colors::set_source(source(cx), cx);
}

pub fn mode(cx: &App) -> Mode {
    match Settings::get(cx).appearance.mode.as_deref() {
        Some("light") => Mode::Light,
        Some("dark") => Mode::Dark,
        _ => Mode::System,
    }
}

pub fn set_mode(mode: Mode, window: &mut Window, cx: &mut App) {
    let stored = match mode {
        Mode::System => None,
        Mode::Light => Some("light".to_owned()),
        Mode::Dark => Some("dark".to_owned()),
    };
    Settings::update(cx, |settings| settings.appearance.mode = stored);
    apply_mode(Some(window), cx);
}

/// Light or dark as chosen, or as the system is. Again whenever the window's
/// appearance changes.
pub fn apply_mode(window: Option<&mut Window>, cx: &mut App) {
    let mode = match mode(cx) {
        Mode::Light => ThemeMode::Light,
        Mode::Dark => ThemeMode::Dark,
        #[cfg(target_os = "android")]
        Mode::System => {
            let _ = &window;
            if gpui_mobile::android::jni::query_night_mode_via_jni() { ThemeMode::Dark } else { ThemeMode::Light }
        }
        #[cfg(not(target_os = "android"))]
        Mode::System => window.map(|window| window.appearance()).unwrap_or_else(|| cx.window_appearance()).into(),
    };
    if Theme::global(cx).mode != mode {
        Theme::change(mode, None, cx);
    }
}

/// Reads the installed themes again and wears what the settings name.
pub fn reload_themes(cx: &mut App) {
    let themes = store().themes().into_iter().map(Rc::new).collect();
    cx.set_global(Installed(themes));
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        match chosen(mode, cx) {
            Some(theme) => gpui_adaptive_colors::wear(theme, cx),
            None => gpui_adaptive_colors::wear_scheme(mode, cx),
        }
    }
}

fn chosen(mode: ThemeMode, cx: &App) -> Option<Rc<ThemeConfig>> {
    let appearance = &Settings::get(cx).appearance;
    let wanted = if mode.is_dark() { appearance.dark_theme.as_deref() } else { appearance.light_theme.as_deref() }?;
    cx.try_global::<Installed>()?.0.iter().find(|theme| theme.mode == mode && theme.name.as_ref() == wanted).cloned()
}

/// The scheme first, then the installed themes of that mode by name.
pub fn theme_names(mode: ThemeMode, cx: &App) -> Vec<SharedString> {
    let mut names: Vec<SharedString> = cx
        .try_global::<Installed>()
        .map(|installed| installed.0.iter().filter(|theme| theme.mode == mode).map(|theme| theme.name.clone()).collect())
        .unwrap_or_default();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names.insert(0, SCHEME.into());
    names
}

/// What is worn in `mode`, as [`theme_names`] lists it.
pub fn worn(mode: ThemeMode, cx: &App) -> SharedString {
    chosen(mode, cx).map_or(SCHEME.into(), |theme| theme.name.clone())
}

pub fn choose_theme(mode: ThemeMode, name: &str, cx: &mut App) {
    let name = (name != SCHEME).then(|| name.to_owned());
    Settings::update(cx, |settings| {
        if mode.is_dark() {
            settings.appearance.dark_theme = name;
        } else {
            settings.appearance.light_theme = name;
        }
    });
    reload_themes(cx);
}
