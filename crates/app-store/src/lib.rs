//! The app's durable local stores, compiled as their own crate so a change
//! elsewhere in the Tauri shell does not recompile them and their tests run
//! in their own harness (#1924). The modules keep their sources, and their
//! `crate::` paths, under `src-tauri/src`; the app re-exports them at its
//! crate root, so callers there are unchanged.

// The modules were written as part of the `app` binary, which has no public
// API documentation surface; they keep that surface here.
#![allow(missing_docs)]

#[path = "../../../src-tauri/src/findings.rs"]
pub mod findings;
#[path = "../../../src-tauri/src/paths.rs"]
pub mod paths;
#[path = "../../../src-tauri/src/sources.rs"]
pub mod sources;
