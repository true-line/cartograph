//! Tauri build script: runs `tauri_build::build()` for the app shell.
//!
//! `APP_TEST_GROUP` (#1924) names the one group of app unit tests a CI shard
//! compiles: `acceptance` or `core`. Unset or empty builds every test, as
//! local runs and the lint job do. scripts/app-test-groups.json assigns each
//! test to its group, and the shard script refuses a test it leaves out.

const GROUPS: [&str; 2] = ["acceptance", "core"];

fn main() {
    println!("cargo::rerun-if-env-changed=APP_TEST_GROUP");
    println!("cargo::rustc-check-cfg=cfg(app_test_group, values(\"acceptance\", \"core\"))");
    match std::env::var("APP_TEST_GROUP").unwrap_or_default().as_str() {
        "" => {}
        group if GROUPS.contains(&group) => println!("cargo::rustc-cfg=app_test_group=\"{group}\""),
        group => panic!("APP_TEST_GROUP must be one of {GROUPS:?}, got {group:?}"),
    }
    tauri_build::build()
}
