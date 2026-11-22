//! Acquisition and retained-session logical payload roles, tied to acquire's
//! actual overlap. No native stacks, allocator slack, caches or RSS guarantee.
use crate::mcp_reads::{ReadBasis, ReadSession, scope::ScopeLimits, worker::WorkerLimits};
use context_hub::{
    FactReference,
    coverage::ScopedRepo,
    logical::{self, count, json_bytes, map, set, slots, string_wire, sum},
};
use std::{mem::size_of, path::PathBuf};

#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionReservation {
    pub acquisition_peak_bytes: usize,
    /// ReadSession only. Dispatcher request IDs depend on the transport frame
    /// limit and are exposed separately by request_ids_bytes.
    pub retained_session_bytes: usize,
    /// Wire Vec, hex-hash String and returned ID String while the session stays live.
    pub basis_digest_bytes: usize,
    #[cfg(test)]
    pub capability_selection_bytes: usize,
    #[cfg(test)]
    pub capability_read_peak_bytes: usize,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScopeReservation {
    pub peak_bytes: usize,
    pub result_bytes: usize,
}

/// Each completed observation owns raw source rows/ManagedOrigin display clone
/// and host owner metadata. No complete plugin runtime or artifact Vec exists.
pub(crate) fn scope_reservation(limits: ScopeLimits) -> Option<ScopeReservation> {
    use adapters_plugin_host::{
        discovery::{DiscoveredPlugin, HASH_READ_BUFFER_BYTES},
        gate::{DeclaredCell, ExpectedEdge, ExpectedNode, GoldenCase, GoldenCorpus},
    };
    let q = limits.max_sources.get();
    let sr = limits.max_source_registry_bytes.get();
    let entries = limits.max_directory_entries.get();
    let metadata = limits.max_metadata_bytes.get();
    let corpus = usize::try_from(limits.max_corpus_bytes.get()).ok()?;
    // Source SQL payload and the ManagedOrigin display-name clone are separate
    // owners. app-data path and registry-id are cloned for each source row.
    let sources = sum(&[
        sr,
        sr,
        slots::<crate::sources::RegisteredSource>(q)?,
        crate::sources::source_owner_metadata_bytes(q)?,
    ])?;
    let hash = core_prov::content_hash(&[]).len();
    let result_bytes = sum(&[slots::<ScopedRepo>(q)?, sr, q.checked_mul(hash)?])?;
    let events = adapters_fw::events::logical::registry_reservation(
        limits.max_event_registry_bytes.get(),
        metadata,
        entries,
    )?;
    let filenames = sum(&[
        metadata,
        slots::<(String, PathBuf)>(entries)?,
        slots::<(String, Vec<u8>)>(entries)?,
    ])?;
    let event_input = sum(&[
        limits.max_event_registry_bytes.get(),
        filenames,
        crate::mcp_reads::scope::READ_BUFFER_BYTES,
    ])?;
    let event_parse_peak = sum(&[event_input, events.layering_peak])?;
    // Typed corpus currency uses the largest real row header for each possible
    // input occurrence; it is not an imaginary serde Value retained by parsing.
    let corpus_rows = corpus.checked_add(1)?;
    let corpus_header = [
        size_of::<GoldenCase>(),
        size_of::<ExpectedNode>(),
        size_of::<ExpectedEdge>(),
        size_of::<DeclaredCell>(),
    ]
    .into_iter()
    .max()?;
    let parsed_corpus = sum(&[
        json_bytes(corpus)?,
        corpus_rows.checked_mul(corpus_header)?,
        size_of::<GoldenCorpus>(),
    ])?;
    let active_claims = sum(&[
        corpus,
        slots::<ingest::matrix::ReportedCell>(corpus_rows)?,
        slots::<crate::ActivePlugin>(entries)?,
    ])?;
    let discovered = sum(&[
        metadata,
        map::<String, DiscoveredPlugin>(entries)?,
        slots::<DiscoveredPlugin>(entries)?,
        HASH_READ_BUFFER_BYTES,
    ])?;
    // A current corpus's raw bytes and parsed DTO coexist while from_slice runs;
    // previous active cells/claims remain until coverage_config_hash completes.
    let corpus_parse = sum(&[
        discovered,
        metadata,
        corpus,
        parsed_corpus,
        active_claims,
        crate::mcp_reads::scope::READ_BUFFER_BYTES,
    ])?;
    // The config function owns pinned identifiers, then an actual JSON Value
    // and serialized buffer while all plugin claims and registries remain live.
    let pinned_suffix =
        adapters_plugin_host::pinned_extractor_id("", &core_prov::content_hash(&[]))
            .len()
            .checked_add("#".len())?
            .checked_add(hash)?;
    let pinned_payload = sum(&[metadata, entries.checked_mul(pinned_suffix)?])?;
    let config_wire = sum(&[
        string_wire(pinned_payload)?,
        entries.checked_mul(count(&" ")?)?,
        count(
            &serde_json::json!({"basis":"coverage-config-v2","plugins":[],"registries":core_prov::content_hash(&[])}),
        )?,
    ])?;
    let config_hash_peak = sum(&[
        discovered,
        active_claims,
        pinned_payload,
        slots::<String>(entries)?,
        json_bytes(config_wire)?,
        config_wire,
        // registries.identity builds borrowed source pairs and one canonical Vec.
        slots::<(&str, &str)>(
            entries.checked_add(adapters_fw::events::Registries::builtin().sources.len())?,
        )?,
        string_wire(metadata)?,
        entries.checked_mul(hash)?,
    ])?;
    let plugins = corpus_parse.max(config_hash_peak);
    let peak_bytes = sum(&[
        sources,
        events.retained,
        result_bytes,
        plugins.max(event_parse_peak),
    ])?;
    Some(ScopeReservation {
        peak_bytes,
        result_bytes,
    })
}

fn rejection_set_bytes() -> Option<usize> {
    sum(&[
        agents::MAX_CURATION_ROWS.checked_mul(core_prov::content_hash(&[]).len())?,
        set::<String>(agents::MAX_CURATION_ROWS)?,
    ])
}
fn curation_row_peak() -> Option<usize> {
    sum(&[
        agents::MAX_SELECTED_ASSERTION_ROW_BYTES,
        json_bytes(agents::MAX_SELECTED_ASSERTION_ROW_BYTES)?,
        size_of::<agents::AssertionDecisionRecord>(),
        core_prov::content_hash(&[]).len(),
    ])
}
/// Additional graph-only read/canonical identity workspace for evidence freshness
/// checks. Callers add this to retained_session_bytes; it is not a second session.
pub(crate) fn freshness_snapshot_peak_bytes(
    limits: core_graph::SnapshotReadLimits,
) -> Option<usize> {
    let snapshot = logical::snapshot(limits)?;
    sum(&[
        logical::graph(limits)?,
        snapshot.retained,
        snapshot.identity_peak,
        limits.max_bytes,
    ])
}
/// Persistent admission IDs plus one current key clone/serialization scratch.
/// Keys use id.to_string(); R+2 is input bytes plus quoted-string delimiters.
pub(crate) fn request_ids_bytes(ids: usize, read_frame_bytes: usize) -> Option<(usize, usize)> {
    let key = read_frame_bytes.checked_add("\"\"".len())?;
    Some((
        sum(&[ids.checked_mul(key)?, set::<String>(ids)?])?,
        sum(&[key, size_of::<String>()])?,
    ))
}

/// Additional discovery workspace, excluding the retained session and transport
/// response DOM. Frame-sized input labels/query canonicalization are separately
/// charged by the caller's argument workspace.
pub(crate) fn discovery_peak_bytes(limits: WorkerLimits) -> Option<usize> {
    logical::discovery_workspace_bytes(
        limits.graph,
        limits.coverage,
        limits.scope.max_sources.get(),
        limits.scope.max_source_registry_bytes.get(),
        limits.dispatch.max_response_bytes.get(),
    )
}

/// Additional `cartograph_query` workspace (AC-1809): the core query over the
/// session's borrowed projection, its neighborhood sets and page, and the
/// coverage wrapper. The request is at most one admitted frame; transport
/// framing and the response DOM are charged with the other roles.
pub(crate) fn query_peak_bytes(limits: WorkerLimits) -> Option<usize> {
    Some(
        logical::qualified_query_workspace(
            limits.graph,
            limits.coverage,
            limits.scope.max_sources.get(),
            limits.scope.max_source_registry_bytes.get(),
            limits.max_request_frame_bytes.get(),
        )?
        .peak_bytes,
    )
}

impl SessionReservation {
    pub(crate) fn for_limits(limits: WorkerLimits) -> Option<Self> {
        let scope = scope_reservation(limits.scope)?;
        let graph = logical::graph(limits.graph)?;
        let snapshot = logical::snapshot(limits.graph)?;
        let coverage = logical::coverage(
            limits.coverage,
            limits.scope.max_sources.get(),
            limits.scope.max_source_registry_bytes.get(),
        )?;
        let rejected = rejection_set_bytes()?;
        let basis = ReadBasis::reservation_fixture();
        let revision_payload = basis.base_revision.graph_instance.len();
        let revision = sum(&[size_of::<core_graph::StoreRevision>(), revision_payload])?;
        let basis_digest_bytes = ReadBasis::digest_workspace()?;
        let hash = core_prov::content_hash(&[]);
        let (n, bytes) = crate::mcp_reads::capability_caps(limits.graph);
        let overlay = core_graph::overlay::capability_read_workspace(n, bytes)?;
        // The Actor vector moved out of the selection holds at most n slots.
        let capability_selection_bytes = sum(&[
            overlay.retained_selection_bytes,
            slots::<core_graph::overlay::OverlayFact>(n)?,
        ])?;
        // The output recipe is dropped. Basis payload is already charged in
        // fixed_basis; pre-construction enum/vector headers still have owners.
        let captured = sum(&[
            capability_selection_bytes,
            count(&basis.knowledge)?,
            size_of::<crate::mcp_reads::KnowledgeBasis>(),
            // Capability and Actor vectors split one combined selection.
            slots::<Vec<core_graph::overlay::OverlayFact>>(2)?,
        ])?;
        let fixed_basis = sum(&[size_of::<ReadSession>(), count(&basis)?])?;
        let retained_session_bytes = sum(&[
            graph,
            snapshot.retained,
            snapshot.visibility,
            rejected,
            coverage.retained,
            // The stored inventories stay for the coverage artifacts
            // (AC-0657), beside the qualification's own retained copies.
            coverage.stored,
            fixed_basis,
            capability_selection_bytes,
            revision_payload, // ReadSession already owns the inline revision header
        ])?;
        let before_scope = sum(&[rejected, scope.peak_bytes])?;
        let bounded_read = sum(&[
            super::schema::workspace_bytes()?,
            rejected,
            scope.result_bytes,
            graph,
            limits.graph.max_bytes,
            coverage.stored,
            coverage.inventory_parse,
            revision,
        ])?;
        let capability_read_peak_bytes = sum(&[
            rejected,
            scope.result_bytes,
            graph,
            coverage.stored,
            revision,
            overlay.peak_bytes,
        ])?;
        let after_scope = sum(&[
            captured,
            revision,
            rejected,
            scope.result_bytes,
            graph,
            coverage.stored,
            scope.peak_bytes,
        ])?;
        let comparisons = sum(&[
            captured,
            revision,
            rejected,
            rejected,
            scope.result_bytes,
            scope.result_bytes,
            graph,
            coverage.stored,
            curation_row_peak()?,
        ])?;
        let common_identity = sum(&[
            captured,
            revision,
            rejected,
            rejected,
            scope.result_bytes,
            scope.result_bytes,
            graph,
            coverage.stored,
        ])?;
        let revision_recheck =
            sum(&[common_identity, revision, super::schema::workspace_bytes()?])?;
        let raw_identity = sum(&[common_identity, snapshot.retained, snapshot.identity_peak])?;
        // Projection first retains borrowed row-reference vectors; owned clones
        // move into ContextSnapshot. Coverage holds the same snapshot through Arc.
        let projected_identity = sum(&[
            common_identity,
            snapshot.visibility,
            snapshot.retained,
            snapshot.identity_peak,
            coverage.retained,
            coverage.qualification_peak,
            slots::<&FactReference>(limits.graph.max_rows)?,
        ])?;
        let rejection_digest = agents::MAX_CURATION_ROWS
            .checked_mul(count(&hash)?)?
            .checked_add(count(&("compiler-rejections-v1", Vec::<String>::new()))?)?;
        let registration_digest = sum(&[
            string_wire(limits.scope.max_source_registry_bytes.get())?,
            limits
                .scope
                .max_sources
                .get()
                .checked_mul(count(&ScopedRepo {
                    repo_key: String::new(),
                    registered_source_id: Some(String::new()),
                    config_hash: hash.clone(),
                    layer_hints: None,
                })?)?,
            count(&("source-registration-basis-v1", Vec::<ScopedRepo>::new()))?,
        ])?;
        let finished_basis = sum(&[
            retained_session_bytes,
            rejected,
            scope.result_bytes,
            scope.result_bytes,
            coverage.stored,
            rejection_digest.max(registration_digest),
        ])?;
        let acquisition_peak_bytes = [
            before_scope,
            bounded_read,
            capability_read_peak_bytes,
            after_scope,
            comparisons,
            revision_recheck,
            raw_identity,
            projected_identity,
            finished_basis,
            sum(&[retained_session_bytes, basis_digest_bytes])?,
        ]
        .into_iter()
        .max()?;
        Some(Self {
            acquisition_peak_bytes,
            retained_session_bytes,
            basis_digest_bytes,
            #[cfg(test)]
            capability_selection_bytes,
            #[cfg(test)]
            capability_read_peak_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> WorkerLimits {
        crate::mcp_reads::worker::tests::limits()
    }
    #[test]
    fn session_roles_cover_retained_fixture_payload_and_checked_freshness() {
        let limits = limits();
        let reservation = SessionReservation::for_limits(limits).unwrap();
        let raw = logical::graph(limits.graph).unwrap();
        assert!(
            reservation.retained_session_bytes
                >= raw + logical::snapshot(limits.graph).unwrap().retained
        );
        assert!(reservation.acquisition_peak_bytes >= reservation.retained_session_bytes);
        assert!(freshness_snapshot_peak_bytes(limits.graph).unwrap() >= raw);
        let mut overflow = limits;
        overflow.graph.max_bytes = usize::MAX;
        assert!(SessionReservation::for_limits(overflow).is_none());
        assert!(freshness_snapshot_peak_bytes(overflow.graph).is_none());
        assert!(request_ids_bytes(usize::MAX, 1).is_none());
    }
    #[test]
    fn all_retained_and_acquisition_inputs_are_monotone() {
        let initial = limits();
        let base = SessionReservation::for_limits(initial).unwrap();
        let fields: &[fn(&mut WorkerLimits)] = &[
            |p| p.graph.max_bytes += 1,
            |p| p.graph.max_rows += 1,
            |p| p.graph.max_values += 1,
            |p| p.coverage.max_bytes += 1,
            |p| p.coverage.max_records += 1,
            |p| {
                p.scope.max_sources =
                    std::num::NonZeroUsize::new(p.scope.max_sources.get() + 1).unwrap()
            },
            |p| {
                p.scope.max_source_registry_bytes =
                    std::num::NonZeroUsize::new(p.scope.max_source_registry_bytes.get() + 1)
                        .unwrap()
            },
            |p| {
                p.scope.max_event_registry_bytes =
                    std::num::NonZeroUsize::new(p.scope.max_event_registry_bytes.get() + 1).unwrap()
            },
            |p| {
                p.scope.max_directory_entries =
                    std::num::NonZeroUsize::new(p.scope.max_directory_entries.get() + 1).unwrap()
            },
            |p| {
                p.scope.max_metadata_bytes =
                    std::num::NonZeroUsize::new(p.scope.max_metadata_bytes.get() + 1).unwrap()
            },
            |p| {
                p.scope.max_corpus_bytes =
                    std::num::NonZeroU64::new(p.scope.max_corpus_bytes.get() + 1).unwrap()
            },
        ];
        for edit in fields {
            let mut bigger = initial;
            edit(&mut bigger);
            let b = SessionReservation::for_limits(bigger).unwrap();
            assert!(b.retained_session_bytes >= base.retained_session_bytes);
            assert!(b.acquisition_peak_bytes >= base.acquisition_peak_bytes);
        }
        // Artifact byte work and JSON depth/native stack are explicitly not retained payload roles.
        let (ids, scratch) = request_ids_bytes(2, 32).unwrap();
        assert!(ids >= 2 * 34 && scratch >= 34);
    }
    #[test]
    fn concrete_acquired_session_serialized_payload_fits_retained_currency() {
        // AC-0510: acquired revision metadata and digest owners fit retained session currency.
        use core_graph::GraphStore;
        use std::sync::Mutex;
        let limits = limits();
        let mut graph = core_graph::SqliteGraphStore::open_in_memory().unwrap();
        let node = core_graph::Node {
            id: "fixture".into(),
            label: "Module".into(),
            props: serde_json::json!({"name":"fixture payload"}),
        };
        graph.put_node(&node).unwrap();
        let graph = Mutex::new(graph);
        let decisions = Mutex::new(agents::DecisionLog::open(":memory:").unwrap());
        let session = ReadSession::acquire(
            &graph,
            &decisions,
            || {
                Ok(vec![ScopedRepo {
                    repo_key: "fixture/repo".into(),
                    registered_source_id: Some("source".into()),
                    config_hash: "config".into(),
                    layer_hints: None,
                }])
            },
            limits.graph,
            limits.coverage,
            spec::ExportMode::VerifiedOnly,
        )
        .unwrap();
        let bound = SessionReservation::for_limits(limits).unwrap();
        let actual = logical::count(&(
            session.basis(),
            &session.base.revision,
            &session.base.graph.0,
            &session.base.graph.1,
            &session.visible_facts,
            &session.rejected_hashes,
        ))
        .unwrap();
        // Independently materialize the production tuple, hex hash and returned
        // basis ID. All three owners coexist during digest's final formatting.
        let wire = serde_json::to_vec(&("mcp-read-basis-v1", session.basis())).unwrap();
        let hash = core_prov::content_hash(&wire);
        let basis_id = session.basis_id().unwrap();
        assert_eq!(basis_id, format!("mcp-read-basis-v1:{hash}"));
        let digest_owners = sum(&[
            std::mem::size_of_val(&wire),
            wire.len(),
            std::mem::size_of_val(&hash),
            hash.len(),
            std::mem::size_of_val(&basis_id),
            basis_id.len(),
        ])
        .unwrap();
        assert!(bound.basis_digest_bytes >= digest_owners);
        assert!(bound.retained_session_bytes >= actual);
        assert!(bound.acquisition_peak_bytes >= actual);
    }
}
