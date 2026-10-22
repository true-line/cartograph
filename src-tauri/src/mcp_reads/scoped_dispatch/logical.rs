//! Fixed descriptors/capability/catalog DOMs can exceed a tiny response policy
//! before its final check. Count production schemas; no arbitrary byte allowance.
use super::*;
use context_hub::logical::{count, json_bytes, slots, string_wire, sum};
pub(crate) fn logical_protocol_workspace_bytes(
    limits: super::super::worker::WorkerLimits,
) -> Option<usize> {
    let descriptors: Vec<_> = TOOLS
        .iter()
        .map(|tool| schemas::descriptor(*tool, limits.graph))
        .collect();
    let list = json!({"jsonrpc":"2.0","id":null,"result":{"tools":descriptors}});
    let list_dom = json_bytes(count(&list)?)?;
    // Input schema, descriptor Vec, and outer response DOM are distinct owners
    // while serde's Value copying constructs tools/list.
    let list_peak = sum(&[list_dom, list_dom, list_dom, slots::<Value>(TOOLS.len())?])?;
    let hash = core_prov::content_hash(&[]);
    let basis = format!("mcp-scoped-session-v1:{hash}");
    let coverage = format!("coverage-context-v1:{hash}");
    let revision = core_graph::StoreRevision {
        graph_instance: "a".repeat(32),
        generation: i64::MAX as u64,
    };
    let caps = schemas::capabilities(
        "",
        &basis,
        &revision,
        &coverage,
        limits.graph,
        limits.dispatch,
        limits.evidence,
    );
    let cap_dom = json_bytes(count(&success(
        &Value::Null,
        "Scoped context capabilities.",
        caps,
    ))?)?;
    let catalog = schemas::catalog();
    let catalog_dom = json_bytes(count(&success(
        &Value::Null,
        "Scoped artifact availability.",
        json!({"session_id":"","catalog":catalog}),
    ))?)?;
    // Data DOM and success wrapper coexist; full official metadata DTOs are
    // temporary source owners while the catalogue Value is built.
    let catalogue_peak = sum(&[
        catalog_dom,
        catalog_dom,
        count(&spec::artifact_catalog())?,
        slots::<spec::ArtifactDescriptor>(spec::artifact_catalog().len())?,
    ])?;
    let mut admission = Admission::new(TOOLS, limits.dispatch.max_request_ids);
    let Action::Respond(initial)=admission.receive(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"fixture","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})) else {return None;};
    let mut closed = json_bytes(count(&initial)?)?;
    for error in [
        ReadError::StaleBasis,
        ReadError::Limit,
        ReadError::Storage,
        ReadError::ArtifactUnavailable,
        ReadError::EvidenceUnavailable,
        ReadError::EvidenceForgotten,
        ReadError::EvidenceInvalid,
        ReadError::InvalidRequest,
    ] {
        closed = closed
            .max(json_bytes(count(&tool_error(&Value::Null, error))?)?)
            .max(json_bytes(count(&error_response(None, error))?)?);
    }
    let session = json_bytes(string_wire(64)?)?; // production capability hex width
    // Current request/response IDs are composed separately by the worker model.
    sum(&[
        session,
        list_peak
            .max(sum(&[cap_dom, cap_dom])?)
            .max(catalogue_peak)
            .max(closed),
    ])
}
