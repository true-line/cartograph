//! AC-0156–0160 / AC-0163: run the production ownership modules and their
//! existing tests on native runners without linking the Tauri application.
//!
//! Keep these module names: the process-exit regression invokes the exact
//! `job_execution::tests::execution_owner_child` test in this same binary.

// JobStore persists this production data type, without running the semantic
// resolver. Import its exact definition and preserve the qualified type path.
extern crate self as semantic;
#[path = "../../../crates/semantic/src/eval_report.rs"]
mod eval_report;
pub use eval_report::EvalReport;
// As with the production JobStore below, this harness imports the exact data
// definitions without adopting the application API documentation surface.
#[allow(missing_docs)]
#[path = "../../../crates/semantic/src/family.rs"]
mod family;
pub use family::{FamilyKey, HopKind};
// report.rs carries its own inner missing_docs allow (#1764).
#[path = "../../../crates/semantic/src/evaluation/report.rs"]
pub mod evaluation_report;
/// Preserve the production type path without linking the ANN resolver.
pub mod evaluation {
    pub use crate::evaluation_report as report;
}

// The small harness omits host callers and the app's public API documentation
// surface. These allowances apply only to this test target; implementation
// errors and Clippy diagnostics retain their normal gates.
#[allow(dead_code, missing_docs, unused_imports)]
#[path = "../../../src-tauri/src/jobs.rs"]
mod jobs;

#[allow(dead_code)]
#[path = "../../../src-tauri/src/job_execution.rs"]
mod job_execution;

// The metadata writer's read-only retained identity view uses the same real
// state.db registry validator as the host, rather than a test-only substitute.
#[allow(dead_code, missing_docs)]
#[path = "../../../src-tauri/src/findings.rs"]
mod findings;
#[allow(dead_code)]
#[path = "../../../src-tauri/src/paths.rs"]
mod paths;
#[allow(dead_code, missing_docs, unused_imports)]
#[path = "../../../src-tauri/src/sources.rs"]
mod sources;
