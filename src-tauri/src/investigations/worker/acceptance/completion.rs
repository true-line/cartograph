//! Separately staged V7 acceptance; no provider runs during ordinary tests.
#[path = "completion/control.rs"]
mod control;
#[path = "completion/execution.rs"]
pub(super) mod execution;
#[path = "completion/prepare.rs"]
pub(super) mod prepare;
#[path = "completion/records.rs"]
mod records;
