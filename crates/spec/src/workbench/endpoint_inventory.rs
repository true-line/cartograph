//! Endpoint inventory (#1658, AC-0694, catalogue D4 §4.1):
//! `endpoint-inventory.md`.
//!
//! A deterministic (T0) projection of the recovered `Endpoint` facts, one
//! row each, whether or not a test references it. No model is called and
//! nothing is recovered here: the method and path are shown as the fact
//! records them, and as unknown where it records none.
//!
//! **Requests and responses (D4 §4.2, #1998).** Each endpoint also gets a
//! row of what its handler is recorded to read and to return: the literal
//! query, body and form names of `request_inputs`, and the statuses, kinds
//! and top-level JSON keys of `responses`, each with the reasons the adapter
//! gave for not proving more. Each is a fact of its own provenance, cited by
//! its own id (`request-inputs:<endpoint>`, `responses:<endpoint>`). The
//! flask-limiter rate limits of `rate_limits` are stated the same way
//! (`rate-limits:<endpoint>`), with an environment default named. An
//! endpoint with neither says "not recovered" in the row, so an absent shape
//! is never silent. Authentication is in `endpoint-permissions.md`.
//!
//! **Listed.** Every admitted `Endpoint` fact whose confidence is not Gap,
//! once, by fact id. An `Endpoint` fact recorded as a Gap is counted and
//! listed in `gap_register.md`, not here.
//!
//! **Cost.** One pass over the admitted nodes and one sort: linear in the
//! graph plus the endpoints' sort.
//!
//! **Mode.** The producer receives the facts the export mode admitted, like
//! every other graph artifact, so a `verified-only` export holds no
//! InferredWeak row. The caller counts what the mode and rejections withheld
//! ([`withheld`]) and the document states it (R-INT-5).

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, included, provenance};
use core_graph::Node;
use core_prov::{ConfidenceTier, Provenance};
use serde::Deserialize as _;
use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

const ENDPOINT: &str = "Endpoint";
/// What an absent method or path is shown as.
const UNKNOWN: &str = "unknown";

/// How many `Endpoint` facts the export mode and the rejections withhold,
/// counted over the facts before that filter.
pub(super) fn withheld<N: Borrow<Node>>(
    nodes: &[N],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> usize {
    nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| node.label == ENDPOINT)
        .filter(|node| !included(&provenance(&node.props, &node.id), mode, rejected))
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

/// A recorded text property, or unknown. An empty string records nothing.
fn recorded<'a>(node: &'a Node, key: &str) -> &'a str {
    node.props[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(UNKNOWN)
}

/// The first evidence span, and how many more the fact index lists.
struct Span<'a>(&'a Provenance);
impl fmt::Display for Span<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(first) = self.0.evidence.first() else {
            return f.write_str("—");
        };
        write!(
            f,
            "{}:{} bytes {}..{} @ {}",
            first.repo, first.path, first.byte_start, first.byte_end, first.commit_sha
        )?;
        match self.0.evidence.len() - 1 {
            0 => Ok(()),
            more => write!(f, " (+{more} more in the fact index)"),
        }
    }
}

const INTRO: &str = "# Endpoint inventory\n\n\
Every recovered endpoint, whether or not a test references it. Each row is one `Endpoint` fact \
as an adapter recorded it. No model is involved.\n\n\
## How to read this document\n\n\
- **Method and path** are shown as the fact records them, and as `unknown` where it records \
none. Nothing is inferred to fill them in.\n\
- **Requests and responses** lists, per endpoint, the literal input names its handler is \
recorded to read and the statuses, body kinds and top-level JSON keys it is recorded to return. \
\"Not proven\" names why the adapter could not record more; \"not recovered\" means nothing was \
recorded for that endpoint, which is not evidence that it reads or returns nothing. \
Authentication is in `endpoint-permissions.md`, and `test_arrangements.md` lists the tests that \
reference each endpoint.\n\
- An endpoint recorded as a Gap is counted below and listed in `gap_register.md`, not here.\n\
- Every row carries the endpoint's tier, confidence and source span. The fact index lists \
every span.\n\n";

/// `endpoint-inventory.md` over the facts the export mode admitted.
pub(super) fn output(
    nodes: &[&Node],
    (mode, withheld): (ExportMode, usize),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut endpoints: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == ENDPOINT)
        .collect();
    endpoints.sort_by(|left, right| left.id.cmp(&right.id));
    endpoints.dedup_by(|left, right| left.id == right.id);
    let gaps = endpoints
        .iter()
        .filter(|node| provenance(&node.props, &node.id).confidence_tier == ConfidenceTier::Gap)
        .count();
    endpoints
        .retain(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap);

    let mut assertions = Assertions::new(&budget);
    for endpoint in &endpoints {
        assertions.node(endpoint, None)?;
    }

    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Endpoints listed: {}. Recorded as a Gap: {gaps}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        endpoints.len(),
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Endpoint facts left out of this export because they are InferredWeak or rejected: \
             {withheld}. A `best-effort` export includes the InferredWeak ones, marked.\n"
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "InferredWeak rows are included and marked in the Note column. Endpoint facts left \
             out of this export because they are rejected: {withheld}.\n"
        )?,
    }

    content.push_str(
        "## Endpoints\n\n\
         | Endpoint | Method | Path | Framework | Language | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    )?;
    for endpoint in &endpoints {
        let prov = provenance(&endpoint.props, &endpoint.id);
        writeln!(
            content,
            "| {} | {} | {} | {} | {} | {:?} | {:?} | {} | {} |",
            Literal(&endpoint.id),
            Literal(recorded(endpoint, "method")),
            Literal(recorded(endpoint, "path")),
            Literal(recorded(endpoint, "framework")),
            Literal(recorded(endpoint, "language")),
            prov.tier,
            prov.confidence_tier,
            Literal(Span(&prov)),
            if prov.confidence_tier == ConfidenceTier::InferredWeak {
                "rests on an InferredWeak fact; a verified-only export omits this row"
            } else {
                "—"
            }
        )?;
    }
    if endpoints.is_empty() {
        content.push_str("| — | — | — | — | — | — | — | — | None in this export |\n")?;
    }
    contract(&mut content, &mut assertions, &endpoints, mode)?;
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
    })
}

/// Subject kinds of the nested shape facts.
pub(super) const INPUTS_KIND: &str = "RequestInputs";
pub(super) const RESPONSES_KIND: &str = "Responses";
pub(super) const LIMITS_KIND: &str = "RateLimits";

/// A nested shape fact (`request_inputs`, `responses`) the mode admits.
pub(super) fn shape<'a>(
    endpoint: &'a Node,
    key: &str,
    mode: ExportMode,
) -> Option<(&'a serde_json::Value, Provenance)> {
    let raw = &endpoint.props[key];
    if !raw.is_object() {
        return None;
    }
    let prov = Provenance::deserialize(&*crate::stored_prov(raw))
        .ok()
        .filter(|prov| prov.validate().is_ok())?;
    (prov.confidence_tier != ConfidenceTier::InferredWeak || mode == ExportMode::BestEffort)
        .then_some((raw, prov))
}

pub(super) fn strings(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

/// The top-level JSON keys a `responses` shape records for `status`, when
/// the adapter tied keys to statuses (#1998, AC-0902).
pub(super) fn status_keys(raw: &serde_json::Value, status: u64) -> Vec<&str> {
    strings(&raw["keys_by_status"][status.to_string()])
}

/// The JSON keys cell: `200: id, name; 400: error` when the keys are tied to
/// their statuses, with any key whose status is computed after them, or the
/// keys alone for a fact recorded before keys were tied to statuses.
fn json_keys(raw: &serde_json::Value) -> String {
    let keys = strings(&raw["keys"]);
    let Some(by_status) = raw["keys_by_status"].as_object().filter(|m| !m.is_empty()) else {
        return if keys.is_empty() {
            "—".to_owned()
        } else {
            keys.join(", ")
        };
    };
    let mut statuses: Vec<(u64, Vec<&str>)> = by_status
        .iter()
        .filter_map(|(status, keys)| Some((status.parse().ok()?, strings(keys))))
        .collect();
    statuses.sort();
    let tied: std::collections::BTreeSet<&str> = statuses
        .iter()
        .flat_map(|(_, keys)| keys.iter().copied())
        .collect();
    let mut parts: Vec<String> = statuses
        .iter()
        .map(|(status, keys)| format!("{status}: {}", keys.join(", ")))
        .collect();
    let untied: Vec<&str> = keys.into_iter().filter(|k| !tied.contains(k)).collect();
    if !untied.is_empty() {
        parts.push(format!("status computed: {}", untied.join(", ")));
    }
    parts.join("; ")
}

/// A plain statement of a reason an adapter recorded for a partial shape.
pub(super) fn reason(key: &str) -> &str {
    match key {
        "body_name_rebound" => "the body's local name is rebound",
        "body_read_dynamically" => "a body key is computed or the body is passed on",
        "query_read_dynamically" => "a query key is computed or the query is passed on",
        "form_read_dynamically" => "a form key is computed or the form is passed on",
        "request_passed" => "the request is passed to another function",
        "response_built_elsewhere" => "a JSON body is built outside the handler",
        "response_not_proven" => "a returned value is not a recognised response",
        "status_computed" => "a status is computed",
        other => other,
    }
}

/// `100 a day, 50 an hour` for one recorded limit entry, with what qualifies
/// it; `None` for an entry whose value is not resolved.
fn limit_entry(entry: &serde_json::Value) -> Option<String> {
    let limits = entry["limits"].as_array()?;
    let parts: Vec<String> = limits
        .iter()
        .filter_map(|limit| {
            let count = limit["count"].as_u64()?;
            let per = limit["per"].as_u64()?;
            let unit = limit["unit"].as_str()?;
            Some(if per == 1 {
                let article = if unit == "hour" { "an" } else { "a" };
                format!("{count} {article} {unit}")
            } else {
                format!("{count} per {per} {unit}s")
            })
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut out = parts.join(", ");
    let mut notes = Vec::new();
    if let Some(key) = entry["environment_key"].as_str() {
        notes.push(format!(
            "the default; the environment variable {key} replaces it"
        ));
    }
    if entry["keyed_by"] == "custom" {
        notes.push("counted per a key the code computes".to_owned());
    }
    if entry["shared"] == true {
        notes.push("shared with other endpoints".to_owned());
    }
    if entry["conditional"] == true {
        notes.push("not every request counts".to_owned());
    }
    if !notes.is_empty() {
        out.push_str(&format!(" ({})", notes.join("; ")));
    }
    Some(out)
}

/// The recorded rate limits of an endpoint as one statement, and whether
/// any entry stayed unresolved. `None` when none is recorded or the mode
/// does not admit the fact.
pub(super) fn rate_limits(endpoint: &Node, mode: ExportMode) -> Option<(String, bool, Provenance)> {
    let (raw, prov) = shape(endpoint, "rate_limits", mode)?;
    let entries = raw["entries"].as_array()?;
    let stated: Vec<String> = entries.iter().filter_map(limit_entry).collect();
    let unresolved = stated.len() < entries.len();
    let text = if stated.is_empty() {
        "a limit is declared; its value is not resolved".to_owned()
    } else {
        format!("at most {}", stated.join("; and "))
    };
    Some((text, unresolved, prov))
}

/// `query a, b; body c` from the recorded input names.
pub(super) fn reads(inputs: &serde_json::Value) -> String {
    let parts: Vec<String> = [("query", "query"), ("body", "body"), ("form", "form")]
        .into_iter()
        .filter_map(|(key, name)| {
            let names = strings(&inputs[key]);
            (!names.is_empty()).then(|| format!("{name} {}", names.join(", ")))
        })
        .collect();
    if parts.is_empty() {
        "none proven".into()
    } else {
        parts.join("; ")
    }
}

/// The D4 §4.2 table: what each endpoint is recorded to read and return.
fn contract(
    content: &mut Text,
    assertions: &mut Assertions,
    endpoints: &[&Node],
    mode: ExportMode,
) -> Result<(), RenderLimit> {
    use super::citation_id;
    use core_graph::FactKey;
    let mut recovered = 0usize;
    let mut rows = String::new();
    for endpoint in endpoints {
        let inputs = shape(endpoint, "request_inputs", mode);
        let responses = shape(endpoint, "responses", mode);
        let limits = rate_limits(endpoint, mode);
        if inputs.is_none() && responses.is_none() && limits.is_none() {
            writeln!(
                rows,
                "| {} | {} | {} | not recovered | not recovered | — | none recorded | — | — |",
                Literal(&endpoint.id),
                Literal(recorded(endpoint, "method")),
                Literal(recorded(endpoint, "path")),
            )?;
            continue;
        }
        recovered += 1;
        let mut not_proven: BTreeSet<&str> = BTreeSet::new();
        let mut cites = Vec::new();
        let read = match &inputs {
            Some((raw, prov)) => {
                let id = format!("request-inputs:{}", endpoint.id);
                assertions.derived(
                    &id,
                    INPUTS_KIND,
                    &format_args!("Request inputs of {}: {}", endpoint.id, reads(raw)),
                    prov.clone(),
                )?;
                cites.push(citation_id(&FactKey::Node { id }));
                not_proven.extend(strings(&raw["partial"]).into_iter().map(reason));
                reads(raw)
            }
            None => "not recovered".into(),
        };
        let (respond, keys) = match &responses {
            Some((raw, prov)) => {
                let statuses: Vec<String> = raw["statuses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_u64)
                    .map(|status| status.to_string())
                    .collect();
                let kinds = strings(&raw["kinds"]);
                let respond = match (statuses.is_empty(), kinds.is_empty()) {
                    (true, true) => "none proven".to_owned(),
                    (true, false) => kinds.join(", "),
                    (false, true) => statuses.join(", "),
                    (false, false) => format!("{} ({})", statuses.join(", "), kinds.join(", ")),
                };
                let keys = json_keys(raw);
                let id = format!("responses:{}", endpoint.id);
                assertions.derived(
                    &id,
                    RESPONSES_KIND,
                    &format_args!("Responses of {}: {respond}; JSON keys {keys}", endpoint.id),
                    prov.clone(),
                )?;
                cites.push(citation_id(&FactKey::Node { id }));
                not_proven.extend(strings(&raw["partial"]).into_iter().map(reason));
                (respond, keys)
            }
            None => ("not recovered".into(), "—".into()),
        };
        let limit = match limits {
            Some((text, unresolved, prov)) => {
                let id = format!("rate-limits:{}", endpoint.id);
                assertions.derived(
                    &id,
                    LIMITS_KIND,
                    &format_args!("Rate limits of {}: {text}", endpoint.id),
                    prov,
                )?;
                cites.push(citation_id(&FactKey::Node { id }));
                if unresolved {
                    not_proven.insert("a declared limit's value is not resolved");
                }
                text
            }
            None => "none recorded".into(),
        };
        writeln!(
            rows,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            Literal(&endpoint.id),
            Literal(recorded(endpoint, "method")),
            Literal(recorded(endpoint, "path")),
            Literal(read),
            Literal(respond),
            Literal(keys),
            Literal(limit),
            if not_proven.is_empty() {
                "—".to_owned()
            } else {
                Literal(not_proven.into_iter().collect::<Vec<_>>().join("; ")).to_string()
            },
            cites.join(", "),
        )?;
    }
    writeln!(
        content,
        "\n## Requests and responses\n\n\
         Endpoints with a recorded request or response shape: {recovered} of {}. Inputs, \
         responses and flask-limiter rate limits are recorded for Flask handlers only; every \
         other endpoint reads \"not recovered\" and \"none recorded\", which is not evidence \
         that it has none. A rate limit refuses the requests past it with flask-limiter's \
         429 response.\n\n\
         | Endpoint | Method | Path | Reads | Responds | JSON keys | Rate limit | Not proven | \
         Citation |\n\
         |---|---|---|---|---|---|---|---|---|",
        endpoints.len()
    )?;
    if endpoints.is_empty() {
        rows.push_str("| — | — | — | — | — | — | — | — | None in this export |\n");
    }
    content.push_str(&rows)?;
    Ok(())
}

/// Borrowed index roles of this producer, for the compile workspace model.
pub(super) fn logical_index_bytes(rows: usize) -> Option<usize> {
    use super::accounting::{set_slots, slots, sum};
    sum(&[
        slots::<&Node>(rows)?,    // endpoints
        set_slots::<&str>(rows)?, // withheld ids, counted before the filter
    ])
}

#[cfg(test)]
#[path = "endpoint_inventory_tests.rs"]
mod tests;
