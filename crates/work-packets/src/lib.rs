//! Local work-packet primitives (SPEC-11).
//!
//! Canonical encoding does not validate a packet's evidence or authorize work.
//! The typed packet contract must be validated separately before persistence.

mod body;
mod canonical;
mod export;
mod freshness;
mod input;
mod selection;
mod store;

pub use body::*;
pub use canonical::{CanonicalError, CanonicalLimits, canonical_json};
pub use export::export_markdown;
pub use freshness::{
    FreshnessInput, FreshnessObservation, FreshnessResult, FreshnessState, assess_freshness,
};
pub use input::{
    JsonInputError, JsonInputLimit, JsonInputLimits, JsonPath, JsonPathPattern, JsonPathSegment,
    deserialize_json, deserialize_json_scoped, preflight_json, preflight_json_scoped,
};
pub use selection::{
    ArtifactSelection, DecisionSelection, EvidenceSelection, FactSelection, SelectionError,
};
pub use store::*;
