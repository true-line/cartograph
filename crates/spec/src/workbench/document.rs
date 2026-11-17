//! Compiled catalogue documents (#1748, SPEC-15 slice 1, ADR-0064 decision
//! 9, AC-0663): D1 as `product-specification.md`.
//!
//! The document is the catalogue's D1 arranged from the section table, over
//! facts the export already admits. No model is called, no store is read and
//! no fact is written or changed (R-INT-1, R-INT-3).
//!
//! **Sections.** Every section of the table is printed, in order, with its
//! kind. A P section is its frame. A P+A section is its frame followed by
//! the statement that it is not drafted. An A section is that statement
//! alone. An H section is a labelled blank. No draft exists before the draft
//! store (slice 3), so no claim is printed in either mode and the omitted
//! count of a `verified-only` export is zero for every section.
//!
//! **Frames.** A node frame lists every admitted non-Gap node of its label
//! with its tier, confidence and citation id, and cites each as an assertion
//! of this artifact, so the fact index has a row for every printed id
//! (ADR-0058). The flow frame summarizes the traced flows and the gap frame
//! counts the Gap findings; `flow_dossiers.md` and `gap_register.md` hold
//! and cite each one. An artifact frame names the artifacts of the same
//! export that hold the statements; they are not repeated here.
//!
//! **Header.** The catalogue version and the export mode always. The graph
//! content hash is the one the host supplies, or the supplied curated
//! projection's; the curated revision is that projection's. A compile that
//! is supplied neither says so, and one whose projection could not be
//! computed still states the graph hash and gives the reason code.
//!
//! **Cost.** One pass over the nodes per node frame and one over the flows.
//! Nothing is indexed or walked.

use super::catalogue::{
    CATALOGUE_VERSION, CatalogueSection, SectionFrame, SectionKind, catalogue_sections,
};
use super::curated::Curated;
use super::producers::Output;
use super::render::{Assertions, Budget, Literal, NodeName, RenderLimit, Text};
use super::{ExportMode, citation_id, provenance};
use core_graph::{FactKey, Node};
use core_prov::ConfidenceTier;
use flowtracer::Flow;
use std::fmt::Write as _;

/// Artifact id of `product-specification.md`.
pub(super) const D1_ARTIFACT_ID: &str = "d1-product-specification";

const NOT_DRAFTED: &str = "_Not drafted._ No accepted draft is recorded for this section.\n";

const HUMAN_BLANK: &str = "_Left blank for the team._ The code cannot show this, and nothing is \
                           written here from it.\n";

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::VerifiedOnly => "verified-only",
        ExportMode::BestEffort => "best-effort",
    }
}

/// The admitted non-Gap nodes of `label`, in the order the export holds
/// them, each with its provenance.
fn framed<'a>(nodes: &[&'a Node], label: &str) -> Vec<(&'a Node, core_prov::Provenance)> {
    nodes
        .iter()
        .copied()
        .filter(|node| node.label == label)
        .map(|node| (node, provenance(&node.props, &node.id)))
        .filter(|(_, prov)| prov.confidence_tier != ConfidenceTier::Gap)
        .collect()
}

fn frame(
    section: &CatalogueSection,
    source: SectionFrame,
    (nodes, flows, gaps): (&[&Node], &[Flow], usize),
    assertions: &mut Assertions,
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    match source {
        SectionFrame::Nodes(label) => {
            let rows = framed(nodes, label);
            if rows.is_empty() {
                writeln!(out, "No `{label}` fact is in this export.")?;
                return Ok(0);
            }
            out.push_str("| Name | Tier | Confidence | Citation |\n|---|---|---|---|\n")?;
            for (node, prov) in &rows {
                assertions.node(node, None)?;
                writeln!(
                    out,
                    "| {} | {:?} | {:?} | {} |",
                    Literal(NodeName(node)),
                    prov.tier,
                    prov.confidence_tier,
                    citation_id(&FactKey::Node {
                        id: node.id.clone()
                    })
                )?;
            }
            Ok(rows.len())
        }
        SectionFrame::Flows => {
            if flows.is_empty() {
                out.push_str("No flow is traced in this export.\n")?;
                return Ok(0);
            }
            out.push_str("| Trigger | Kind | Status | Hops |\n|---|---|---|---|\n")?;
            for flow in flows {
                writeln!(
                    out,
                    "| {} | {} | {:?} | {} |",
                    Literal(&flow.trigger_name),
                    Literal(&flow.trigger_kind),
                    flow.status,
                    flow.hops.len()
                )?;
            }
            writeln!(
                out,
                "\nEach hop, with its tier, confidence and citation, is in `flow_dossiers.md`."
            )?;
            Ok(0)
        }
        SectionFrame::Gaps => {
            writeln!(
                out,
                "Gap findings in this export: {gaps}. Each is listed with its reason and \
                 citation in `gap_register.md`. A Gap is something the trace could not \
                 establish; nothing in this document fills one."
            )?;
            Ok(0)
        }
        SectionFrame::Artifacts(files) => {
            out.push_str("The projected statements of this section are in ")?;
            for (index, file) in files.iter().enumerate() {
                if index > 0 {
                    out.push_str(" and ")?;
                }
                write!(out, "`{file}`")?;
            }
            writeln!(
                out,
                " of this export, each with its tier, confidence and citation. They are not \
                 yet arranged per {} and are not repeated here.",
                section.subject.unwrap_or("section").to_lowercase()
            )?;
            Ok(0)
        }
    }
}

pub(super) fn d1(
    nodes: &[&Node],
    flows: &[Flow],
    gaps: usize,
    (curated, graph_content_hash): (Curated<'_>, Option<&str>),
    mode: ExportMode,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    let mut content = Text::new(&budget, "# Product specification\n\n")?;
    writeln!(
        content,
        "- Document: D1 of document catalogue version {CATALOGUE_VERSION}\n\
         - Export mode: `{}`",
        mode_name(mode)
    )?;
    // The host's own hash first: it is known even when no projection is.
    let graph_hash = graph_content_hash.or(curated
        .projection()
        .map(|projection| projection.graph_basis.as_str()));
    match graph_hash {
        Some(hash) => writeln!(content, "- Graph content hash: {}", Literal(hash))?,
        None => content.push_str("- Graph content hash: not supplied to this compilation\n")?,
    }
    match curated {
        Curated::Computed(projection) => writeln!(
            content,
            "- Curated revision: {}",
            Literal(&projection.revision)
        )?,
        Curated::NotComputed(failure) => writeln!(
            content,
            "- Curated revision: not computed (`{}`); see `curated.md`",
            failure.code()
        )?,
        Curated::NotSupplied => {
            content.push_str("- Curated revision: not supplied to this compilation\n")?;
        }
    }
    content.push_str(
        "\nSection kinds: **P** is compiled from recovered facts. **A** is drafted by an agent \
         role from cited facts and shown only after a human accepts it; it stays T3 / \
         InferredWeak. **P+A** is a projected frame followed by drafted prose. **H** is left \
         for the team. A citation id resolves in `fact-index.md`.\n",
    )?;

    let mut cited = 0usize;
    for section in catalogue_sections("D1") {
        writeln!(
            content,
            "\n## {} {} ({})\n",
            section.number,
            section.title,
            section.kind.mark()
        )?;
        if let Some(source) = section.frame {
            cited += frame(
                section,
                source,
                (nodes, flows, gaps),
                &mut assertions,
                &mut content,
            )?;
        }
        match section.kind {
            SectionKind::Projected => {}
            SectionKind::Framed => {
                content.push_str("\n")?;
                content.push_str(NOT_DRAFTED)?;
            }
            SectionKind::Authored => content.push_str(NOT_DRAFTED)?,
            SectionKind::Human => content.push_str(HUMAN_BLANK)?,
        }
        if section.kind.drafted() && mode == ExportMode::VerifiedOnly {
            content.push_str("Drafted claims left out of this `verified-only` export: 0.\n")?;
        }
    }
    writeln!(
        content,
        "\n## Statements in this document\n\n\
         Cited facts printed in the frames above: {cited}. Drafted claims printed: 0."
    )?;
    // Counted by tier, as the catalogue asks of every document.
    let mut tiers: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for assertion in &assertions.values {
        *tiers
            .entry(format!("{:?}", assertion.provenance.tier))
            .or_default() += 1;
    }
    for (tier, count) in &tiers {
        writeln!(content, "- {tier}: {count}")?;
    }
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
    })
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
