//! Source-derived serialized bound. Repeated byte roles describe actual output
//! ownership, not a native allocation multiplier or a process RSS estimate.
use super::bounded::{
    SecurityLimits, SecurityProjectionSize, derive_bounded, security_projection_size,
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};

fn evidence_template() -> EvidenceRef {
    EvidenceRef {
        repo: String::new(),
        path: String::new(),
        byte_start: 0,
        byte_end: 0,
        commit_sha: String::new(),
    }
}

/// One endpoint and one grant row that each yield one finding with exactly
/// one evidence entry, one scope and (for the grant) one action.
fn template_rows() -> Option<(Node, Edge)> {
    let evidence = evidence_template();
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
    Some((endpoint, grant))
}

fn templates() -> Option<(usize, usize)> {
    let evidence = evidence_template();
    let (endpoint, grant) = template_rows()?;
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

/// JSON value occurrences in the same envelope convention as graph reads:
/// every scalar and container counts, object keys do not.
pub(crate) fn json_value_count(value: &serde_json::Value) -> usize {
    1 + match value {
        serde_json::Value::Array(items) => items.iter().map(json_value_count).sum(),
        serde_json::Value::Object(fields) => fields.values().map(json_value_count).sum(),
        _ => 0,
    }
}

/// Checked upper bound on JSON value occurrences in the complete security
/// projection, as finding nodes including their envelope. Each finding has a
/// fixed schema (one fallback scope included). Its variable actions, scopes
/// and evidence are drawn from distinct values of the one source row that
/// produced it: each action or scope is a distinct source string, and each
/// evidence entry needs at least as many source values (object or positional
/// array of the five required fields) as its output object has. Findings never
/// share a source row, so the variable part is bounded by the admitted source
/// values. Per-item output counts are measured through the real projection.
pub fn security_projection_value_bound(graph_rows: usize, graph_values: usize) -> Option<usize> {
    let (endpoint, grant) = template_rows()?;
    let unlimited = SecurityLimits {
        max_rows: usize::MAX,
        max_output_bytes: usize::MAX,
    };
    let values = |nodes: &[Node], edges: &[Edge]| -> Option<usize> {
        let findings = derive_bounded(nodes, edges, unlimited).ok()?;
        let [finding] = findings.as_slice() else {
            return None;
        };
        Some(json_value_count(&serde_json::to_value(finding).ok()?))
    };
    let with = |mut props: serde_json::Value, key: &str, extra: serde_json::Value| {
        props[key].as_array_mut()?.push(extra);
        Some(props)
    };
    let evidence = serde_json::to_value(evidence_template()).ok()?;
    let mut endpoint2 = endpoint.clone();
    endpoint2.props["prov"] = with(
        crate::stored_prov(&endpoint.props).into_owned(),
        "evidence",
        evidence.clone(),
    )?;
    let mut grant2 = grant.clone();
    grant2.props = with(grant.props.clone(), "actions", "*2".into())?;
    let mut grant3 = grant.clone();
    grant3.props = with(grant.props.clone(), "resource_scopes", "2".into())?;
    let one_endpoint = values(std::slice::from_ref(&endpoint), &[])?;
    let one_grant = values(&[], std::slice::from_ref(&grant))?;
    let per_evidence = values(&[endpoint2], &[])?.checked_sub(one_endpoint)?;
    let per_action = values(&[], &[grant2])?.checked_sub(one_grant)?;
    let per_scope = values(&[], &[grant3])?.checked_sub(one_grant)?;
    // The smallest accepted source evidence is the positional five-field array.
    let source_evidence = json_value_count(&serde_json::json!(["", "", 0, 0, ""]));
    if per_evidence > source_evidence || per_action > 1 || per_scope > 1 {
        return None;
    }
    // Endpoint keeps its fixed name scope; the grant keeps one scope as the
    // fallback allowance. Each template's single evidence (and action) varies.
    let endpoint_fixed = one_endpoint.checked_sub(per_evidence)?;
    let grant_fixed = one_grant
        .checked_sub(per_evidence)?
        .checked_sub(per_action)?;
    graph_rows
        .checked_mul(endpoint_fixed.max(grant_fixed))?
        .checked_add(graph_values)
}
