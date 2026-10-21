use super::*;
use crate::jobs::investigations::{SourceValidationInitialData, captured_source_fixture};
use context_hub::coverage::{CoverageQualification, ScopedRepo};
use serde_json::json;

// AC-0287: production adapter, real source/retention stores, real persisted owner,
// invocation and charge. Only admission into the not-yet-public task3 family is
// explicit test setup; all stamps are built/sealed through production code.
#[test]
fn prepared_source_reserved_adapter_reads_only_after_real_charge_commit() {
    adapter(false, false);
}
#[test]
fn prepared_source_reserved_adapter_cancel_after_commit_reads_zero_bytes() {
    adapter(true, false);
}
#[test]
fn prepared_source_reserved_adapter_lost_charge_ack_reads_zero_bytes() {
    adapter(false, true);
}
fn adapter(cancel_after_commit: bool, lose_ack: bool) {
    let mut actual = None;
    let saved = captured_source_fixture(|private| {
        let state = Fixture::state_at(private);
        let (mut context, target) = Fixture::context_for(&state);
        let qualification = CoverageQualification::new(
            &[ScopedRepo {
                repo_key: target.source.repo.clone(),
                registered_source_id: Some(target.registered_source_id.clone()),
                config_hash: "source-validation-fixture".into(),
                layer_hints: None,
            }],
            &[],
        )
        .unwrap();
        let proof = qualification
            .durable_proof(&context.ledger.graph_snapshot_id)
            .unwrap();
        context.ledger.schema_version = 2;
        context.ledger.coverage = Some(proof.clone());
        context.policy = &INVESTIGATION_V2_POLICY;
        for (raw, manifest) in context
            .query_pages
            .iter_mut()
            .zip(&mut context.ledger.queries)
        {
            let mut page: serde_json::Value = serde_json::from_str(raw).unwrap();
            let response = page["context"].take();
            page["context"] = json!({"context_id":proof.context_id,"coverage":proof.qualification,"response":response});
            *raw = page.to_string();
            manifest.response_hash = content_hash(raw.as_bytes());
            manifest.response_bytes = raw.len();
        }
        let archive = InvestigationCoverageArchive::new(proof, vec![]).unwrap();
        context.coverage_archive = Some(archive.clone());
        context.ledger.validate_with_policy(context.policy).unwrap();
        let data = SourceValidationInitialData {
            ledger: context.ledger.clone(),
            query_pages: context.query_pages.clone(),
            archive,
            target: target.clone(),
        };
        actual = Some((state, context, target, private.to_owned()));
        data
    });
    let (state, context, target, private) = actual.unwrap();
    reset_reads();
    let prepared = context.prepare_captured_source(&state, &target).unwrap();
    assert_eq!(read_count(), 0);
    if cancel_after_commit {
        let job = saved.execution().id();
        AFTER_CHARGE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                rusqlite::Connection::open(private.join("state.db"))
                    .unwrap()
                    .execute("UPDATE jobs SET status='cancelled' WHERE id=?1", [job])
                    .unwrap();
            }))
        });
    }
    if lose_ack {
        saved.lose_charge_ack();
    }
    let outcome = prepared.read_reserved(&context, &state, saved.execution(), &saved.basis());
    if cancel_after_commit || lose_ack {
        let failure = match outcome {
            Err(failure) => failure,
            Ok(_) => panic!("failed access fence read source"),
        };
        assert_eq!(read_count(), 0);
        assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
        assert_eq!(failure.validation_receipt().is_some(), cancel_after_commit);
        assert!(context.evidence.is_empty());
    } else {
        let candidate = match outcome {
            Ok(candidate) => candidate,
            Err(error) => panic!("reserved adapter failed: {:?}", error.kind()),
        };
        assert_eq!(read_count(), 1);
        assert!(target.matches(candidate.ledger().citations.last().unwrap()));
        assert!(candidate.validation_receipt().is_some());
        assert!(context.evidence.is_empty());
        candidate.revalidate(&context, &state).unwrap();
    }
    assert_eq!(
        saved.charged_bytes_after_reopen(),
        target.captured.file.byte_len
    );
}
