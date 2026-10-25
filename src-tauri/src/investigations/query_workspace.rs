//! Host query preparation ownership. Logical payload roles, not allocator/RSS.
//!
//! The frozen graph, base ledger and request are borrowed. Selection reads use
//! the store's DEFAULT complete-graph limits even for a small projected page.
use crate::{primary_source::RetentionGuard, sources::RegisteredSource};
use agents::{TaskFactSelection, investigation::InvestigationReceiptReference};
use context_hub::logical::{self, json_bytes, map, set, slots, sum};
use core_graph::{
    SnapshotReadLimits,
    source::{FactKey, FactSourceSelection},
};

// Producer contracts are exercised at and beyond these boundaries below.
const SOURCE_ID_BYTES: usize = 8192;
const SOURCE_LABEL_BYTES: usize = 256;
const SOURCE_ASSOCIATION_BYTES: usize = 256;
const FACT_DIGEST_BYTES: usize = 72;

/// Complete storage/host selection wire before accepted-ledger comparison.
pub(super) fn selection_wire(selected: usize) -> Option<usize> {
    use context_hub::logical::string_wire;
    // Bound storage-selected identities independently of the accepted ledger:
    // an edge key may contain two 8192-byte IDs and one 256-byte label. Its
    // association adds repo/receipt256 each and two72-byte digests; omission
    // adds repo256. Escaping occurs before candidate-ledger refusal.
    let row_payload = sum(&[
        SOURCE_ID_BYTES,
        SOURCE_ID_BYTES,
        SOURCE_LABEL_BYTES, // selected edge key
        SOURCE_ID_BYTES,
        SOURCE_ID_BYTES,
        SOURCE_LABEL_BYTES, // repeated binding key
        SOURCE_ASSOCIATION_BYTES,
        SOURCE_ASSOCIATION_BYTES,
        FACT_DIGEST_BYTES,
        FACT_DIGEST_BYTES,
        SOURCE_ASSOCIATION_BYTES,
    ])?;
    let selected_envelope = agents::TaskFactSelection {
        fact: agents::TaskFactKey::Edge {
            source: String::new(),
            label: String::new(),
            destination: String::new(),
        },
        fact_digest: String::new(),
        binding: Some(agents::TaskSourceAssociation {
            repo_key: String::new(),
            receipt_id: String::new(),
            emitted_fact_digest: String::new(),
        }),
    };
    selected.checked_mul(sum(&[
        string_wire(row_payload)?,
        logical::count(&selected_envelope)?,
        1, // array separator
    ])?)
}

/// Additional selection observation, including conversion while the storage
/// result still owns its graph and selected rows. Schema checks are sequential.
/// `selection_wire` must bound the complete storage and host selection DTOs,
/// including bounded association/omission strings, not only the requested keys.
pub(super) fn selection_peak(selected: usize, selection_wire: usize) -> Option<usize> {
    let graph = SnapshotReadLimits::default();
    let snapshot = logical::snapshot(graph)?;
    let keys = sum(&[selection_wire, set::<FactKey>(selected)?])?;
    let result = sum(&[
        json_bytes(selection_wire)?,
        map::<FactKey, TaskFactSelection>(selected)?,
    ])?;
    // A complete selected fact's properties can consume the entire graph byte
    // budget. The snapshot identity currency covers its Value/canonicalization
    // and serialized digest buffers without assuming the page-byte cap.
    let digest = snapshot.identity_peak;
    let storage_rows = sum(&[
        json_bytes(selection_wire)?,
        slots::<FactSourceSelection>(selected)?,
        // Association and omission SQL cells, bounded before UTF-8 decode.
        // These observations are sequential; retain one current cell tuple.
        SOURCE_ASSOCIATION_BYTES,
        SOURCE_ASSOCIATION_BYTES,
        FACT_DIGEST_BYTES,
        slots::<Option<Vec<u8>>>(3)?,
        slots::<String>(3)?,
        32, // StoreRevision instance identity
        slots::<core_graph::StoreRevision>(1)?,
    ])?;
    sum(&[
        keys,
        logical::graph(graph)?,
        graph.max_bytes, // raw SQL fields during bounded graph decoding
        storage_rows,
        result,
        digest.max(crate::mcp_reads::reservation::schema::workspace_bytes()?),
    ])
}

/// Retention guards and coherent source registrations. `app_path_bytes` is a
/// separately admitted host path owner, never inferred from client input.
pub(super) fn guard_owners(
    selected: usize,
    selection_wire: usize,
    app_path_bytes: usize,
) -> Option<(usize, usize)> {
    let source_id = "src_".len().checked_add(32)?;
    // Use the whole selected registry observation for each retained source;
    // this intentionally dominates its smaller final RegisteredSource value.
    let sources = sum(&[
        selected.checked_mul(crate::sources::selected_source_peak_bytes()?)?,
        selection_wire, // BTreeMap repository keys
        map::<String, RegisteredSource>(selected)?,
    ])?;
    let guards = sum(&[
        selected.checked_mul(sum(&[source_id, source_id, app_path_bytes])?)?,
        map::<String, RetentionGuard>(selected)?,
    ])?;
    let references = sum(&[
        selection_wire, // repository and receipt identities copied from bindings
        selected.checked_mul(source_id)?,
        slots::<InvestigationReceiptReference>(selected)?,
    ])?;
    let retained = sum(&[sources, guards, references])?;
    let acquisition = sum(&[
        retained,
        app_path_bytes, // conservative path scratch; Unix revalidation borrows components
        slots::<std::path::PathBuf>(1)?,
        selected.checked_mul(source_id)?,
        set::<String>(selected)?,
        source_id, // current ID clone before insertion
        source_id, // relative lockfile name; the directory handle is borrowed
        ".lock".len(),
        slots::<String>(1)?,
        "Retained source is busy or locking is unavailable; retry after the active operation finishes.".len(),
    ])?;
    Some((retained, acquisition))
}

/// Retained inventories for disjoint selected facts, plus one current decode.
/// Their legacy payload/value totals share the frozen graph's admitted bounds;
/// captured receipts have independent per-option limits and can mix with legacy.
fn option_owners(fact_wire: usize, fact_values: usize, options: usize) -> Option<(usize, usize)> {
    use super::context::{EvidenceOption, EvidenceOptions};
    use adapters_lang_ts::captured::{MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES};
    use core_prov::{EvidenceRef, Provenance};
    let values = fact_values.min(fact_wire.checked_add(1)?);
    let receipt = crate::mcp_reads::reservation::receipt::workspace_bytes()?;
    let captured_options = sum(&[
        MAX_RECEIPT_BYTES,
        MAX_RECEIPT_BYTES, // expanded v3 shared range identities
        slots::<EvidenceOption>(MAX_RECEIPT_RANGES)?,
    ])?;
    let retained = sum(&[
        slots::<EvidenceOptions>(options)?,
        fact_wire, // all returned fact keys together
        fact_wire, // all legacy evidence strings together
        slots::<EvidenceOption>(values)?,
        options.checked_mul(captured_options)?,
    ])?;
    // Before validation one legacy provenance can consume the whole graph
    // allowance. Its input vector overlaps the output covered by retained.
    let legacy = sum(&[
        slots::<Provenance>(1)?,
        fact_wire,
        slots::<EvidenceRef>(values)?,
    ])?;
    let legacy_peak = sum(&[
        logical::json(fact_wire, values)?, // cloned provenance Value
        logical::json(fact_wire, values)?, // tagged ProducingMethod buffer
        legacy,
    ])?;
    let stored_peak = sum(&[
        receipt.peak,
        SOURCE_ASSOCIATION_BYTES,
        SOURCE_ASSOCIATION_BYTES,
        slots::<Option<String>>(3)?,
        fact_wire, // graph_fact clone held by task_receipt's caller
    ])?;
    let captured_peak = stored_peak.max(sum(&[
        receipt.retained,
        adapters_lang_ts::captured::fact_match_workspace(fact_wire, values)?,
    ])?);
    Some((retained, legacy_peak.max(captured_peak)))
}

/// Candidate ledger owners before its final manifest-size validation. Successful
/// response bytes alone cannot bound the selected fact and citation metadata.
fn ledger_owners(
    policy: &agents::investigation::InvestigationPolicy,
    ledger_wire: usize,
    request_wire: usize,
    selected: usize,
    selection_wire: usize,
) -> Option<(usize, usize)> {
    use agents::{TaskFactKey, investigation::*};
    use context_hub::logical::{count, string_wire};
    let citation = InvestigationCitation {
        citation_id: String::new(),
        fact: TaskFactKey::Edge {
            source: String::new(),
            label: String::new(),
            destination: String::new(),
        },
        fact_digest: String::new(),
        source: None,
        role: None,
        index: None,
        text_hash: None,
        origin: InvestigationEvidenceOrigin::GraphMetadata,
    };
    let query = InvestigationQueryManifest {
        query: InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: Vec::new(),
            max_facts: usize::MAX,
            max_bytes: usize::MAX,
            cursor: None,
        },
        response_hash: core_prov::content_hash(&[]),
        response_bytes: usize::MAX,
        returned_facts: usize::MAX,
        total_selected: usize::MAX,
        has_more: false,
    };
    let reference = InvestigationReceiptReference {
        source_id: String::new(),
        repo_key: String::new(),
        receipt_id: String::new(),
    };
    let reference_wire = sum(&[
        selection_wire,
        selected.checked_mul(sum(&[count(&reference)?, string_wire(36)?, 1])?)?,
    ])?;
    let citations = sum(&[
        selection_wire, // complete fact/digest strings already JSON-escaped
        selected.checked_mul(sum(&[
            count(&citation)?,
            string_wire("fact-".len().checked_add(usize::MAX.to_string().len())?)?,
            1,
        ])?)?,
    ])?;
    // Count append as well as old fields. Replacements are no larger than this
    // conservative append currency; old/new vectors can coexist in assignment.
    let candidate_wire = sum(&[
        ledger_wire,
        selection_wire,
        reference_wire,
        citations,
        request_wire,
        count(&query)?,
        3,
        u64::MAX.to_string().len(),
    ])?;
    let base = sum(&[
        slots::<InvestigationInputLedger>(1)?,
        json_bytes(ledger_wire)?,
    ])?;
    let candidate = sum(&[
        slots::<InvestigationInputLedger>(1)?,
        json_bytes(candidate_wire)?,
    ])?;
    let retained = sum(&[base, candidate, MAX_QUERY_BYTES, slots::<String>(1)?])?;
    let peak = sum(&[
        retained,
        json_bytes(selection_wire)?, // replaced selected vector
        json_bytes(reference_wire)?, // cloned guard references before filtering
        ledger_validation_workspace(policy, candidate_wire)?,
        slots::<InvestigationQueryBudget>(2)?,
    ])?;
    Some((retained, peak))
}

/// Same acquisition limits as FrozenContext preparation, independent of the
/// connection's read-page limits. Public task activation remains separate.
#[cfg(unix)]
pub(crate) fn admitted_workspace(
    scope: crate::mcp_reads::scope::ScopeLimits,
    app_path_bytes: usize,
) -> Option<(usize, usize)> {
    use agents::investigation::*;
    let result = workspace(
        &INVESTIGATION_V2_POLICY,
        QueryInputs {
            graph: SnapshotReadLimits::default(),
            coverage: super::context::coverage_preparation_limits(
                MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            )
            .ok()?,
            scoped_repos: scope.max_sources.get(),
            scope_identity_bytes: scope.max_source_registry_bytes.get(),
            ledger_wire: MAX_MANIFEST_BYTES,
            // Queries are decoded from a bounded action before host preparation.
            request_wire: MAX_ACTION_BYTES,
            app_path_bytes,
        },
    )?;
    Some((result.retained, result.peak))
}

#[derive(Clone, Copy)]
pub(super) struct QueryInputs {
    pub(super) graph: SnapshotReadLimits,
    pub(super) coverage: core_graph::coverage::CoverageReadLimits,
    pub(super) scoped_repos: usize,
    pub(super) scope_identity_bytes: usize,
    pub(super) ledger_wire: usize,
    pub(super) request_wire: usize,
    pub(super) app_path_bytes: usize,
}

pub(super) struct QueryWorkspace {
    pub(super) retained: usize,
    pub(super) peak: usize,
}

pub(super) fn workspace(
    policy: &agents::investigation::InvestigationPolicy,
    input: QueryInputs,
) -> Option<QueryWorkspace> {
    use agents::investigation::ledger_validation_workspace;
    let selected = policy
        .limits()
        .selected_facts
        .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
    let selection_wire = selection_wire(selected)?;
    let page = logical::qualified_query_workspace(
        input.graph,
        input.coverage,
        input.scoped_repos,
        input.scope_identity_bytes,
        input.request_wire,
    )?; // qualified currency also dominates the legacy page
    let selection = sum(&[
        json_bytes(selection_wire)?,
        map::<FactKey, TaskFactSelection>(selected)?,
    ])?;
    let keys = sum(&[
        input.graph.max_bytes,
        selection_wire,
        set::<FactKey>(input.graph.max_rows.checked_add(selected)?)?,
    ])?;
    let (guards, guard_peak) = guard_owners(selected, selection_wire, input.app_path_bytes)?;
    // Keys are admitted before options are constructed; each page fact is
    // unique and belongs to both the admitted graph and that selected set.
    let option_count = selected
        .min(agents::investigation::MAX_QUERY_FACTS)
        .min(input.graph.max_rows);
    let (options, option_peak) =
        option_owners(input.graph.max_bytes, input.graph.max_values, option_count)?;
    let (ledger, ledger_peak) = ledger_owners(
        policy,
        input.ledger_wire,
        input.request_wire,
        selected,
        selection_wire,
    )?;
    let select = selection_peak(selected, selection_wire)?;
    let phases = [
        ledger_validation_workspace(policy, input.ledger_wire)?,
        sum(&[keys, page.peak_bytes])?,
        sum(&[keys, page.response_bytes, select])?,
        sum(&[keys, page.response_bytes, selection, guard_peak])?,
        sum(&[keys, page.response_bytes, selection, guards, select])?,
        sum(&[
            keys,
            page.response_bytes,
            selection,
            guards,
            options,
            option_peak,
        ])?,
        // Initial evidence options stay live while the replacement page is
        // queried. They are truncated only after its exact continuation exists.
        sum(&[keys, selection, guards, options, page.peak_bytes])?,
        // Candidate metadata is built while raw/page/options/selection are held;
        // ledger_peak conservatively includes the later base clone/validation.
        sum(&[
            keys,
            page.response_bytes,
            selection,
            guards,
            options,
            ledger_peak,
        ])?,
    ];
    Some(QueryWorkspace {
        retained: sum(&[
            ledger,
            guards,
            slots::<super::context::PreparedContextRead>(1)?,
        ])?,
        peak: sum(&[
            json_bytes(input.request_wire)?,
            slots::<super::context::PreparedContextRead>(1)?,
            *phases.iter().max()?,
        ])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn query_admission_uses_host_limits_and_refuses_path_overflow() {
        // AC-0327: admit host query owners before acquiring a task permit.
        use agents::investigation::*;
        let scope = crate::mcp_reads::worker::tests::limits().scope;
        let small = admitted_workspace(scope, 16).unwrap();
        let large = admitted_workspace(scope, 4096).unwrap();
        assert!(large.0 > small.0);
        assert!(large.1 >= small.1);
        assert!(admitted_workspace(scope, usize::MAX).is_none());
        // Boundary selector verifies the action-wire currency used by admission.
        let query = InvestigationQuery {
            scope: InvestigationScope::Neighborhood {
                anchor: "\u{0001}".repeat(1024),
                hops: 3,
            },
            kind: Some(InvestigationFactKind::Node),
            labels: vec!["\u{0001}".repeat(128); 16],
            max_facts: MAX_QUERY_FACTS,
            max_bytes: MAX_QUERY_BYTES,
            cursor: Some(InvestigationQueryCursor {
                snapshot_id: "x".repeat(128),
                selection_id: "x".repeat(128),
                offset: 50_000,
            }),
        };
        query.validate().unwrap();
        assert!(logical::count(&query).unwrap() <= MAX_ACTION_BYTES);
    }

    #[test]
    fn query_option_payload_growth_is_shared_across_selected_facts() {
        // AC-0327: adding options cannot multiply disjoint graph payloads.
        let retained = |wire, values, count| option_owners(wire, values, count).unwrap().0;
        let growth = |count| retained(8192, 200, count) - retained(4096, 100, count);
        assert_eq!(growth(1), growth(32));
        assert!(retained(8192, 200, 32) > retained(8192, 200, 1));
        let peak = |count| option_owners(8192, 200, count).unwrap().1;
        assert_eq!(peak(1), peak(32)); // only one inventory decodes at a time
        assert!(option_owners(usize::MAX, 1, 1).is_none());
        assert!(option_owners(4096, 100, usize::MAX).is_none());
    }

    #[test]
    fn query_selection_constants_match_producer_boundaries() {
        // AC-0327: fail if the storage producer's accepted identities grow.
        use core_graph::{GraphPatch, Node, SqliteGraphStore, source::SourceBinding};
        let store = SqliteGraphStore::open_in_memory().unwrap();
        for extra in [0, 1] {
            let node = FactKey::Node {
                id: "n".repeat(SOURCE_ID_BYTES + extra),
            };
            assert_eq!(
                store.read_source_selection_snapshot(&[node]).is_ok(),
                extra == 0
            );
            let edge = FactKey::Edge {
                source: "s".repeat(SOURCE_ID_BYTES),
                label: "e".repeat(SOURCE_LABEL_BYTES + extra),
                destination: "d".repeat(SOURCE_ID_BYTES),
            };
            assert_eq!(
                store.read_source_selection_snapshot(&[edge]).is_ok(),
                extra == 0
            );
        }
        let node = Node {
            id: "fixture".into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        };
        let digest = core_graph::source::node_digest(&node).unwrap();
        assert_eq!(digest.len(), FACT_DIGEST_BYTES);
        for (repo_extra, receipt_extra) in [(0, 0), (1, 0), (0, 1)] {
            let mut store = SqliteGraphStore::open_in_memory().unwrap();
            let binding = SourceBinding {
                fact: FactKey::from_node(&node),
                repo_key: "r".repeat(SOURCE_ASSOCIATION_BYTES + repo_extra),
                receipt_id: "c".repeat(SOURCE_ASSOCIATION_BYTES + receipt_extra),
                emitted_fact_digest: digest.clone(),
            };
            let result = store.apply_patch_with_source_bindings_if_snapshot_matches(
                &(vec![], vec![]),
                &GraphPatch {
                    upsert_nodes: vec![node.clone()],
                    ..GraphPatch::default()
                },
                &binding.repo_key,
                std::slice::from_ref(&binding),
            );
            assert_eq!(result.is_ok(), repo_extra == 0 && receipt_extra == 0);
            if let Ok(applied) = result {
                assert!(applied);
            }
        }
    }

    #[test]
    fn query_workspace_refuses_unrepresentable_admitted_owners() {
        // AC-0327: no saturation can turn an unrepresentable component into an
        // admissible worker reservation. This is not a total worker bound.
        let input = QueryInputs {
            graph: SnapshotReadLimits::default(),
            coverage: core_graph::coverage::CoverageReadLimits::default(),
            scoped_repos: 3,
            scope_identity_bytes: 1024,
            ledger_wire: 4096,
            request_wire: 1024,
            app_path_bytes: 4096,
        };
        let policy = &agents::investigation::INVESTIGATION_V1_POLICY;
        assert!(workspace(policy, input).is_some());
        let corrupt: &[fn(&mut QueryInputs)] = &[
            |i| i.graph.max_bytes = usize::MAX,
            |i| i.graph.max_rows = usize::MAX,
            |i| i.coverage.max_bytes = usize::MAX,
            |i| i.scoped_repos = usize::MAX,
            |i| i.scope_identity_bytes = usize::MAX,
            |i| i.ledger_wire = usize::MAX,
            |i| i.request_wire = usize::MAX,
            |i| i.app_path_bytes = usize::MAX,
        ];
        for edit in corrupt {
            let mut invalid = input;
            edit(&mut invalid);
            assert!(workspace(policy, invalid).is_none());
        }
    }
}
