// AC-0512: immutable bounded overlay capture; official compilation remains legacy.
use super::*;
use crate::mcp_reads::KnowledgeBasis;
#[cfg(unix)]
use crate::mcp_reads::ReadBasis;
#[cfg(unix)]
use core_graph::overlay::BaseRef;
use core_graph::{
    RevisionedSnapshot,
    overlay::{OverlayFact, OverlayItem, OverlayRecipe, OverlayStore},
};

pub(super) struct Fixture {
    pub(super) graph: Mutex<SqliteGraphStore>,
    pub(super) overlay: Mutex<OverlayStore>,
    pub(super) decisions: Mutex<DecisionLog>,
    pub(super) facts: Vec<OverlayFact>,
    pub(super) dir: tempfile::TempDir,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = br#"import {Resolver, Query, Mutation} from '@nestjs/graphql';
@Resolver() export class ShopOrderResolver {
@Query() order() {} @Mutation() addItemToOrder() {} }"#;
        let mut extraction = adapters_lang_ts::extract_source(
            source,
            "packages/core/src/api/resolvers/shop/shop-order.resolver.ts",
            &adapters_lang_ts::SourceId {
                repo: "vendurehq/vendure",
                commit: "fixed",
            },
        )
        .unwrap();
        // Match production ingestion before the FK-enforcing store writes.
        extraction.close_over_endpoints();
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        for n in &extraction.nodes {
            graph.put_node(n).unwrap();
        }
        for e in &extraction.edges {
            graph.put_edge(e).unwrap();
        }
        let base = graph
            .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
            .unwrap();
        let facts = spec::derive_graphql_resolver_capabilities(&base).facts;
        assert_eq!(facts.len(), 1);
        let mut overlay = OverlayStore::open(dir.path().join("overlay.db")).unwrap();
        overlay
            .bind_instance(&base.revision.graph_instance)
            .unwrap();
        Self {
            dir,
            graph: Mutex::new(graph),
            overlay: Mutex::new(overlay),
            decisions: Mutex::new(DecisionLog::open(":memory:").unwrap()),
            facts,
        }
    }
    pub(super) fn base(&self) -> RevisionedSnapshot {
        self.graph
            .lock()
            .unwrap()
            .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
            .unwrap()
    }
    pub(super) fn publish(&self, facts: Vec<OverlayFact>) {
        let base = self.base();
        self.overlay
            .lock()
            .unwrap()
            .publish(
                &base,
                OverlayRecipe {
                    recipe: "host-fixture@1".into(),
                    embedding_namespace: None,
                    provider_ran: false,
                },
                facts,
            )
            .unwrap();
    }
    pub(super) fn acquire(
        &self,
        mode: ExportMode,
        limits: SnapshotReadLimits,
    ) -> Result<ReadSession, crate::mcp_reads::ReadError> {
        ReadSession::acquire_with_overlay(
            &self.graph,
            Some(&self.overlay),
            &self.decisions,
            || Ok(vec![]),
            limits,
            CoverageReadLimits::default(),
            mode,
        )
    }
    pub(super) fn best(&self) -> ReadSession {
        self.acquire(ExportMode::BestEffort, SnapshotReadLimits::default())
            .unwrap()
    }
}

#[test]
fn real_producer_selection_is_immutable_and_keeps_queries_and_compilation_base_only() {
    let f = Fixture::new();
    let before = f.base();
    let absent = f.best();
    assert_eq!(absent.basis.knowledge, KnowledgeBasis::Absent);
    f.publish(f.facts.clone());
    let captured = f.best();
    assert!(matches!(
        captured.basis.knowledge,
        KnowledgeBasis::Current(_)
    ));
    assert_eq!(captured.capabilities, f.facts);
    let verified = f
        .acquire(ExportMode::VerifiedOnly, SnapshotReadLimits::default())
        .unwrap();
    assert_eq!(verified.capabilities, captured.capabilities); // no citation stripping
    let request = || context_hub::coverage::QualifiedRequest {
        request: context_hub::QueryRequest::default(),
        context_id: None,
    };
    assert_eq!(
        serde_json::to_value(absent.query(request()).unwrap()).unwrap(),
        serde_json::to_value(captured.query(request()).unwrap()).unwrap()
    );
    assert_eq!(produced(&absent).artifact, produced(&captured).artifact);
    // B1 deliberately captures without adopting the sidecar in any compiler.
    let compile = |s: &ReadSession| {
        s.compile_artifact("user-stories", trace_limits(), compile_limits())
            .unwrap()
    };
    let (ArtifactCompilation::Produced(a), ArtifactCompilation::Produced(b)) =
        (compile(&absent), compile(&captured))
    else {
        panic!("legacy fits")
    };
    assert_eq!(a.artifact, b.artifact); // B2 replaces this non-adoption assertion.
    let old_basis = captured.basis_id().unwrap();
    let ptr = captured.base.graph.0.as_ptr();
    let mut changed = f.facts.clone();
    let OverlayItem::Node(n) = &mut changed[0].item else {
        unreachable!()
    };
    n.props["title"] = json!("updated knowledge result");
    n.props["prov"]["content_hash"] = json!(core_prov::content_hash(b"updated knowledge result"));
    f.publish(changed.clone());
    let next = f.best();
    assert_eq!(next.capabilities, changed);
    assert_ne!(next.basis_id().unwrap(), old_basis);
    assert_eq!(captured.capabilities, f.facts);
    assert_eq!(captured.basis_id().unwrap(), old_basis);
    assert_eq!(captured.base.graph.0.as_ptr(), ptr);
    assert_eq!(f.base(), before);
}

#[test]
fn stale_selection_is_empty_and_malformed_stale_metadata_is_storage_failure() {
    let f = Fixture::new();
    let before = f.base();
    f.publish(f.facts.clone());
    let mut changed = before.graph.0[0].clone();
    changed.props["changed"] = json!(true);
    f.graph.lock().unwrap().put_node(&changed).unwrap();
    let stale = f.best();
    assert_eq!(
        stale.basis.knowledge,
        KnowledgeBasis::Stale(before.revision)
    );
    assert!(stale.capabilities.is_empty());
    rusqlite::Connection::open(f.dir.path().join("overlay.db"))
        .unwrap()
        .execute(
            "UPDATE overlay_meta SET basis=json_set(basis, '$.base.graph_instance', 'bad')",
            [],
        )
        .unwrap();
    assert!(matches!(
        f.acquire(ExportMode::BestEffort, SnapshotReadLimits::default()),
        Err(ReadError::Storage)
    ));
}

#[test]
fn selection_exact_and_one_under_limits_keep_corruption_over_overflow_precedence() {
    let f = Fixture::new();
    let base = f.base();
    let mut padded = f.facts.clone();
    let OverlayItem::Node(n) = &mut padded[0].item else {
        unreachable!()
    };
    n.props["padding"] = json!("x".repeat(serde_json::to_vec(&base.graph).unwrap().len() * 2));
    let bytes = serde_json::to_vec(&padded[0]).unwrap().len();
    f.publish(padded.clone());
    let limit = SnapshotReadLimits {
        max_bytes: bytes,
        ..SnapshotReadLimits::default()
    };
    assert_eq!(
        f.acquire(ExportMode::BestEffort, limit)
            .unwrap()
            .capabilities,
        padded
    );
    assert!(matches!(
        f.acquire(
            ExportMode::BestEffort,
            SnapshotReadLimits {
                max_bytes: bytes - 1,
                ..limit
            }
        ),
        Err(ReadError::AdmissionLimit)
    ));
    let rows = base.graph.0.len() + base.graph.1.len();
    let copies = |count| {
        (0..count)
            .map(|i| {
                let mut fact = f.facts[0].clone();
                let OverlayItem::Node(n) = &mut fact.item else {
                    unreachable!()
                };
                n.id = format!("capcopy:{i}");
                fact
            })
            .collect::<Vec<_>>()
    };
    let limit = SnapshotReadLimits {
        max_rows: rows + 1,
        ..SnapshotReadLimits::default()
    };
    let mut overflow = copies(rows + 1);
    f.publish(overflow.clone());
    assert_eq!(
        f.acquire(ExportMode::BestEffort, limit)
            .unwrap()
            .capabilities
            .len(),
        rows + 1
    );
    let limit = SnapshotReadLimits {
        max_rows: rows,
        ..limit
    };
    assert!(matches!(
        f.acquire(ExportMode::BestEffort, limit),
        Err(ReadError::AdmissionLimit)
    ));
    let mut bad = f.facts[0].clone();
    let OverlayItem::Node(n) = &mut bad.item else {
        unreachable!()
    };
    n.id = "zz-corrupt".into();
    n.label = "Actor".into();
    overflow.push(bad);
    f.publish(overflow);
    // Node keys sort the Actor after all admitted/overflowing capcopy rows.
    rusqlite::Connection::open(f.dir.path().join("overlay.db")).unwrap()
        .execute("UPDATE overlay_facts SET fact=json_set(fact, '$.cites', json('[]')) WHERE fact LIKE '%zz-corrupt%'", []).unwrap();
    assert!(matches!(
        f.acquire(ExportMode::BestEffort, limit),
        Err(ReadError::Storage)
    ));
}

#[cfg(unix)]
#[test]
fn captured_selection_fits_named_admission_and_every_basis_variant_digest_owner() {
    use crate::mcp_reads::reservation::session::SessionReservation;
    use context_hub::logical::count;
    let f = Fixture::new();
    f.publish(f.facts.clone());
    let base = f.base();
    let mut limits = crate::mcp_reads::worker::tests::limits();
    limits.graph.max_rows = base.graph.0.len() + base.graph.1.len() + 1;
    limits.graph.max_bytes = serde_json::to_vec(&base.graph).unwrap().len() * 2;
    let captured = f.acquire(ExportMode::BestEffort, limits.graph).unwrap();
    let bound = SessionReservation::for_limits(limits).unwrap();
    let (n, bytes) = crate::mcp_reads::capability_caps(limits.graph);
    let storage = core_graph::overlay::capability_read_workspace(n, bytes).unwrap();
    assert_eq!(
        bound.capability_selection_bytes,
        storage.retained_selection_bytes
    );
    let wire_bytes: usize = captured
        .capabilities
        .iter()
        .map(|f| serde_json::to_vec(f).unwrap().len())
        .sum();
    let cites: usize = captured.capabilities.iter().map(|f| f.cites.len()).sum();
    let typed = std::mem::size_of_val(captured.capabilities.as_slice())
        + cites * (std::mem::size_of::<BaseRef>() + 2 * std::mem::size_of::<usize>());
    assert!(bound.capability_selection_bytes >= wire_bytes + typed);
    assert!(bound.capability_read_peak_bytes >= storage.peak_bytes);
    assert!(bound.acquisition_peak_bytes >= bound.capability_read_peak_bytes);
    let retained = count(&(
        captured.basis(),
        &captured.base.revision,
        &captured.base.graph,
        &captured.capabilities,
        &captured.visible_facts,
        &captured.rejected_hashes,
    ))
    .unwrap();
    assert!(bound.retained_session_bytes >= retained);
    let changes: [fn(&mut crate::mcp_reads::worker::WorkerLimits); 2] =
        [|p| p.graph.max_rows += 1, |p| p.graph.max_bytes += 1];
    for change in changes {
        let mut larger = limits;
        change(&mut larger);
        assert!(
            SessionReservation::for_limits(larger)
                .unwrap()
                .capability_selection_bytes
                > bound.capability_selection_bytes
        );
    }
    assert_eq!(
        core_graph::overlay::capability_read_workspace(1, 1)
            .unwrap()
            .row_scratch_bytes,
        storage.row_scratch_bytes
    );
    let profile = ReadBasis::reservation_fixture();
    let mut basis = captured.basis().clone();
    basis.base_revision.generation = u64::MAX;
    for state in [
        KnowledgeBasis::Absent,
        KnowledgeBasis::Current(core_graph::overlay::OverlayIdentity(
            core_prov::content_hash(&[]),
        )),
        profile.knowledge.clone(),
    ] {
        basis.knowledge = state;
        assert!(count(&profile).unwrap() >= count(&basis).unwrap());
    }
    // Largest state has no profile slack to hide a missing Vec/String owner.
    let wire = serde_json::to_vec(&("mcp-read-basis-v1", &profile)).unwrap();
    let hash = core_prov::content_hash(&wire);
    let id = crate::mcp_reads::digest("mcp-read-basis-v1", &profile).unwrap();
    let owners = std::mem::size_of_val(&wire)
        + wire.len()
        + std::mem::size_of_val(&hash)
        + hash.len()
        + std::mem::size_of_val(&id)
        + id.len();
    assert_eq!(bound.basis_digest_bytes, owners);
}
