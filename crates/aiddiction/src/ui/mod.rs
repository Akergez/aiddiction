//! The window: [`shell::Shell`] is the one view it holds; the other modules
//! are its pages and dialogs.

mod agents;
mod board;
mod chat;
mod composer;
mod forms;
mod inbox;
mod nav;
mod new_session;
mod project;
mod review;
mod servers;
mod settings_page;
pub(crate) mod shell;
mod work;

use gpui_adaptive::AppFrame;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, Global, Pixels, SharedString, WindowBounds, WindowOptions, px,
    size,
};

struct MainWindow(AnyWindowHandle);

impl Global for MainWindow {}

pub fn open_window(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, initial_size(), cx))),
        window_min_size: Some(size(px(360.), px(480.))),
        app_id: Some(crate::APP_ID.to_string()),
        ..AppFrame::window_options()
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
    let _ = handle.update(cx, |_, window, cx| gpui_adaptive::Notice::error(message).push(window, cx));
}

/// Hands a link to whatever the system opens links with. Everything goes
/// through here: the Android platform's own `open_url` does nothing at all
/// (matterfast's finding).
pub use gpui_android_glue::open_url;
