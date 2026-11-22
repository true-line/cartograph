//! The Cartograph desktop executable: the application is the `app` library
//! (#1227).

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    app::run();
}
