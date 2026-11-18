//! Compiled catalogue documents (#1748, SPEC-15 slice 1, ADR-0064 decision
//! 9, AC-0663): D1 as `product-specification.md`.
//!
//! The document is the catalogue's D1 arranged from the section table, over
//! facts the export already admits. No model is called, no store is read and
//! no fact is written or changed (R-INT-1, R-INT-3).
//!
//! **Sections.** Every section of the table is printed, in order, with its
//! kind. A P section is its frame. A P+A section is its frame followed by
//! its drafts. An A section is its drafts alone. An H section is a labelled
//! blank.
//!
//! **Drafts** (slice 5, AC-0667). The drafts of a section are the accepted
//! drafts the supplied curated projection lists for its address, in scope
//! and then identity order. An `eligible` draft is printed in a
//! `best-effort` export: each claim with its kind, `T3 / InferredWeak`, the
//! author role, `human-reviewed: accepted` and its citation ids. Each cited
//! fact becomes an assertion of this artifact, so the fact index has a row
//! for every printed id. A draft citing a fact this export does not admit
//! (a rejected inference, say) is not printed, and the section says so: no
//! claim is shown with a citation that does not resolve. A `verified-only`
//! export prints no claim and counts the claims left out. A draft in any
//! other state is named with its state and reason in both modes. A section
//! with no printed draft says it is not drafted.
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
//! When an eligible draft is to be printed, one more pass over the admitted
//! nodes and edges finds the cited facts; with no such draft nothing is
//! indexed or walked.

use super::catalogue::{
    CATALOGUE_VERSION, CatalogueSection, SectionFrame, SectionKind, catalogue_section,
    catalogue_sections,
};
use super::curated::Curated;
use super::producers::Output;
use super::render::{Assertions, Budget, Literal, NodeName, RenderLimit, Text};
use super::{ExportMode, citation_id, provenance};
use context_hub::curated::{CuratedDraft, DraftState, DraftStateReason};
use core_graph::{Edge, FactKey, Node};
use core_prov::ConfidenceTier;
use flowtracer::Flow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The scope of a section written once for the whole system.
pub const SYSTEM_SCOPE: &str = "system";

/// Artifact id of `product-specification.md`.
pub(super) const D1_ARTIFACT_ID: &str = "d1-product-specification";

const NOT_DRAFTED: &str = "_Not drafted._ No accepted draft is recorded for this section.\n";

const NO_ELIGIBLE_DRAFT: &str = "_Not drafted._ No eligible draft is recorded for this section.\n";

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
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
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
                let id = citation_id(&FactKey::Node {
                    id: node.id.clone(),
                });
                writeln!(
                    out,
                    "| {} | {:?} | {:?} | {id} |",
                    Literal(NodeName(node)),
                    prov.tier,
                    prov.confidence_tier,
                )?;
                // Once per fact: a draft above may already cite it.
                if asserted.insert(id) {
                    assertions.node(node, None)?;
                }
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

pub(super) fn state_name(state: DraftState) -> &'static str {
    match state {
        DraftState::Superseded => "superseded",
        DraftState::Unresolvable => "unresolvable",
        DraftState::Stale => "stale",
        DraftState::Conflicted => "conflicted",
        DraftState::Eligible => "eligible",
    }
}

pub(super) fn reason_name(reason: DraftStateReason) -> &'static str {
    match reason {
        DraftStateReason::ReplacedByAcceptedDraft => "replaced_by_accepted_draft",
        DraftStateReason::CitedFactMissing => "cited_fact_missing",
        DraftStateReason::CitedFactChanged => "cited_fact_changed",
        DraftStateReason::AnotherDraftAccepted => "another_draft_accepted",
        DraftStateReason::AllConditionsHold => "all_conditions_hold",
    }
}

fn edge_key(edge: &Edge) -> FactKey {
    FactKey::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    }
}

/// The current content hash of each fact `cited` names, by citation id: the
/// hash a drafting run records and the curated projection later compares
/// (ADR-0064 decision 8). An id that names no fact of `nodes` and `edges`
/// is absent. One pass over both; nothing is kept beyond the cited ids.
pub fn cited_fact_hashes(
    nodes: &[Node],
    edges: &[Edge],
    cited: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    hashes_of(nodes.iter(), edges.iter(), cited)
}

fn hashes_of<'a>(
    nodes: impl Iterator<Item = &'a Node>,
    edges: impl Iterator<Item = &'a Edge>,
    cited: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    if cited.is_empty() {
        return hashes;
    }
    for node in nodes {
        let id = citation_id(&FactKey::Node {
            id: node.id.clone(),
        });
        if cited.contains(&id) {
            let hash = provenance(&node.props, &format!("node:{}", node.id)).content_hash;
            hashes.insert(id, hash);
        }
    }
    for edge in edges {
        let id = citation_id(&edge_key(edge));
        if cited.contains(&id) {
            let identity = format!("edge:{} {} {}", edge.src, edge.label, edge.dst);
            hashes.insert(id, provenance(&edge.props, &identity).content_hash);
        }
    }
    hashes
}

/// Why an address has no citation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionScopeError {
    /// The section table of this catalogue version holds no such section,
    /// or the section is not drafted (it is P or H).
    NotDrafted,
    /// The section is not written per subject, so only the system scope
    /// addresses it.
    NotPerSubject,
    /// The scope names no node of the section's subject label.
    SubjectNotFound,
}

impl SectionScopeError {
    /// The reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotDrafted => "section_not_drafted",
            Self::NotPerSubject => "section_not_per_subject",
            Self::SubjectNotFound => "subject_not_found",
        }
    }
}

/// The section a draft for `(catalogue_version, document, number)` targets:
/// a section of the table, of this catalogue version, of a drafted kind.
pub fn drafted_section(
    catalogue_version: &str,
    document: &str,
    number: &str,
) -> Result<&'static CatalogueSection, SectionScopeError> {
    (catalogue_version == CATALOGUE_VERSION)
        .then(|| catalogue_section(document, number))
        .flatten()
        .filter(|section| section.kind.drafted())
        .ok_or(SectionScopeError::NotDrafted)
}

/// The citation set of a drafting run for `section` and `scope`, restricted
/// to the ids in `cited`: citation id to current content hash (ADR-0064
/// decision 3). An id `cited` names that is outside the set is absent, so
/// admitting a finish against the result refuses a claim that cites it.
///
/// The system scope admits every fact of `nodes` and `edges`, which holds
/// the section's frame. A subject scope admits the subject node, every edge
/// that touches it and the node at the other end of each. One pass over
/// each; nothing is kept beyond the cited ids.
pub fn section_citation_set(
    section: &CatalogueSection,
    scope: &str,
    nodes: &[Node],
    edges: &[Edge],
    cited: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, SectionScopeError> {
    if scope == SYSTEM_SCOPE {
        return Ok(cited_fact_hashes(nodes, edges, cited));
    }
    let label = section.subject.ok_or(SectionScopeError::NotPerSubject)?;
    let subject = nodes
        .iter()
        .find(|node| {
            node.label == label
                && citation_id(&FactKey::Node {
                    id: node.id.clone(),
                }) == scope
        })
        .ok_or(SectionScopeError::SubjectNotFound)?;
    let touching = |edge: &&Edge| edge.src == subject.id || edge.dst == subject.id;
    let mut near: BTreeSet<&str> = BTreeSet::from([subject.id.as_str()]);
    for edge in edges.iter().filter(touching) {
        near.insert(edge.src.as_str());
        near.insert(edge.dst.as_str());
    }
    Ok(hashes_of(
        nodes.iter().filter(|node| near.contains(node.id.as_str())),
        edges.iter().filter(touching),
        cited,
    ))
}

#[derive(Clone, Copy)]
enum Fact<'a> {
    Node(&'a Node),
    Edge(&'a Edge),
}

/// The accepted D1 drafts of the supplied projection, by section number,
/// each section's in scope and then identity order.
fn drafts_by_section<'a>(curated: Curated<'a>) -> BTreeMap<&'a str, Vec<&'a CuratedDraft>> {
    let mut sections: BTreeMap<&str, Vec<&CuratedDraft>> = BTreeMap::new();
    for draft in curated
        .projection()
        .map_or(&[][..], |projection| projection.drafts.as_slice())
    {
        if draft.address.document == "D1" && draft.address.catalogue_version == CATALOGUE_VERSION {
            sections
                .entry(draft.address.section.as_str())
                .or_default()
                .push(draft);
        }
    }
    for drafts in sections.values_mut() {
        drafts.sort_by(|left, right| {
            (&left.address.scope, &left.draft_id).cmp(&(&right.address.scope, &right.draft_id))
        });
    }
    sections
}

/// The admitted facts the printed drafts cite, by citation id. A stale draft
/// is printed in place (ADR-0045), so its citations are looked up too.
fn cited_facts<'a>(
    drafts: &BTreeMap<&str, Vec<&CuratedDraft>>,
    nodes: &[&'a Node],
    edges: &[&'a Edge],
) -> BTreeMap<String, Fact<'a>> {
    let wanted: BTreeSet<&str> = drafts
        .values()
        .flatten()
        .filter(|draft| matches!(draft.state, DraftState::Eligible | DraftState::Stale))
        .flat_map(|draft| &draft.claims)
        .flat_map(|claim| &claim.citations)
        .map(String::as_str)
        .collect();
    let mut facts = BTreeMap::new();
    if wanted.is_empty() {
        return facts;
    }
    for node in nodes {
        let id = citation_id(&FactKey::Node {
            id: node.id.clone(),
        });
        if wanted.contains(id.as_str()) {
            facts.insert(id, Fact::Node(node));
        }
    }
    for edge in edges {
        let id = citation_id(&edge_key(edge));
        if wanted.contains(id.as_str()) {
            facts.insert(id, Fact::Edge(edge));
        }
    }
    facts
}

/// Printed above an accepted draft whose cited facts changed (SPEC-17).
const STALE_LINE: &str = "Stale: cited facts changed since this text was accepted";

/// The header value of `Stale sections:`: the section number and scope of
/// every stale or unresolvable accepted draft, in document order.
fn stale_sections(drafts: &BTreeMap<&str, Vec<&CuratedDraft>>) -> String {
    let listed: Vec<String> = catalogue_sections("D1")
        .filter_map(|section| drafts.get(section.number))
        .flatten()
        .filter(|draft| matches!(draft.state, DraftState::Stale | DraftState::Unresolvable))
        .map(|draft| {
            format!(
                "{} (scope `{}`, `{}`)",
                Literal(&draft.address.section),
                Literal(&draft.address.scope),
                state_name(draft.state)
            )
        })
        .collect();
    if listed.is_empty() {
        "none".to_owned()
    } else {
        listed.join(", ")
    }
}

struct Drafted<'a, 'b> {
    facts: &'b BTreeMap<String, Fact<'a>>,
    /// Citation ids already asserted by this artifact.
    asserted: BTreeSet<String>,
    /// Claims printed in the document.
    printed: usize,
}

impl Drafted<'_, '_> {
    /// The drafts of one section. Returns the number of claims a
    /// `verified-only` export left out.
    fn section(
        &mut self,
        drafts: &[&CuratedDraft],
        mode: ExportMode,
        assertions: &mut Assertions,
        out: &mut Text,
    ) -> Result<usize, RenderLimit> {
        let mut shown = false;
        let mut omitted = 0usize;
        let mut notes: Vec<String> = Vec::new();
        for draft in drafts {
            let stale = draft.state == DraftState::Stale;
            if draft.state == DraftState::Unresolvable {
                // The ids are not printed here: every id this document
                // prints resolves in the fact index, and these do not.
                notes.push(format!(
                    "- Draft `{}` for scope `{}` is listed as `{}` (`{}`) and is not printed: \
                     {} of the facts it cites {} no longer in the graph. `curated.md` names \
                     them.",
                    Literal(&draft.draft_id),
                    Literal(&draft.address.scope),
                    state_name(draft.state),
                    reason_name(draft.reason),
                    draft.missing.len(),
                    if draft.missing.len() == 1 {
                        "is"
                    } else {
                        "are"
                    }
                ));
                continue;
            }
            if !stale && draft.state != DraftState::Eligible {
                notes.push(format!(
                    "- Draft `{}` for scope `{}` is listed as `{}` (`{}`).",
                    Literal(&draft.draft_id),
                    Literal(&draft.address.scope),
                    state_name(draft.state),
                    reason_name(draft.reason)
                ));
                continue;
            }
            if mode == ExportMode::VerifiedOnly {
                omitted += draft.claims.len();
                shown = true;
                if stale {
                    notes.push(format!(
                        "- Draft `{}` for scope `{}` is listed as `{}` (`{}`).",
                        Literal(&draft.draft_id),
                        Literal(&draft.address.scope),
                        state_name(draft.state),
                        reason_name(draft.reason)
                    ));
                }
                continue;
            }
            let resolves = draft
                .claims
                .iter()
                .flat_map(|claim| &claim.citations)
                .all(|id| self.facts.contains_key(id));
            if !resolves {
                notes.push(format!(
                    "- Draft `{}` for scope `{}` is {} and is not printed: it cites a \
                     fact this export does not list.",
                    Literal(&draft.draft_id),
                    Literal(&draft.address.scope),
                    state_name(draft.state)
                ));
                continue;
            }
            if shown {
                out.push_str("\n")?;
            }
            shown = true;
            writeln!(
                out,
                "Drafted for scope `{}` (draft `{}`):\n",
                Literal(&draft.address.scope),
                Literal(&draft.draft_id)
            )?;
            if stale {
                writeln!(out, "{STALE_LINE}: {}.\n", draft.changed.join(", "))?;
            }
            for (index, claim) in draft.claims.iter().enumerate() {
                write!(
                    out,
                    "{}. {} ({}; T3 / InferredWeak; role `{}`; human-reviewed: accepted; cites ",
                    index + 1,
                    Literal(&claim.statement),
                    Literal(&claim.kind),
                    Literal(&draft.role)
                )?;
                for (position, id) in claim.citations.iter().enumerate() {
                    if position > 0 {
                        out.push_str(", ")?;
                    }
                    out.push_str(id)?;
                    if self.asserted.insert(id.clone()) {
                        match self.facts[id] {
                            Fact::Node(node) => assertions.node(node, None)?,
                            Fact::Edge(edge) => assertions.edge(edge)?,
                        }
                    }
                }
                if stale && claim.citations.iter().any(|id| draft.changed.contains(id)) {
                    out.push_str("; cites a changed fact")?;
                }
                out.push_str(")\n")?;
                self.printed += 1;
            }
        }
        if !shown {
            out.push_str(if drafts.is_empty() {
                NOT_DRAFTED
            } else {
                NO_ELIGIBLE_DRAFT
            })?;
        }
        for note in &notes {
            writeln!(out, "{note}")?;
        }
        Ok(omitted)
    }
}

pub(super) fn d1(
    (nodes, edges): (&[&Node], &[&Edge]),
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
    let drafts = drafts_by_section(curated);
    match curated {
        Curated::Computed(_) => {
            writeln!(content, "- Stale sections: {}", stale_sections(&drafts))?;
        }
        Curated::NotComputed(_) | Curated::NotSupplied => {
            content.push_str("- Stale sections: not known without a curated projection\n")?;
        }
    }
    content.push_str(
        "\nSection kinds: **P** is compiled from recovered facts. **A** is drafted by an agent \
         role from cited facts and shown only after a human accepts it; it stays T3 / \
         InferredWeak. **P+A** is a projected frame followed by drafted prose. **H** is left \
         for the team. A citation id resolves in `fact-index.md`.\n",
    )?;

    let facts = match mode {
        ExportMode::BestEffort => cited_facts(&drafts, nodes, edges),
        // No claim is printed, so no cited fact is looked for.
        ExportMode::VerifiedOnly => BTreeMap::new(),
    };
    let mut drafted = Drafted {
        facts: &facts,
        asserted: BTreeSet::new(),
        printed: 0,
    };
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
                (&mut assertions, &mut drafted.asserted),
                &mut content,
            )?;
        }
        match section.kind {
            SectionKind::Projected => {}
            SectionKind::Framed => content.push_str("\n")?,
            SectionKind::Authored => {}
            SectionKind::Human => content.push_str(HUMAN_BLANK)?,
        }
        if section.kind.drafted() {
            let listed = drafts.get(section.number).map_or(&[][..], Vec::as_slice);
            let omitted = drafted.section(listed, mode, &mut assertions, &mut content)?;
            if mode == ExportMode::VerifiedOnly {
                writeln!(
                    content,
                    "Drafted claims left out of this `verified-only` export: {omitted}."
                )?;
            }
        }
    }
    writeln!(
        content,
        "\n## Statements in this document\n\n\
         Cited facts printed in the frames above: {cited}. Drafted claims printed: {}.",
        drafted.printed
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

#[cfg(test)]
#[path = "document_draft_tests.rs"]
mod draft_tests;
