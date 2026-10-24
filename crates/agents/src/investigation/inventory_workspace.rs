//! Inventory preparation DTO ownership, before durable publication.
use super::*;
use context_hub::logical::{count, json_bytes, slots, string_wire, sum};

/// Logical component owners; not an allocator or process RSS bound.
#[derive(Debug, Clone, Copy)]
pub struct InventoryPreparationWorkspace {
    /// Two owned ledgers and raw detail, excluding the host wrapper header.
    pub retained: usize,
    /// Peak including validation and temporary detail. The host separately
    /// accounts its wrapper, borrowed frozen context/request and publication.
    pub peak: usize,
}

/// Model the host's validate/read/encode/clone/append/validate sequence.
/// `ledger_wire` must bound typed base-ledger serialization BEFORE entry;
/// final manifest admission follows candidate allocation. No worker is admitted
/// by this calculation, and later prompt/provider ownership is not included.
pub fn inventory_preparation_workspace(
    policy: &InvestigationPolicy,
    ledger_wire: usize,
) -> Option<InventoryPreparationWorkspace> {
    let manifest = InvestigationCoverageReadManifest {
        request: InvestigationCoverageRead {
            context_id: String::new(),
            repo: String::new(),
            inventory_hash: String::new(),
            max_bytes: usize::MAX,
        },
        response_hash: String::new(),
        response_bytes: usize::MAX,
    };
    // Successful archive lookup proves copied identity limits. Empty quotes in
    // the DTO envelope are conservatively counted again by string_wire.
    let append = sum(&[
        count(&manifest)?,
        string_wire("context-v2:".len().checked_add(64)?)?,
        string_wire(context_hub::coverage::MAX_COVERAGE_IDENTITY_BYTES)?,
        string_wire(64)?,
        string_wire(64)?,
        1,                          // new array separator
        u64::MAX.to_string().len(), // possible revision digit growth
    ])?;
    let candidate_wire = ledger_wire.checked_add(append)?;
    let base = sum(&[
        slots::<InvestigationInputLedger>(1)?,
        json_bytes(ledger_wire)?,
    ])?;
    let candidate = sum(&[
        slots::<InvestigationInputLedger>(1)?,
        json_bytes(candidate_wire)?,
    ])?;
    let raw = sum(&[MAX_QUERY_BYTES, slots::<Vec<u8>>(1)?])?;
    let retained = sum(&[base, candidate, raw])?;
    let (detail, archive_peak) = InvestigationCoverageArchive::read_workspace_bytes()?;
    // Detail remains live while candidate validation and the second ledger
    // clone occur; converting the raw Vec to String moves its allocation.
    let construction = sum(&[
        retained,
        detail,
        ledger_validation_workspace(policy, candidate_wire)?,
        slots::<InvestigationQueryBudget>(2)?,
    ])?;
    let peak = ledger_validation_workspace(policy, ledger_wire)?
        .max(archive_peak)
        .max(construction);
    Some(InventoryPreparationWorkspace { retained, peak })
}
