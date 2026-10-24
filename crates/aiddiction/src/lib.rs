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
//! - [`session`] and [`thread`] — one conversation with an agent;
//! - [`ui`] — the window.

mod appearance;
// The demo server is a Unix program.
#[cfg(all(unix, not(target_os = "android")))]
mod demo;
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
    diffreview_ui::init(cx);
    aiddiction_client::install_crypto_provider();
    runtime::install(cx);
    settings::Settings::install(cx);
    appearance::init(cx);
    appearance::apply_mode(None, cx);
    #[cfg(all(unix, not(target_os = "android")))]
    demo::start_if_requested(cx);
    ui::open_window(cx);
}

/// The Android entry point, called by `android-activity` on a thread of its
/// own once the activity has loaded this library. As in money-manager-ng.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use gpui_mobile::android::jni;
    use std::sync::atomic::{AtomicBool, Ordering};

    // The toolkit belongs to the thread it first ran on and cannot be started
    // twice: a second activity in a kept process ends it, and the system
    // starts over in a fresh one.
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        std::process::exit(0);
    }

    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("aiddiction")
            .with_filter(android_logger::FilterBuilder::new().parse("warn,aiddiction=info").build()),
    );
    jni::install_panic_hook();

    // The application's private directory is the whole of where settings
    // are kept; `paths` reads these on first use.
    if let Some(files) = app.internal_data_path() {
        // SAFETY: no other thread of ours exists yet to read the environment.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", files.join("config"));
            std::env::set_var("XDG_DATA_HOME", files.join("data"));
        }
    }

    jni::init_platform(&app);
    let Some(platform) = jni::shared_platform() else {
        tracing::error!("no Android platform to run on");
        return;
    };

    gpui_kit::gpui::Application::with_platform(platform.into_rc())
        .with_assets(gpui_kit::assets::Assets)
        .run(start);

    std::process::exit(0);
}
