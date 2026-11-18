//! Endpoint inventory (#1658, AC-0694, catalogue D4 §4.1):
//! `endpoint-inventory.md`.
//!
//! A deterministic (T0) projection of the recovered `Endpoint` facts, one
//! row each, whether or not a test references it. No model is called and
//! nothing is recovered here: the method and path are shown as the fact
//! records them, and as unknown where it records none. No response schema,
//! parameter or authentication is stated; `endpoint-permissions.md` lists
//! the recovered permission sites.
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
- **Nothing else is stated.** This document states no parameters, response schemas or \
authentication. `endpoint-permissions.md` lists the permission sites recovered for endpoints, \
and `test_arrangements.md` lists the tests that reference them.\n\
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
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
    })
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
