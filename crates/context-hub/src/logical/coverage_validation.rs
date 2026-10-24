//! Temporary owners in CoverageProof::validate / validate_inventories.
use super::{count, json_bytes, map, slots, sum};
use crate::coverage::{CoverageBasis, MAX_COVERAGE_IDENTITY_BYTES, RepoQualification};
use serde_json::Value;
use std::collections::BTreeMap;

/// Additional logical workspace for proof validation and sequential batch
/// inventory validation. Inputs remain borrowed; their typed owners and decode
/// workspace must be reserved separately. This is not an allocator/RSS bound.
///
/// `proof_bytes` bounds compact serialization of the *typed* CoverageProof,
/// not its original JSON (defaults may expand it). `inventory_bytes` bounds
/// each retained body and `repos` bounds proof repo/basis entries. Unknown batch
/// keys return a fixed error; this does NOT cover validate_inventory's clone
/// of an arbitrary unmatched input key. No policy ceilings are changed.
pub fn coverage_batch_validation_workspace(
    proof_bytes: usize,
    inventory_bytes: usize,
    repos: usize,
) -> Option<usize> {
    // Both identity inputs are subsets of the typed proof. Include their
    // tuple/object wrappers before bounding Value conversion, canonicalization
    // container rebuilds (payload moves), serialized bytes and returned hash.
    let wire = sum(&[
        proof_bytes,
        count(&("coverage-context-v1", Value::Null))?,
        count(&(
            "context-v2",
            serde_json::json!({"snapshot_id":"","coverage_id":""}),
        ))?,
    ])?;
    let values = wire.checked_add(1)?;
    let identity = sum(&[
        json_bytes(wire)?,
        map::<String, Value>(values)?,
        slots::<Value>(values)?,
        wire,
        slots::<String>(1)?,
        "coverage-context-v1:".len(),
        blake3::OUT_LEN.checked_mul(2)?,
    ])?;
    // The lookup map survives each inventory's parse/derive. Parsing can
    // succeed before a malformed field rejects: charge the full Value, escape
    // scratch, copied commit/hints and sorting workspace even on that path.
    let inventory = sum(&[
        slots::<BTreeMap<&str, (&RepoQualification, &CoverageBasis)>>(1)?,
        map::<&str, (&RepoQualification, &CoverageBasis)>(repos)?,
        json_bytes(inventory_bytes)?,
        inventory_bytes,
        inventory_bytes,
        slots::<String>(inventory_bytes.checked_add(1)?)?,
        slots::<String>(inventory_bytes.checked_add(1)?)?,
        slots::<Vec<String>>(1)?,
        slots::<String>(2)?,
        MAX_COVERAGE_IDENTITY_BYTES,
        blake3::OUT_LEN.checked_mul(2)?,
    ])?;
    Some(identity.max(inventory))
}
