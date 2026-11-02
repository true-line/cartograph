//! AC-0287: real host preparation -> prompt/proof/seal -> owner writer -> install.
//! All source/capture data is isolated test setup; no intake or provider runs.
use super::*;
use crate::investigations::{
    context::metadata_admission::MetadataPublicationError, runtime::RunClock,
};
use crate::jobs::investigations::{MetadataAdmission, metadata_host_fixture as seed};
use agents::investigation::staged_refusal::CompletionCallMode;
use core_graph::coverage::CoverageRecord;
use llm::{
    EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider,
    bounded::{CompletionLimits, PreparedBoundedCompletion},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

#[path = "context_metadata_fixture.rs"]
mod fixture;
use fixture::*;

#[test]
fn host_metadata_query_and_inventory_publish_real_preparation_and_reopen() {
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        let mut f = Fixture::new();
        let publication = f.prepare(kind);
        let expected = publication.prepared.ledger().clone();
        let raw = publication.prepared.raw().to_owned();
        let detail = f
            .publish(publication, f.seed.detail.summary.revision)
            .unwrap();
        assert_eq!(f.context.ledger, expected);
        let installed = if kind == PreparedContextReadKind::Query {
            &f.context.query_pages
        } else {
            &f.context.inventory_details
        };
        assert_eq!(installed, &[raw]);
        let (reopened, ledger, ready) = f.reopened();
        assert!(ready);
        assert_eq!(reopened, detail);
        assert_eq!(ledger, expected);
        assert_eq!(detail.usage.tool_actions, 1);
    }
}

#[test]
fn host_metadata_cancellation_is_typed_and_installs_nothing() {
    let mut f = Fixture::new();
    let publication = f.prepare(PreparedContextReadKind::Query);
    let base = f.context.ledger.clone();
    let revision = seed::cancel(
        &mut f.state.jobs.lock().unwrap(),
        &f.seed.detail.summary.investigation_id,
    );
    let before = f.reopened();
    assert!(matches!(
        f.publish(publication, revision),
        Err(MetadataPublicationError::Store(
            crate::jobs::investigations::InvestigationStoreError::Cancelled
        ))
    ));
    assert_eq!(f.context.ledger, base);
    assert!(f.context.query_pages.is_empty());
    assert!(f.context.inventory_details.is_empty());
    assert_eq!(f.reopened(), before);
}

#[test]
fn host_metadata_retained_identity_callback_rejects_without_install_or_publication() {
    let mut f = Fixture::new();
    let publication = f.prepare(PreparedContextReadKind::Query);
    let before = f.reopened();
    let base = f.context.ledger.clone();
    let connection = rusqlite::Connection::open(f.directory.path().join("state.db")).unwrap();
    connection
        .execute("DELETE FROM registered_sources", [])
        .unwrap();
    assert!(matches!(
        f.publish(publication, f.seed.detail.summary.revision),
        Err(MetadataPublicationError::Host(HostError::SourceUnavailable))
    ));
    assert_eq!(f.context.ledger, base);
    assert!(f.context.query_pages.is_empty());
    assert_eq!(f.reopened(), before);
}

#[test]
fn host_metadata_lost_commit_ack_does_not_install_prepared_state() {
    for kind in [
        PreparedContextReadKind::Query,
        PreparedContextReadKind::Inventory,
    ] {
        let mut f = Fixture::new();
        let publication = f.prepare(kind);
        let base = f.context.ledger.clone();
        let committed = publication.prepared.ledger().clone();
        seed::lose_next_ack();
        assert!(matches!(
            f.publish(publication, f.seed.detail.summary.revision),
            Err(MetadataPublicationError::Store(
                crate::jobs::investigations::InvestigationStoreError::Storage
            ))
        ));
        assert_eq!(f.context.ledger, base);
        assert!(f.context.query_pages.is_empty());
        assert!(f.context.inventory_details.is_empty());
        let (_, ledger, ready) = f.reopened();
        assert!(ready);
        assert_eq!(ledger, committed);
        // The uncertain acknowledgement does not trigger preparation or retry.
    }
}

// #1617: classify only query-selection errors after host consistency checks.
#[test]
fn query_rejection_is_typed_without_weakening_host_validation() {
    let mut f = Fixture::new();
    let query = InvestigationQuery {
        scope: InvestigationScope::Neighborhood {
            anchor: "absent".into(),
            hops: 1,
        },
        kind: None,
        labels: vec![],
        max_facts: 1,
        max_bytes: 16384,
        cursor: None,
    };
    let before = f.context.ledger.clone();
    assert!(matches!(
        f.context
            .prepare_query_with_diagnostic(&f.state, query.clone()),
        Err(ContextReadPreparationError::QueryRejected(
            QueryRejection::UnknownAnchor
        ))
    ));
    assert_eq!(f.context.ledger, before);
    assert!(f.context.query_pages.is_empty());
    // A broken host tuple must take precedence over the same bad model selector.
    f.context.coverage_archive = None;
    assert!(matches!(
        f.context.prepare_query_with_diagnostic(&f.state, query),
        Err(ContextReadPreparationError::Host(HostError::InvalidInput))
    ));
}

#[test]
fn query_rejection_keeps_host_corruption_and_budget_terminal() {
    use context_hub::ContextError as E;
    for error in [
        E::InvalidCoverage("bad".into()),
        E::DuplicateNode("bad".into()),
        E::InventoryUnavailable("bad".into()),
        E::InventoryChanged("bad".into()),
    ] {
        assert_eq!(
            query_preparation_error(error),
            ContextReadPreparationError::Host(HostError::InvalidInput)
        );
    }
    assert_eq!(
        query_preparation_error(E::ResponseBudgetExceeded {
            max_bytes: 1,
            required_bytes: 2
        }),
        ContextReadPreparationError::Host(HostError::LimitExceeded)
    );
    for (error, reason) in [
        (E::UnknownNode("absent".into()), QueryRejection::UnknownNode),
        (
            E::CursorSnapshotMismatch,
            QueryRejection::CursorSnapshotMismatch,
        ),
        (
            E::CursorSelectionMismatch,
            QueryRejection::CursorSelectionMismatch,
        ),
        (
            E::CursorContextMismatch,
            QueryRejection::CursorContextMismatch,
        ),
        (
            E::InvalidCursorOffset {
                offset: 2,
                total_selected: 1,
            },
            QueryRejection::InvalidCursorOffset,
        ),
        (
            E::InvalidLimit {
                name: "hops",
                requested: 0,
                maximum: 2,
            },
            QueryRejection::InvalidLimit,
        ),
    ] {
        assert_eq!(
            query_preparation_error(error),
            ContextReadPreparationError::QueryRejected(reason)
        );
    }
}
