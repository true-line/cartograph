//! Separately versioned @10 acceptance machinery. No live stage is implicit.
//! Scripted tests establish mechanics only, not AC-0191 provider acceptance.
#[path = "admission/artifacts.rs"]
mod artifacts;
#[path = "admission/records.rs"]
mod records;
