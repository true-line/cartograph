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

/// Additional selection observation, including conversion while the storage
/// result still owns its graph and selected rows. Schema checks are sequential.
/// `selection_wire` must bound the complete storage and host selection DTOs,
/// including bounded association/omission strings, not only the requested keys.
fn selection_peak(selected: usize, selection_wire: usize) -> Option<usize> {
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
        256,
        256,
        72,
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
fn guard_owners(
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
        app_path_bytes, // canonical path returned by PrivateStorage::verify
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

/// One evidence inventory and the temporary receipt/provenance work needed to
/// construct it. The fact-matching adapter supplies its independently checked
/// additional peak for the SAME separately admitted full-fact wire bound.
fn option_owners(fact_wire: usize, fact_match_peak: usize) -> Option<(usize, usize)> {
    use super::context::{EvidenceOption, EvidenceOptions};
    use adapters_lang_ts::captured::{MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES};
    use core_prov::{EvidenceRef, Provenance};
    let receipt = crate::mcp_reads::reservation::receipt::workspace_bytes()?;
    // A legacy provenance list is decoded before validation. Its element count
    // follows admitted fact bytes, not the captured receipt's range-count cap.
    let legacy = sum(&[
        fact_wire,
        slots::<Provenance>(1)?,
        slots::<EvidenceRef>(fact_wire.checked_add(1)?)?,
    ])?;
    let legacy_options = sum(&[
        fact_wire,
        slots::<EvidenceOption>(fact_wire.checked_add(1)?)?,
    ])?;
    let captured_options = sum(&[
        MAX_RECEIPT_BYTES,
        MAX_RECEIPT_BYTES, // expanded v3 shared range identities
        slots::<EvidenceOption>(MAX_RECEIPT_RANGES)?,
    ])?;
    let retained = sum(&[
        slots::<EvidenceOptions>(1)?,
        fact_wire, // returned TaskFactKey cloned from the selected fact
        legacy_options.max(captured_options),
    ])?;
    let legacy_peak = sum(&[
        json_bytes(fact_wire)?, // cloned provenance Value during typed decode
        json_bytes(fact_wire)?, // internally tagged ProducingMethod buffer
        legacy,
        legacy_options, // collection may keep input/output vector allocations
    ])?;
    // stored_receipt retains two admitted SQL identity strings while decoding
    // and round-tripping the receipt. The raw receipt is included in peak.
    let stored_peak = sum(&[
        receipt.peak,
        256,
        256,
        slots::<Option<String>>(3)?,
        fact_wire, // graph_fact clone held by task_receipt's caller
    ])?;
    let captured_peak = stored_peak.max(sum(&[
        receipt.retained,
        fact_match_peak.max(captured_options),
    ])?);
    Some((retained, sum(&[retained, legacy_peak.max(captured_peak)])?))
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
    use context_hub::logical::string_wire;
    let selected = policy
        .limits()
        .selected_facts
        .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
    // Bound storage-selected identities independently of the accepted ledger:
    // an edge key may contain two 8192-byte IDs and one 256-byte label. Its
    // association adds repo/receipt256 each and two72-byte digests; omission
    // adds repo256. Escaping occurs before candidate-ledger refusal.
    let row_payload = sum(&[
        8192, 8192, 256, // selected edge key
        8192, 8192, 256, // storage SourceBinding repeats its key
        256, 256, 72, 72, 256,
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
    let selection_wire = selected.checked_mul(sum(&[
        string_wire(row_payload)?,
        logical::count(&selected_envelope)?,
        1, // array separator
    ])?)?;
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
    let fact_match_peak =
        adapters_lang_ts::captured::fact_match_workspace_bytes(input.graph.max_bytes)?;
    let (one_option, option_peak) = option_owners(input.graph.max_bytes, fact_match_peak)?;
    let options = agents::investigation::MAX_QUERY_FACTS.checked_mul(one_option)?;
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
