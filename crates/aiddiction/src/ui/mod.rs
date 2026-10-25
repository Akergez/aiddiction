//! The window: [`shell::Shell`] is the one view it holds; the other modules
//! are its pages and dialogs.

mod chat;
mod forms;
mod project;
mod review;
mod servers;
mod settings_page;
pub(crate) mod shell;
mod sidebar;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AnyWindowHandle, App, Bounds, Global, Pixels, SharedString, Window, WindowBounds, WindowOptions, px, size};

struct MainWindow(AnyWindowHandle);

impl Global for MainWindow {}

pub fn open_window(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, initial_size(), cx))),
        window_min_size: Some(size(px(360.), px(480.))),
        app_id: Some(crate::APP_ID.to_string()),
        ..TitleBar::window_options()
    };
    match gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| shell::Shell::new(window, cx))) {
        Ok((handle, shell)) => {
            cx.set_global(MainWindow(handle));
            let _ = handle.update(cx, |_, window, _| window.set_window_title("Aiddiction"));
            crate::script::play(handle, shell, cx);
        }
        Err(error) => tracing::error!(%error, "could not open a window"),
    }
}

/// `AIDDICTION_SIZE=WxH` pins the window's size (phone layout: `400x800`).
fn initial_size() -> gpui_kit::Size<Pixels> {
    std::env::var("AIDDICTION_SIZE")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some(size(px(w.parse().ok()?), px(h.parse().ok()?)))
        })
        .unwrap_or_else(|| size(px(1100.), px(760.)))
}

/// Shows an error over the window, from anywhere with an `App`.
pub fn toast_error(message: impl Into<SharedString>, cx: &mut App) {
    let message = message.into();
    tracing::warn!("{message}");
    let Some(handle) = cx.try_global::<MainWindow>().map(|window| window.0) else { return };
    let _ = handle.update(cx, |_, window, cx| window.push_notification(Notification::error(message), cx));
}

/// The layout a window is wide enough for, read off its width in rems on
/// every frame (money-manager-ng's rule): below 44 rem one page at a time.
pub fn is_phone(window: &Window) -> bool {
    window.viewport_size().width / window.rem_size() < 44.0
}

/// How much of the top and the bottom the system draws over: the status
/// bar, and the navigation bar or the on-screen keyboard.
#[cfg(target_os = "android")]
pub fn insets() -> (Pixels, Pixels) {
    gpui_mobile::android::jni::platform()
        .and_then(|platform| platform.primary_window())
        .map(|window| {
            let insets = window.safe_area_insets_logical();
            (px(insets.top), px(insets.bottom))
        })
        .unwrap_or_default()
}

#[cfg(not(target_os = "android"))]
pub fn insets() -> (Pixels, Pixels) {
    (px(0.), px(0.))
}

/// How tall the on-screen keyboard is over `window`, nothing while it is
/// down. The platform reports one inset for whatever covers the bottom; one
/// too tall to be the navigation bar is the keyboard (money-manager-ng's
/// rule).
pub fn keyboard_inset(window: &Window) -> Pixels {
    let bottom = insets().1;
    if bottom > window.viewport_size().height * 0.2 { bottom } else { px(0.) }
}

/// Hands a link to whatever the system opens links with. Everything goes
/// through here: the Android platform's own `open_url` does nothing at all
/// (matterfast's finding).
pub fn open_url(url: &str, cx: &mut App) {
    #[cfg(not(target_os = "android"))]
    cx.open_url(url);
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        match gpui_mobile::packages::url_launcher::launch_url(url) {
            Ok(true) => {}
            Ok(false) => tracing::error!("nothing on this phone opens {url}"),
            Err(error) => tracing::error!(%error, "could not open {url}"),
        }
    }
}

/// The bar across the top of the window around `content`: the toolkit's
/// title bar on a desktop, a plain strip under the status bar on a phone.
pub fn title_frame(content: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    #[cfg(not(target_os = "android"))]
    {
        TitleBar::new().bg(theme.title_bar).border_color(theme.title_bar_border).child(content).into_any_element()
    }
    #[cfg(target_os = "android")]
    {
        gpui_kit::component::h_flex()
            .flex_none()
            .mt(insets().0)
            .h_12()
            .px_3()
            .border_b_1()
            .bg(theme.title_bar)
            .border_color(theme.title_bar_border)
            .child(content)
            .into_any_element()
    }
}
