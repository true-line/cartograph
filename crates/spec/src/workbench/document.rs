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
//! export that hold the statements; they are not repeated here. With
//! derived Capabilities, §1.5 lists the rejection exits each one reaches
//! (`document_rules.rs`, AC-0849).
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

use super::capabilities::{NO_AREA, Stories, Story, Support, area_name};
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

/// §1.1 points to the drawn system (#2049): the diagrams are compiled in
/// every export, whether or not the section's prose is drafted.
const AT_A_GLANCE: &str = "The system at a glance is drawn in [`architecture.md`](architecture.md): \
who uses it and what it depends on (C4 level 1), the parts that run on their own with their \
data stores (level 2) and each part's components (level 3), every box and arrow citing the \
facts it rests on. [`architecture.dsl`](architecture.dsl) holds the same model as a \
Structurizr workspace.\n\n";

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

/// [`cited_fact_hashes`] over `nodes` alone: the knowledge overlay's
/// derived Capabilities and Actors, which a draft may cite beside the base
/// facts (#2040).
pub fn cited_node_hashes<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    cited: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    hashes_of(nodes.into_iter(), std::iter::empty(), cited)
}

/// The current digest of each fact `cited` names, by citation id: the
/// canonical digest of every field of the node or edge, which an
/// investigation records for each fact it reads and the curated projection
/// compares for a finding (ADR-0074 decision 5). An id that names no fact
/// of `nodes` and `edges` is absent, and so is a fact whose digest cannot
/// be computed. One pass over both; nothing is kept beyond the cited ids.
pub fn cited_fact_digests(
    nodes: &[Node],
    edges: &[Edge],
    cited: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut digests = BTreeMap::new();
    if cited.is_empty() {
        return digests;
    }
    for node in nodes {
        let id = citation_id(&FactKey::Node {
            id: node.id.clone(),
        });
        if cited.contains(&id)
            && let Ok(digest) = core_graph::source::node_digest(&stored_node(node))
        {
            digests.insert(id, digest);
        }
    }
    for edge in edges {
        let id = citation_id(&edge_key(edge));
        if cited.contains(&id)
            && let Ok(digest) = core_graph::source::edge_digest(&stored_edge(edge))
        {
            digests.insert(id, digest);
        }
    }
    digests
}

/// A fact as it was stored, for a digest over every field: borrowed unless
/// the base holds its `prov` or rule payload as text (#2215).
fn stored_node(node: &Node) -> std::borrow::Cow<'_, Node> {
    use core_graph::stored::compact_text;
    if compact_text(&node.props["prov"]).is_some() || compact_text(&node.props["rule"]).is_some() {
        let mut node = node.clone();
        crate::workbench::restore_stored(&node.label, &mut node.props);
        std::borrow::Cow::Owned(node)
    } else {
        std::borrow::Cow::Borrowed(node)
    }
}

fn stored_edge(edge: &Edge) -> std::borrow::Cow<'_, Edge> {
    if core_graph::stored::compact_text(&edge.props["prov"]).is_some() {
        let mut edge = edge.clone();
        core_graph::stored::restore_prov(&mut edge.props);
        std::borrow::Cow::Owned(edge)
    } else {
        std::borrow::Cow::Borrowed(edge)
    }
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
/// that touches it and the node at the other end of each, and then each
/// `DocSection` whose `MENTIONS` link names one of those nodes, with that
/// link, so a `documented_intent` claim can cite the section it reports
/// (ADR-0065, AC-0681). Two passes over the edges and one over the nodes;
/// nothing is kept beyond the cited ids.
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
    // The documentation sections that name a fact of the set. A link has a
    // section as its source and never makes the set grow further.
    let mentions = |edge: &&Edge| {
        edge.label == "MENTIONS"
            && edge.src.starts_with("docsec:")
            && near.contains(edge.dst.as_str())
    };
    let sections: BTreeSet<&str> = edges
        .iter()
        .filter(mentions)
        .map(|edge| edge.src.as_str())
        .collect();
    Ok(hashes_of(
        nodes
            .iter()
            .filter(|node| near.contains(node.id.as_str()) || sections.contains(node.id.as_str())),
        edges.iter().filter(|edge| touching(edge) || mentions(edge)),
        cited,
    ))
}

#[derive(Clone, Copy)]
pub(super) enum Fact<'a> {
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
    facts_named(&wanted, nodes, edges)
}

/// The facts of `nodes` and `edges` that `wanted` names, by citation id.
/// One pass over both, and none when nothing is wanted.
pub(super) fn facts_named<'a>(
    wanted: &BTreeSet<&str>,
    nodes: &[&'a Node],
    edges: &[&'a Edge],
) -> BTreeMap<String, Fact<'a>> {
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

/// The headings of a frame arranged per capability. With areas (#260) each
/// area is one `###` heading over its Capabilities' `####` headings;
/// without, every Capability keeps its own `###` heading.
pub(super) struct AreaHeadings<'a> {
    arranged: bool,
    last: Option<Option<&'a str>>,
}
impl<'a> AreaHeadings<'a> {
    pub(super) fn new(knowledge: &Stories<'a>) -> Self {
        Self {
            // Areas exist when the host supplied Subsystems, whether or not
            // a Capability is in one (#2066).
            arranged: !knowledge.areas.is_empty() || !knowledge.file_areas.is_empty(),
            last: None,
        }
    }
    /// Write the area heading when `area` differs from the previous
    /// Capability's, and return the marks of the Capability's own heading.
    pub(super) fn before(
        &mut self,
        area: Option<&'a Node>,
        out: &mut Text,
    ) -> Result<&'static str, RenderLimit> {
        if !self.arranged {
            return Ok("###");
        }
        let id = area.map(|node| node.id.as_str());
        if self.last != Some(id) {
            self.last = Some(id);
            match area {
                Some(node) => writeln!(out, "### {}\n", Literal(area_name(node)))?,
                None => writeln!(out, "### {NO_AREA}\n")?,
            }
        }
        Ok("####")
    }
}

/// The Endpoints a derived story cites, in citation order.
fn cited_endpoints<'s, 'a>(story: &'s Story<'a>) -> impl Iterator<Item = &'a Node> + 's {
    story.support.iter().filter_map(|(_, fact)| match fact {
        // An HTTP operation, or a process start or scheduled job (#1997).
        Support::Node(node)
            if matches!(node.label.as_str(), "Endpoint" | "EntryPoint" | "Schedule") =>
        {
            Some(*node)
        }
        _ => None,
    })
}

/// The sentence stating how many HTTP endpoints no Capability cites, and
/// why (#2007): an endpoint whose `HANDLES` edge reaches no Symbol has no
/// resolved handler, so no rule can group it; one with a handler is in no
/// capability rule's scope, counted by framework. `None` when every
/// endpoint is cited, or there is none.
fn uncovered_endpoints(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: Option<&Stories<'_>>,
) -> Option<String> {
    // A Gap endpoint is in the gap register, not the inventory: not counted.
    let endpoints: Vec<&Node> = framed(nodes, "Endpoint")
        .into_iter()
        .map(|(node, _)| node)
        .collect();
    if endpoints.is_empty() {
        return None;
    }
    let cited: BTreeSet<&str> = knowledge
        .into_iter()
        .flat_map(|k| k.included.iter())
        .filter(|story| story.node.label == "Capability")
        .flat_map(cited_endpoints)
        .map(|n| n.id.as_str())
        .collect();
    let symbols: BTreeSet<&str> = nodes
        .iter()
        .filter(|n| n.label == "Symbol")
        .map(|n| n.id.as_str())
        .collect();
    let handled: BTreeSet<&str> = edges
        .iter()
        .filter(|e| e.label == "HANDLES" && symbols.contains(e.dst.as_str()))
        .map(|e| e.src.as_str())
        .collect();
    let front_doors =
        crate::front_doors::vercel_front_doors(endpoints.iter().copied(), edges.iter().copied());
    let mut unresolved = 0usize;
    let mut fronting = 0usize;
    let mut by_framework: BTreeMap<&str, usize> = BTreeMap::new();
    for endpoint in endpoints.iter().filter(|n| !cited.contains(n.id.as_str())) {
        if front_doors.contains_key(endpoint.id.as_str()) {
            fronting += 1;
        } else if handled.contains(endpoint.id.as_str()) {
            *by_framework
                .entry(endpoint.props["framework"].as_str().unwrap_or("unknown"))
                .or_default() += 1;
        } else {
            unresolved += 1;
        }
    }
    let ungrouped: usize = by_framework.values().sum();
    if unresolved + ungrouped + fronting == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if fronting > 0 {
        // Each front door's function serves the same routes: count each once.
        let behind: usize = front_doors
            .iter()
            .filter(|(id, _)| !cited.contains(*id))
            .filter_map(|(id, n)| {
                endpoints
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| (e.props["handler_sym"].as_str(), *n))
            })
            .collect::<BTreeMap<_, _>>()
            .values()
            .sum();
        parts.push(format!(
            "{fronting} are Vercel functions that pass requests on to the {behind} routes \
             registered behind them, which are the operations"
        ));
    }
    if unresolved > 0 {
        parts.push(format!(
            "{unresolved} have no resolved handler (their `HANDLES` target is a Gap or missing), \
             so no rule can group them"
        ));
    }
    if ungrouped > 0 {
        let frameworks: Vec<String> = by_framework
            .iter()
            .map(|(framework, n)| format!("{} {n}", Literal(framework)))
            .collect();
        parts.push(format!(
            "{ungrouped} have a handler that no capability rule groups (by framework: {})",
            frameworks.join(", ")
        ));
    }
    Some(format!(
        "\nOperations in no Capability: {} of {} HTTP endpoints. {}. Each is listed in \
         `endpoint-inventory.md`.\n",
        unresolved + ungrouped + fronting,
        endpoints.len(),
        parts.join("; ")
    ))
}

/// The knowledge layer's derived facts of `label` (#1443): Capabilities as
/// one row each with the operations they group, Actor caller sets grouped
/// by the permissions they list. Each printed fact is cited once. They are
/// Semantic / InferredWeak: derived by rule from the cited base facts.
fn derived_frame(
    label: &str,
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let stories: Vec<&Story<'_>> = knowledge
        .included
        .iter()
        .filter(|s| s.node.label == label)
        .collect();
    // Area facts cited beside the stories (#260).
    let mut extra = 0usize;
    for story in &stories {
        let id = citation_id(&FactKey::Node {
            id: story.node.id.clone(),
        });
        if asserted.insert(id) {
            assertions.node(story.node, None)?;
        }
    }
    if label == "Actor" {
        // Caller sets by the permissions they list: one role per set.
        let operations: BTreeMap<&str, &Node> = knowledge
            .included
            .iter()
            .flat_map(cited_endpoints)
            .map(|endpoint| (endpoint.id.as_str(), endpoint))
            .collect();
        // Each role's operations and the derived Actors behind it (#2326).
        let mut roles: BTreeMap<Vec<&str>, (Vec<String>, Vec<String>)> = BTreeMap::new();
        for story in &stories {
            let any_of: Vec<&str> = story.node.props["any_of"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            let endpoint = story.node.props["endpoint_id"].as_str().unwrap_or_default();
            let operation = operations
                .get(endpoint)
                .map_or_else(|| endpoint.to_owned(), |n| NodeName(n).to_string());
            let (listed, actors) = roles.entry(any_of).or_default();
            if !listed.contains(&operation) {
                listed.push(operation);
            }
            actors.push(citation_id(&FactKey::Node {
                id: story.node.id.clone(),
            }));
        }
        writeln!(
            out,
            "Derived caller roles: {} permission sets over {} guarded operations. A caller \
             holding any one listed permission is admitted. Assumed from each guard's \
             convention (Semantic / InferredWeak), not effective runtime authorization or an \
             identified user; `user_stories.md` lists each operation's caller set.\n",
            roles.len(),
            stories.len()
        )?;
        out.push_str("| Any of | Operations | Examples | Citation |\n|---|---|---|---|\n")?;
        let and_more = |more: usize| {
            if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            }
        };
        for (any_of, (operations, actors)) in &roles {
            let shown: Vec<&str> = operations.iter().take(3).map(String::as_str).collect();
            let more = operations.len() - shown.len();
            // At most three of the role's Actors, so a composer can cite it.
            let cited: Vec<&str> = actors.iter().take(3).map(String::as_str).collect();
            writeln!(
                out,
                "| {} | {} | {}{} | {}{} |",
                Literal(any_of.join(", ")),
                operations.len(),
                Literal(shown.join("; ")),
                and_more(more),
                cited.join(", "),
                and_more(actors.len() - cited.len()),
            )?;
        }
    } else {
        writeln!(
            out,
            "Derived Capabilities: {}. Each groups the operations one controller or resolver \
             serves, or the process starts and scheduled jobs that begin work no HTTP request \
             starts (Semantic / InferredWeak, derived by rule from the cited facts); \
             `user_stories.md` states them as stories.\n",
            stories.len()
        )?;
        let listed = knowledge.capabilities();
        let arranged = !knowledge.areas.is_empty();
        if arranged {
            // Each area once, with its citation, before the rows it heads.
            let mut areas: Vec<(&Node, usize)> = Vec::new();
            for (area, _) in &listed {
                if let Some(area) = area {
                    match areas.last_mut() {
                        Some((last, count)) if last.id == area.id => *count += 1,
                        _ => areas.push((area, 1)),
                    }
                }
            }
            writeln!(
                out,
                "Capability areas: {}. Each is a Subsystem candidate: source files grouped \
                 by the concept they are named for and what they use, across directories \
                 (Semantic / InferredWeak, derived by rule from the files it cites). A \
                 capability is listed under the area that holds most of its handler files. \
                 Capabilities with no area: {}.\n",
                areas.len(),
                listed.iter().filter(|(area, _)| area.is_none()).count()
            )?;
            out.push_str("| Area | Capabilities | Files | Citation |\n|---|---|---|---|\n")?;
            for (area, count) in &areas {
                let id = citation_id(&FactKey::Node {
                    id: area.id.clone(),
                });
                writeln!(
                    out,
                    "| {} | {count} | {} | {id} |",
                    Literal(area_name(area)),
                    area.props["files"].as_u64().unwrap_or_default(),
                )?;
                if asserted.insert(id) {
                    assertions.node(area, None)?;
                    extra += 1;
                }
            }
            out.push_str("\n| Area | Capability | Operations | Examples | Citation |\n|---|---|---|---|---|\n")?;
        } else {
            out.push_str("| Capability | Operations | Examples | Citation |\n|---|---|---|---|\n")?;
        }
        for (area, story) in &listed {
            let operations: Vec<String> = cited_endpoints(story)
                .map(|n| NodeName(n).to_string())
                .collect();
            let shown: Vec<&str> = operations.iter().take(3).map(String::as_str).collect();
            let more = operations.len() - shown.len();
            if arranged {
                write!(
                    out,
                    "| {} ",
                    Literal(area.map_or(NO_AREA, |node| area_name(node)))
                )?;
            }
            writeln!(
                out,
                "| {} | {} | {}{} | {} |",
                Literal(NodeName(story.node)),
                operations.len(),
                Literal(shown.join("; ")),
                if more > 0 {
                    format!("; and {more} more")
                } else {
                    String::new()
                },
                citation_id(&FactKey::Node {
                    id: story.node.id.clone(),
                })
            )?;
        }
    }
    Ok(stories.len() + extra)
}

pub(super) fn d1(
    (nodes, edges): (&[&Node], &[&Edge]),
    flows: &[Flow],
    gaps: usize,
    (curated, graph_content_hash): (Curated<'_>, Option<&str>),
    knowledge: Option<&Stories<'_>>,
    (mode, rejected): (ExportMode, &BTreeSet<String>),
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

    // A draft may cite a derived Capability or Actor this document prints
    // (#2040), so the included stories are looked up beside the base facts.
    let citable: Vec<&Node> = match knowledge {
        Some(knowledge) => nodes
            .iter()
            .copied()
            .chain(knowledge.included.iter().map(|story| story.node))
            .collect(),
        None => nodes.to_vec(),
    };
    let facts = match mode {
        ExportMode::BestEffort => cited_facts(&drafts, &citable, edges),
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
        // §1.5 states the source no rule evidence was read from, arranged
        // per capability or not (#2009).
        if section.subject == Some("BusinessRule")
            && let Some(unread) = crate::rules::unread_rule_sentence(
                &crate::rules::unread_rule_languages(nodes.iter().copied()),
            )
        {
            content.push_str(&unread)?;
        }
        if let Some(source) = section.frame {
            // The knowledge layer's derived Capabilities and Actor caller
            // sets frame 1.2 and 1.3 beside the base facts (#1443).
            let derived = match source {
                SectionFrame::Nodes(label) => knowledge.map_or(0, |k| {
                    k.included.iter().filter(|s| s.node.label == label).count()
                }),
                _ => 0,
            };
            let base = match source {
                SectionFrame::Nodes(label) => framed(nodes, label).len(),
                _ => 0,
            };
            let arranged = matches!(source, SectionFrame::Artifacts(_))
                && knowledge
                    .is_some_and(|k| k.included.iter().any(|s| s.node.label == "Capability"));
            // §1.4 arranged per capability (AC-0850); with no requested
            // operation it stays a pointer.
            let (scenarios, scenario_facts) = match knowledge {
                Some(knowledge) if arranged && section.subject == Some("Capability") => {
                    acceptance::acceptance_per_capability(
                        (nodes, edges),
                        knowledge,
                        (&mut assertions, &mut drafted.asserted),
                        &mut content,
                    )?
                }
                _ => (0, 0),
            };
            if scenarios > 0 {
                cited += scenario_facts;
            } else if arranged
                && section.subject == Some("BusinessRule")
                && let Some(knowledge) = knowledge
            {
                // Arranged per capability (#1443); the inventory keeps every
                // observation with its dependencies.
                content.push_str(
                    "Every rule observation, with its tier, confidence, citation and \
                     dependencies, is in `rule-evidence.md` of this export.\n",
                )?;
                cited += rules::rules_per_capability(
                    (nodes, edges),
                    knowledge,
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
                cited += operations::limits_per_capability(
                    knowledge,
                    mode,
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
            } else if let (SectionFrame::Flows, Some(knowledge)) = (source, knowledge)
                && let Some(flow_facts) = flows::flows_per_capability(
                    (nodes, edges),
                    flows,
                    knowledge,
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?
            {
                // What each operation's flow does, per capability (#1985).
                cited += flow_facts;
            } else if derived == 0 || base > 0 {
                cited += frame(
                    section,
                    source,
                    (nodes, flows, gaps),
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
            }
            let mut actions_listed = false;
            if let (SectionFrame::Nodes(label), Some(knowledge)) = (source, knowledge)
                && derived > 0
            {
                cited += derived_frame(
                    label,
                    knowledge,
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
                if label == "Capability" {
                    cited += operations::operations_per_capability(
                        (nodes, edges),
                        knowledge,
                        mode,
                        (&mut assertions, &mut drafted.asserted),
                        &mut content,
                    )?;
                    // The user actions that reach each capability (#2005).
                    cited += actions::actions_per_capability(
                        (nodes, edges),
                        knowledge,
                        (&mut assertions, &mut drafted.asserted),
                        &mut content,
                    )?;
                    actions_listed = true;
                }
            }
            // With no derived Capability, the actions whose target is not
            // recovered are still listed (AC-0976).
            if matches!(source, SectionFrame::Nodes("Capability")) && !actions_listed {
                cited += actions::unresolved_actions_only(
                    (nodes, edges),
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
            }
            // §1.3 states the operations no Capability groups, and why (#2007).
            if matches!(source, SectionFrame::Nodes("Capability"))
                && let Some(sentence) = uncovered_endpoints((nodes, edges), knowledge)
            {
                content.push_str(&sentence)?;
            }
            // Python callers by their recovered guards frame 1.2 (#1979).
            if matches!(source, SectionFrame::Nodes("Actor")) {
                let mut guards = BTreeSet::new();
                cited += super::endpoint_permissions::callers_frame(
                    nodes,
                    (mode, rejected),
                    (&mut assertions, &mut drafted.asserted),
                    &mut guards,
                    &mut content,
                )?;
                cited += rules::guard_checks(
                    (nodes, edges),
                    &guards,
                    (&mut assertions, &mut drafted.asserted),
                    &mut content,
                )?;
            }
        }
        match section.kind {
            SectionKind::Projected => {}
            SectionKind::Framed => content.push_str("\n")?,
            SectionKind::Authored if section.number == "1.1" => content.push_str(AT_A_GLANCE)?,
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

#[path = "document_rules.rs"]
pub(super) mod rules;

#[path = "document_acceptance.rs"]
pub(super) mod acceptance;

#[path = "document_flows.rs"]
pub(super) mod flows;

#[path = "document_operations.rs"]
pub(super) mod operations;

#[path = "document_actions.rs"]
pub(super) mod actions;

#[path = "document_statements.rs"]
pub(super) mod statements;

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "document_draft_tests.rs"]
mod draft_tests;

#[cfg(test)]
#[path = "document_area_tests.rs"]
mod area_tests;
