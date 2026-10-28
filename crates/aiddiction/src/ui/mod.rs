//! The window: [`shell::Shell`] is the one view it holds; the other modules
//! are its pages and dialogs.

mod agents;
mod chat;
mod forms;
mod inbox;
mod nav;
mod new_session;
mod project;
mod review;
mod servers;
mod settings_page;
pub(crate) mod shell;

use gpui_kit::component::notification::Notification;
use gpui_android_glue::keyboard::Purpose;
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::{TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, Bounds, Entity, EventEmitter, Global, Pixels, SharedString, Window, WindowBounds,
    WindowOptions, px, size,
};

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

/// How much of the top and the bottom the system's bars draw over: the
/// status bar and the navigation bar, never the on-screen keyboard
/// (`keyboard_inset`). Nothing on a desktop.
pub fn insets() -> (Pixels, Pixels) {
    gpui_android_glue::insets::system_bars()
}

/// How tall the on-screen keyboard is over `window` in this frame (the
/// navigation bar under it included), nothing while it is down. While it
/// slides this is how far it has come: read it in `render`, never keep it.
pub use gpui_android_glue::keyboard::keyboard_inset;

/// Tells a phone's keyboard what `input` (an `InputState` or a
/// `TextareaState`) is for whenever it has the focus: a keyboard for prose,
/// with capitals and corrections, for a message; one for addresses for a URL;
/// one that learns nothing for a token. Nothing off Android. The subscription
/// lasts as long as the input.
pub fn keyboard_for<S: EventEmitter<InputEvent>>(input: &Entity<S>, purpose: Purpose, cx: &mut App) {
    cx.subscribe(input, move |input, event: &InputEvent, _| {
        gpui_android_glue::keyboard::follow(input.entity_id(), event, purpose)
    })
    .detach();
}

/// Hands a link to whatever the system opens links with. Everything goes
/// through here: the Android platform's own `open_url` does nothing at all
/// (matterfast's finding).
pub use gpui_android_glue::open_url;

/// The bar across the top of the window around `content`: the toolkit's
/// title bar on a desktop, a plain strip under the status bar on a phone.
pub fn title_frame(content: impl IntoElement, cx: &App) -> AnyElement {
    let frame = gpui_android_glue::chrome::Frame { below_status_bar: true, ..Default::default() };
    gpui_android_glue::chrome::title_frame(content, frame, cx)
}
