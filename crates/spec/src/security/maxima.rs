//! Source-derived serialized bound. Repeated byte roles describe actual output
//! ownership, not a native allocation multiplier or a process RSS estimate.
use super::bounded::{SecurityProjectionSize, security_projection_size};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};

fn templates() -> Option<(usize, usize)> {
    let evidence = EvidenceRef {
        repo: String::new(),
        path: String::new(),
        byte_start: 0,
        byte_end: 0,
        commit_sha: String::new(),
    };
    // Count fixed output schemas through the real projection. Empty variable
    // strings expose the generated labels, title delimiters, IDs and rule data.
    let support = serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence.clone()],
            "",
            b"",
        )
        .ok()?,
    )
    .ok()?;
    let endpoint = Node {
        id: String::new(),
        label: "Endpoint".into(),
        props: serde_json::json!({"method":"","path":"","authenticated":false,"prov":support}),
    };
    let grant = Edge {
        src: String::new(),
        dst: String::new(),
        label: "GRANTS".into(),
        props: serde_json::json!({"actions":["*"],"resource_scopes":[""],"prov":support}),
    };
    let endpoint_bytes = security_projection_size(&[endpoint], &[])?
        .bytes
        .checked_sub(2)?;
    let grant_bytes = security_projection_size(&[], &[grant])?
        .bytes
        .checked_sub(2)?;
    // Evidence can be represented positionally in source serde. Its object
    // schema therefore needs its own per-source-value allowance in the bound.
    let evidence_bytes = serde_json::to_vec(&evidence).ok()?.len();
    Some((
        endpoint_bytes.max(grant_bytes).checked_add(1)?,
        evidence_bytes,
    ))
}

/// Checked upper bound on complete compact security JSON from admitted source
/// graph canonical bytes, combined node/edge rows, and JSON values. At most one
/// finding is produced per row. This bounds serialized payload only, not RSS.
/// Source value counts include graph/fact envelopes as in SnapshotReadLimits.
pub fn security_projection_bound(
    graph_bytes: usize,
    graph_rows: usize,
    graph_values: usize,
) -> Option<SecurityProjectionSize> {
    let (template, evidence_schema) = templates()?;
    let roles = [
        graph_bytes, // title source fields
        graph_bytes, // subject ID source fields
        graph_bytes, // resource scope source fields
        graph_bytes, // action source fields
        graph_bytes,
    ]; // provenance evidence source fields
    let bytes = roles
        .into_iter()
        .try_fold(2_usize, usize::checked_add)?
        .checked_add(graph_rows.checked_mul(template)?)? // fixed finding schema + comma
        .checked_add(graph_values.checked_mul(2)?)? // scopes.join(", ") delimiters
        .checked_add(graph_values.checked_mul(evidence_schema)?)?; // positional evidence -> object schema
    Some(SecurityProjectionSize {
        rows: graph_rows,
        bytes,
    })
}
