use super::*;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use serde_json::json;

fn archive(prefix: &str) -> InvestigationCoverageArchive {
    let inventory = json!({"inventory":"coverage@1", "repo":"acme/api",
        "commit":"a".repeat(40), "layer_hints":[], "producers":[],
        "unsupported":[], "limited":false});
    let record = CoverageRecord {
        repo_key: "acme/api".into(),
        registered_source_id: Some("src:api".into()),
        ingest_id: "job:1".into(),
        config_hash: "plugins:1".into(),
        inventory_json: format!(
            "{prefix}{}",
            serde_json::to_string_pretty(&inventory).unwrap()
        ),
    };
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:api".into()),
            config_hash: "plugins:1".into(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    InvestigationCoverageArchive::new(
        qualification.durable_proof(snapshot.id()).unwrap(),
        vec![stored],
    )
    .unwrap()
}

#[test]
fn workflow_coverage_repeats_exact_retained_detail_with_nested_escaping() {
    // AC-0191: repeated inventory acquisitions cannot disappear from a full
    // budget component, and nested inventory JSON must retain both encodings.
    let archive = archive("");
    let bound = WorkflowCoverageDetailBound::derive(&archive, &INVESTIGATION_V2_POLICY).unwrap();
    let repo = &archive.proof().qualification.repos[0];
    let detail = archive
        .read(&InvestigationCoverageRead {
            context_id: archive.proof().context_id.clone(),
            repo: repo.repo.clone(),
            inventory_hash: repo.inventory_hash.clone().unwrap(),
            max_bytes: MAX_QUERY_BYTES,
        })
        .unwrap();
    let count = INVESTIGATION_V2_POLICY.limits().tool_actions as usize;
    let repeated = vec![detail; count];
    let raw = serde_json::to_string(&repeated).unwrap();
    assert_eq!(bound.maximum_read_count, count);
    assert_eq!(bound.details_array_json_bytes(), raw.len());
    assert_eq!(
        bound.details_array_as_prompt_string_bytes(),
        serde_json::to_vec(&raw).unwrap().len()
    );
    assert!(bound.details_array_as_prompt_string_bytes() > raw.len());
    assert_eq!(
        bound.status,
        "partial_component_only_no_full_workflow_fit_claim"
    );
}

#[test]
fn workflow_coverage_component_does_not_erase_an_unreadable_inventory() {
    // AC-0191: a failed bound contribution is never counted as zero/fit.
    let archive = archive(&" ".repeat(MAX_QUERY_BYTES));
    assert_eq!(
        WorkflowCoverageDetailBound::derive(&archive, &INVESTIGATION_V2_POLICY),
        Err(InvestigationError::LimitExceeded)
    );
}

#[test]
fn workflow_coverage_empty_archive_has_only_array_framing() {
    // AC-0191: an archive with no recorded detail has a real empty universe;
    // this partial component does not certify absent future contributions.
    let qualification = CoverageQualification::new(&[], &[]).unwrap();
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    let archive = InvestigationCoverageArchive::new(
        qualification.durable_proof(snapshot.id()).unwrap(),
        vec![],
    )
    .unwrap();
    let bound = WorkflowCoverageDetailBound::derive(&archive, &INVESTIGATION_V2_POLICY).unwrap();
    assert_eq!(bound.maximum_read_count, 0);
    assert_eq!(bound.details_array_json_bytes(), 2);
    assert_eq!(bound.details_array_as_prompt_string_bytes(), 4);
    assert_eq!(
        WorkflowCoverageDetailBound::derive(&archive, &INVESTIGATION_V1_POLICY),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn workflow_component_arithmetic_overflow_is_explicit() {
    // AC-0191: overflow must not become a fitting or zero contribution.
    assert_eq!(array_bytes(0, 0), Ok(2));
    assert_eq!(array_bytes(1, 3), Ok(5));
    assert_eq!(array_bytes(2, 3), Ok(9));
    assert_eq!(
        array_bytes(usize::MAX, 2),
        Err(InvestigationError::LimitExceeded)
    );
    assert_eq!(
        array_bytes(1, usize::MAX),
        Err(InvestigationError::LimitExceeded)
    );
}
