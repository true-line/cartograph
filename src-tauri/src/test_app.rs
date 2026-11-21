//! Test-only owner of a Tauri mock app that frees what the app manages (#1621).
//!
//! Tauri 2.12 keeps a handle to the app inside the app: every registered
//! plugin stores an `AppHandle`, and the core path plugin manages
//! `PathResolver(AppHandle)`. Dropping an `App` therefore never drops its
//! managed state, so each test's `AppState` (SQLite stores, WAL files, locks)
//! stayed open until the test process exited, and a single-process macOS run
//! ran out of spawnable descriptors (EBADF past about 10,000).
//!
//! `TestApp` breaks those cycles when it is dropped: it unregisters Tauri's
//! own plugins and removes Tauri's own `PathResolver`, never application
//! state. What the app manages is then dropped by ordinary `Arc` ownership
//! once nothing else holds a handle. A handle cloned into a detached worker
//! keeps the whole app, and every `State` it reads, alive until it is dropped.
use std::ops::Deref;

use tauri::Manager;
use tauri::test::MockRuntime;

pub(crate) struct TestApp {
    app: Option<tauri::App<MockRuntime>>,
}

impl TestApp {
    pub(crate) fn new(app: tauri::App<MockRuntime>) -> Self {
        Self { app: Some(app) }
    }
}

/// A mock app with Tauri's test context and no assets.
pub(crate) fn mock_app() -> TestApp {
    TestApp::new(tauri::test::mock_app())
}

impl Deref for TestApp {
    type Target = tauri::App<MockRuntime>;

    fn deref(&self) -> &Self::Target {
        self.app.as_ref().expect("test app is present until drop")
    }
}

/// Plugins Tauri registers on every app (`register_core_plugins` and the IPC
/// channel plugin); the app's own code registers none in tests.
const TAURI_PLUGINS: [&str; 10] = [
    "path",
    "event",
    "window",
    "webview",
    "app",
    "resources",
    "image",
    "menu",
    "tray",
    "__TAURI_CHANNEL__",
];

impl Drop for TestApp {
    fn drop(&mut self) {
        let Some(app) = self.app.take() else { return };
        let handle = app.handle();
        for name in TAURI_PLUGINS {
            handle.remove_plugin(name);
        }
        // The one removal that is not a plain API: Tauri's resolver holds the
        // last internal handle. `&mut self` proves no `State` borrowed through
        // this owner is alive; a later `path()` through an escaped handle
        // panics as unmanaged instead of reading freed memory.
        #[allow(deprecated)]
        drop(app.unmanage::<tauri::path::PathResolver<MockRuntime>>());
        drop(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Held(#[allow(dead_code)] Arc<()>);

    #[test]
    fn a_plain_mock_app_keeps_its_managed_state_after_drop() {
        let held = Arc::new(());
        let app = tauri::test::mock_app();
        app.manage(Held(held.clone()));
        drop(app);
        // Documents the Tauri cycle this owner exists for; if Tauri stops
        // holding a handle in its own state, this fails and `TestApp` can go.
        assert_eq!(Arc::strong_count(&held), 2);
    }

    #[test]
    fn dropping_the_owner_drops_managed_state() {
        let held = Arc::new(());
        let app = mock_app();
        app.manage(Held(held.clone()));
        assert!(app.path().app_data_dir().is_ok());
        drop(app);
        assert_eq!(Arc::strong_count(&held), 1);
    }

    #[test]
    fn an_escaped_handle_keeps_state_until_it_is_dropped() {
        let held = Arc::new(());
        let app = mock_app();
        app.manage(Held(held.clone()));
        let handle = app.handle().clone();
        drop(app);
        assert!(handle.try_state::<Held>().is_some());
        assert_eq!(Arc::strong_count(&held), 2);
        drop(handle);
        assert_eq!(Arc::strong_count(&held), 1);
    }
}
