//! The desktop executable; the application itself is the library.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    // Android never runs this: the activity loads the library instead.
    #[cfg(not(target_os = "android"))]
    aidiction::run();
}
