//! Aiddiction: projects and ACP agents on your own servers.
//!
//! The server keeps the projects and runs the agents; this application picks
//! a server, shows its projects, makes new ones, starts an agent in one and
//! talks to it. It is a library so that Android can load it: there a Java
//! activity loads this as a shared object and calls [`android_main`].
//! Everywhere else `main.rs` calls [`run`].
//!
//! - [`settings`] — the servers this installation knows;
//! - [`server`] — the server in use: projects, agents, instances;
//! - [`session`] and [`thread`] — one conversation with an agent, [`login`]
//!   the ways of signing its agent in;
//! - [`ui`] — the window.

mod appearance;
// The demo server is a Unix program.
#[cfg(all(unix, not(target_os = "android")))]
mod demo;
mod inbox;
mod login;
mod paths;
mod review;
mod runtime;
mod script;
mod server;
mod session;
mod settings;
mod thread;
mod ui;

/// Names the application to the system; directories are named after it.
pub const APP_ID: &str = "app.akergez.Aiddiction";

/// The desktop entry point: what `main` is.
#[cfg(not(target_os = "android"))]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,aiddiction=info".into()),
        )
        .init();

    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(start);
}

/// What every platform does once the toolkit is running.
fn start(cx: &mut gpui_kit::App) {
    gpui_kit::init(cx);
    // On Android `launch::run` has done this already.
    #[cfg(not(target_os = "android"))]
    gpui_android_glue::init(cx);
    diffreview_ui::init(cx);
    aiddiction_client::install_crypto_provider();
    runtime::install(cx);
    settings::Settings::install(cx);
    appearance::init(cx);
    appearance::apply_mode(None, cx);
    gpui_adaptive::init(input_mode(), cx);
    #[cfg(all(unix, not(target_os = "android")))]
    demo::start_if_requested(cx);
    ui::open_window(cx);
}

/// How the person points: a finger on a phone, a pointer elsewhere.
/// `AIDDICTION_INPUT=touch` draws a desktop window for a finger, which is
/// how the UI scenarios see the phone.
fn input_mode() -> gpui_adaptive::InputMode {
    if cfg!(target_os = "android") || std::env::var("AIDDICTION_INPUT").as_deref() == Ok("touch") {
        gpui_adaptive::InputMode::Touch
    } else {
        gpui_adaptive::InputMode::Pointer
    }
}

/// The Android entry point, called by `android-activity` on a thread of its
/// own once the activity has loaded this library. `launch::run` points the
/// XDG variables at the application's private directory, which is the whole
/// of where settings are kept, and hands the activity to the certificate
/// verifier the client's sockets use.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: gpui_android_glue::launch::AndroidApp) {
    use gpui_android_glue::launch::{Config, run};
    run(app, Config::new("aiddiction").filter("warn,aiddiction=info"), gpui_kit::assets::Assets, start);
}
