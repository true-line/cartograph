//! Tauri build script: runs `tauri_build::build()` for the app shell.
//!
//! `APP_TEST_GROUP` (#1924) names the one group of app unit tests a CI shard
//! compiles: `acceptance` or `core`. Unset or empty builds every test, as
//! local runs and the lint job do. scripts/app-test-groups.json assigns each
//! test to its group, and the shard script refuses a test it leaves out.
//!
//! On Windows the app manifest (Common Controls v6, which the dialog APIs
//! import from) is embedded by the linker for every target, not only the
//! binary as `tauri_build` does, so the unit-test executable loads too (#227).

const GROUPS: [&str; 2] = ["acceptance", "core"];

fn main() {
    println!("cargo::rerun-if-env-changed=APP_TEST_GROUP");
    println!("cargo::rustc-check-cfg=cfg(app_test_group, values(\"acceptance\", \"core\"))");
    match std::env::var("APP_TEST_GROUP").unwrap_or_default().as_str() {
        "" => {}
        group if GROUPS.contains(&group) => println!("cargo::rustc-cfg=app_test_group=\"{group}\""),
        group => panic!("APP_TEST_GROUP must be one of {GROUPS:?}, got {group:?}"),
    }
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let windows = if windows_msvc {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
        println!("cargo::rerun-if-changed={}", manifest.display());
        println!("cargo::rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg=/MANIFESTINPUT:{}",
            manifest.display()
        );
        tauri_build::WindowsAttributes::new_without_app_manifest()
    } else {
        tauri_build::WindowsAttributes::new()
    };
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("tauri build script");
}
