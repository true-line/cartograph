use super::*;
use core_graph::{SnapshotReadLimits, StoreRevision};
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn session() -> Value {
    json!({"session_id":{"type":"string"},"session_basis_id":{"type":"string","description":"Exact mcp-scoped-session-v1 identity from capabilities, never a query/full-graph ID."}})
}
pub(super) fn fact() -> Value {
    json!({"oneOf":[object(json!({"kind":{"const":"node"},"id":{"type":"string"}}),&["kind","id"]),object(json!({"kind":{"const":"edge"},"source":{"type":"string"},"label":{"type":"string"},"destination":{"type":"string"}}),&["kind","source","label","destination"])]})
}
fn query() -> Value {
    json!({"oneOf":[object(json!({"by":{"const":"identifier"},"id":{"type":"string"}}),&["by","id"]),object(json!({"by":{"const":"name"},"name":{"type":"string"}}),&["by","name"]),object(json!({"by":{"const":"text"},"text":{"type":"string"}}),&["by","text"])]})
}
fn target() -> Value {
    let source = object(
        json!({"repo":{"type":"string"},"path":{"type":"string"},"byte_start":{"type":"integer","minimum":0},"byte_end":{"type":"integer","minimum":1},"commit_sha":{"type":"string"}}),
        &["repo", "path", "byte_start", "byte_end", "commit_sha"],
    );
    let file = object(
        json!({"source_id":{"type":"string"},"capture_id":{"type":"string"},"path":{"type":"string"},"digest":{"type":"string"},"byte_len":{"type":"integer","minimum":0}}),
        &["source_id", "capture_id", "path", "digest", "byte_len"],
    );
    let captured = object(
        json!({"file":file,"byte_start":{"type":"integer","minimum":0},"byte_end":{"type":"integer","minimum":1}}),
        &["file", "byte_start", "byte_end"],
    );
    let revision = object(
        json!({"graph_instance":{"type":"string"},"generation":{"type":"integer","minimum":0}}),
        &["graph_instance", "generation"],
    );
    // Scoped target uses TaskFactKey's tagged form, which is the same node/edge
    // shape as the strict selection schema. Decode also closes EvidenceRef.
    object(
        json!({"schema_version":{"const":1},"target_id":{"type":"string"},"basis_id":{"type":"string"},"revision":revision,"fact":fact(),"fact_digest":{"type":"string"},"registered_source_id":{"type":"string"},"receipt_id":{"type":"string"},"receipt_inventory_index":{"type":"integer","minimum":0},"role":{"type":"string"},"index":{"type":"integer","minimum":0},"source":source,"captured":captured}),
        &[
            "schema_version",
            "target_id",
            "basis_id",
            "revision",
            "fact",
            "fact_digest",
            "registered_source_id",
            "receipt_id",
            "receipt_inventory_index",
            "role",
            "index",
            "source",
            "captured",
        ],
    )
}
pub(in crate::mcp_reads) fn descriptor(tool: ReadTool, graph: SnapshotReadLimits) -> Value {
    let (description, schema) = match tool {
        ReadTool::Capabilities => (
            "Describe revision-bound scoped retrieval and its explicit limitations.",
            object(json!({}), &[]),
        ),
        ReadTool::Artifacts => (
            "List supported closed partitions and complete-only artifacts unavailable in this scoped session.",
            object(session(), &["session_id", "session_basis_id"]),
        ),
        ReadTool::Discover => {
            let cursor = object(
                json!({"query_basis_id":{"type":"string"},"after_id":{"type":"string","description":"Caller-selected exclusive lower key; exhaustion covers only its tail, never global absence."}}),
                &["query_basis_id", "after_id"],
            );
            let request = object(
                json!({"query":query(),"labels":{"type":"array","items":{"type":"string"}},"max_scan_rows":{"type":"integer","minimum":1,"maximum":graph.max_rows},"max_candidates":{"type":"integer","minimum":1,"maximum":context_hub::discovery::MAX_CANDIDATES},"max_bytes":{"type":"integer","minimum":1,"description":"Maximum entire MCP response bytes including protocol envelope."},"cursor":{"anyOf":[cursor,{"type":"null"}]}}),
                &[
                    "query",
                    "labels",
                    "max_scan_rows",
                    "max_candidates",
                    "max_bytes",
                ],
            );
            let mut props = session();
            props["request"] = request;
            (
                "Scan one bounded node-key window using discovery-v1 matching. Zero matches with More is not absence. Degrees and remaining totals are unknown.",
                object(props, &["session_id", "session_basis_id", "request"]),
            )
        }
        ReadTool::ReadFact => {
            let mut props = session();
            props["fact"] = fact();
            props["max_bytes"] = json!({"type":"integer","minimum":1});
            (
                "Read one exact scoped fact; edge projection requires a bounded complete endpoint closure. No global absence claim.",
                object(
                    props,
                    &["session_id", "session_basis_id", "fact", "max_bytes"],
                ),
            )
        }
        ReadTool::Evidence => {
            let mut describe = session();
            describe["mode"] = json!({"const":"describe"});
            describe["kind"] =
                json!({"enum":["retained_primary_source","retained_observed_input"]});
            describe["fact"] = fact();
            describe["ordinal"] = json!({"type":"integer","minimum":0,"maximum":adapters_lang_ts::captured::MAX_RECEIPT_RANGES-1});
            describe["max_bytes"] = json!({"type":"integer","minimum":1});
            let mut read = session();
            read["mode"] = json!({"const":"read"});
            read["kind"] = json!({"enum":["retained_primary_source","retained_observed_input"]});
            read["target"] = target();
            read["max_bytes"] = json!({"type":"integer","minimum":1});
            (
                "Describe or read an exact retained span using a scoped target: `retained_primary_source` is the fact's declaring source, `retained_observed_input` the captured state, export or trace that observed it (never a declaration); read text has secrets redacted (see `redaction`); no full-input-closure, checkout fallback or omission inference.",
                json!({"oneOf":[object(describe,&["session_id","session_basis_id","mode","kind","fact","ordinal","max_bytes"]),object(read,&["session_id","session_basis_id","mode","kind","target","max_bytes"])]}),
            )
        }
        ReadTool::ReadArtifact => {
            let mut props = session();
            props["kind"] = json!({"enum":["data-model","rule-evidence","rule-evidence-index"]});
            props["root"] = json!({"type":"string","minLength":1});
            props["max_bytes"] = json!({"type":"integer","minimum":1});
            (
                "Compile a bounded whole DataEntity component or explicit BusinessRule observation closure. A Gap-tier DataEntity entry root returns unavailable; this does not establish missing data. A leaf may reach a large component and return a limit; never full-system evidence.",
                object(
                    props,
                    &[
                        "session_id",
                        "session_basis_id",
                        "kind",
                        "root",
                        "max_bytes",
                    ],
                ),
            )
        }
        ReadTool::Query => {
            let cursor = object(
                json!({"snapshot_id":{"type":"string"},"selection_id":{"type":"string"},"offset":{"type":"integer","minimum":0}}),
                &["snapshot_id", "selection_id", "offset"],
            );
            let mut props = session();
            props["anchor"] = json!({"type":"string","description":"Exact node identity, for example a discovery candidate's id."});
            props["hops"] =
                json!({"type":"integer","minimum":1,"maximum":context_hub::MAX_NEIGHBORHOOD_HOPS});
            props["kind"] = json!({"enum":["node","edge",null]});
            props["labels"] = json!({"type":"array","items":{"type":"string"},"description":"Exact labels; empty selects every label."});
            props["max_facts"] =
                json!({"type":"integer","minimum":1,"maximum":context_hub::MAX_FACTS});
            props["max_bytes"] = json!({"type":"integer","minimum":1,"description":"Maximum entire MCP response bytes including protocol envelope."});
            props["cursor"] = json!({"anyOf":[cursor,{"type":"null"}]});
            (
                "Read one bounded page of a node's induced neighborhood (the visible nodes within hops and the edges among them) at this revision. Acquisition beyond the window limits returns a limit, never a partial neighborhood; no global absence claim.",
                object(
                    props,
                    &[
                        "session_id",
                        "session_basis_id",
                        "anchor",
                        "hops",
                        "max_facts",
                        "max_bytes",
                    ],
                ),
            )
        }
    };
    json!({"name":tool.name(),"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}})
}
pub(super) fn catalog() -> Value {
    let entries: Vec<_> = crate::mcp_reads::artifacts::readable_catalog().into_iter().map(|metadata| {
        let scope = match metadata.id {
            "data-model" => Some(("DataEntity", "whole_connected_entity_component")),
            "rule-evidence" | "rule-evidence-index" => Some(("BusinessRule", "explicit_rule_observation")),
            _ => None,
        };
        match scope {
            Some((label, scope)) => json!({"metadata":metadata,"status":"partition_supported","required_basis":"closed_partition","required_root_label":label,"scope":scope,"closure_may_exceed_limits":true}),
            None => json!({"metadata":metadata,"status":"unavailable","required_basis":"complete","reason":if metadata.id=="flow-dossiers" {"explicit_seed_tracer_not_available"} else {"complete_basis_required"}}),
        }
    }).collect();
    json!({"mode":"scoped","complete_system":false,"artifacts":entries})
}
/// A scoped session reads bounded windows and never the complete graph, so
/// it computes no curated projection (SPEC-12 slice 4, AC-0654) and offers
/// no curated read. The catalogue lists `curated` and `curated-json` as
/// unavailable for the same reason.
const H3_CURATED_AVAILABLE: bool = false;
const H3_CURATED_UNAVAILABLE_REASON: &str = "complete_basis_required";
pub(super) fn capabilities(
    session_id: &str,
    basis: &str,
    revision: &StoreRevision,
    coverage_id: &str,
    graph: SnapshotReadLimits,
    limits: DispatchLimits,
    evidence: EvidenceWorkLimits,
) -> Value {
    json!({"mode":"scoped","session_id":session_id,"session_basis_id":basis,"revision":revision,"coverage_id":coverage_id,"matching_policy":context_hub::discovery::DISCOVERY_POLICY,"implemented_tools":TOOLS.iter().map(|t|t.name()).collect::<Vec<_>>(),"structured_content_required":true,"complete_system":false,"global_absence_available":false,"evidence_absence_reason_available":false,"forgotten_receipt_classification":{"available":true,"scope":"exact_current_basis_binding_receipt_id","partial":true,"omitted_available":false,"unknown_is_forgotten":false},"receipt_omission_classification":{"available":true,"scope":"exact_current_fact_same_transaction_omission","partial":true,"kinds":["declarations","rules","store_capacity"],"unknown_is_omitted":false,"not_omitted_is_receipt_available":false},"h3_curated_available":H3_CURATED_AVAILABLE,"h3_curated_unavailable_reason":H3_CURATED_UNAVAILABLE_REASON,"artifact_content_available":true,"artifact_availability":"enumerated_by_cartograph_artifacts","relations_access":{"via":ReadTool::Query.name(),"query_tool_available":TOOLS.contains(&ReadTool::Query),"scopes":["neighborhood"],"max_neighborhood_hops":context_hub::MAX_NEIGHBORHOOD_HOPS,"max_facts":context_hub::MAX_FACTS,"acquisition":"bounded_window_limit_not_partial"},"unknown_degrees":true,"remaining_row_totals":"unknown","cursor_lower_bound":"caller_selected_exclusive_primary_key","exhaustion":"raw_tail_only_not_global_absence","worker_reservation_model":super::super::worker::SCOPED_WORKER_MODEL,"limits":{"max_response_bytes":limits.max_response_bytes.get(),"max_request_ids":limits.max_request_ids.get(),"window":{"max_rows":graph.max_rows,"hard_max_rows":SnapshotReadLimits::MAX_ROWS,"max_bytes":graph.max_bytes,"max_depth":graph.max_depth,"max_values":graph.max_values},"evidence":{"max_file_validation_bytes":evidence.max_file_validation_bytes.get(),"max_span_bytes":evidence.max_span_bytes.get()}}})
}
