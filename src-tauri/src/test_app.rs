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
//! once nothing else holds a handle.
//!
//! Removing the resolver is only sound while no other thread can be inside
//! `path()`, so it happens only when no handle outlives the owner. A test whose
//! handles may outlive it (a detached worker, a returned callback handle) calls
//! [`TestApp::retain_with_escaped_handles`]: that app is left exactly as Tauri
//! leaves it, state and all. An unmarked app whose handle outlives it fails the
//! test, so a new escape cannot silently race the removal.
use std::cell::Cell;
use std::ops::Deref;
use std::sync::{Arc, Weak};

use tauri::Manager;
use tauri::test::MockRuntime;

pub(crate) struct TestApp {
    app: Option<tauri::App<MockRuntime>>,
    /// Dropped only with the app's manager, so it shows whether a handle
    /// outlived this owner.
    alive: Weak<()>,
    retained: Cell<bool>,
}

/// Managed only so the owner can tell whether the app was freed.
struct Liveness(#[allow(dead_code)] Arc<()>);

impl TestApp {
    pub(crate) fn new(app: tauri::App<MockRuntime>) -> Self {
        let alive = Arc::new(());
        let weak = Arc::downgrade(&alive);
        app.manage(Liveness(alive));
        Self {
            app: Some(app),
            alive: weak,
            retained: Cell::new(false),
        }
    }

    /// Keep this app and its state for the rest of the process, because a
    /// handle cloned out of it may still be in use when the owner drops.
    pub(crate) fn retain_with_escaped_handles(&self) {
        self.retained.set(true);
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
        if self.retained.get() {
            // Tauri's own cycle keeps it, as for an app built without TestApp.
            drop(app);
            return;
        }
        let handle = app.handle();
        for name in TAURI_PLUGINS {
            handle.remove_plugin(name);
        }
        // The one removal that is not a plain API: Tauri's resolver holds the
        // last internal handle. `&mut self` proves no `State` borrowed through
        // this owner is alive, and an unmarked owner promises no other handle
        // is in use; the check below fails the test that breaks that promise.
        #[allow(deprecated)]
        drop(app.unmanage::<tauri::path::PathResolver<MockRuntime>>());
        drop(app);
        if self.alive.upgrade().is_some() && !std::thread::panicking() {
            panic!(
                "a handle outlived its TestApp; call retain_with_escaped_handles() \
                 before handing a handle to work that can outlive the test"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_retained_app_keeps_its_state_and_path_for_escaped_handles() {
        let held = Arc::new(());
        let app = mock_app();
        app.manage(Held(held.clone()));
        app.retain_with_escaped_handles();
        let handle = app.handle().clone();
        drop(app);
        assert!(handle.try_state::<Held>().is_some());
        assert!(handle.path().app_data_dir().is_ok());
        drop(handle);
        assert_eq!(Arc::strong_count(&held), 2);
    }

    #[test]
    fn an_unmarked_handle_that_outlives_its_owner_fails_the_test() {
        let app = mock_app();
        let handle = app.handle().clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(app)));
        assert!(outcome.is_err());
        drop(handle);
    }
}
