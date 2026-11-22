//! The app's durable local stores and job execution, compiled as their own crate so a change
//! elsewhere in the Tauri shell does not recompile them and their tests run
//! in their own harness (#1924). The modules keep their sources, and their
//! `crate::` paths, under `src-tauri/src`; the app re-exports them at its
//! crate root, so callers there are unchanged.

// The modules were written as part of the `app` binary, which has no public
// API documentation surface; they keep that surface here.
#![allow(missing_docs)]
// With `test-support` the job modules compile their shared test fixtures for
// the app's tests, as they did inside the app's test build; the `#[test]`
// functions that use some of them are compiled only under `cfg(test)`.
#![cfg_attr(
    all(feature = "test-support", not(test)),
    allow(dead_code, unused_imports)
)]

#[path = "../../../src-tauri/src/edge_merge.rs"]
pub mod edge_merge;
#[path = "../../../src-tauri/src/findings.rs"]
pub mod findings;
#[path = "../../../src-tauri/src/ingest_basis.rs"]
pub mod ingest_basis;
#[path = "../../../src-tauri/src/job_execution.rs"]
pub mod job_execution;
#[path = "../../../src-tauri/src/jobs.rs"]
pub mod jobs;
#[path = "../../../src-tauri/src/observation_inputs.rs"]
pub mod observation_inputs;
#[path = "../../../src-tauri/src/paths.rs"]
pub mod paths;
#[path = "../../../src-tauri/src/primary_source.rs"]
pub mod primary_source;
#[path = "../../../src-tauri/src/sources.rs"]
pub mod sources;
