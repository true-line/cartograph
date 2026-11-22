//! Found/recovered ADR facts and deterministic ADR/code drift (US-0013).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use flowtracer::Flow;
use serde::{Deserialize, Serialize};

mod bounded;
pub use bounded::{AdrLimit, AdrLimitExceeded, AdrLimits};
use bounded::{
    Budget, DriftProps, EdgeView, NodeView, OnlyProvenance, ProvenanceView, RecoveredProps, Text,
    canonical_hash, text_hash,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Deterministic extractor id for Markdown ADR/RFC files.
pub const FOUND_ADR_EXTRACTOR_ID: &str = "t0.adr-markdown";
/// Largest found ADR/RFC file the reader stores, in bytes. A larger file is
/// not read and is reported as [`UnreadAdrReason::TooLarge`] (#1847).
pub const MAX_FOUND_ADR_BYTES: u64 = 1024 * 1024;
/// Local semantic projection id for recovered decision drafts.
pub const RECOVERED_ADR_EXTRACTOR_ID: &str = "t2.adr-recovery";

/// ADR-related graph facts produced by extraction or compilation.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct AdrFacts {
    /// Found/recovered ADR and Drift nodes.
    pub nodes: Vec<Node>,
    /// DECIDES/CONFLICTS/DRIFTS_FROM edges.
    pub edges: Vec<Edge>,
}

/// Why a found ADR/RFC file was not read (#1847). No fact from it joined the
/// graph, and the coverage inventory names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreadAdrReason {
    /// Larger than [`MAX_FOUND_ADR_BYTES`].
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
}

/// One found ADR/RFC file the reader did not read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct UnreadAdr {
    /// Repo-relative path.
    pub path: String,
    /// Why it was not read.
    pub reason: UnreadAdrReason,
}

/// What one found-ADR read recovered and what it could not read (#1847).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct FoundAdrRead {
    /// ADR nodes and DECIDES edges.
    pub facts: AdrFacts,
    /// ADR files read.
    pub files: u64,
    /// ADR files not read, in path order.
    pub unread: Vec<UnreadAdr>,
}

/// Whether a Markdown file at `rel` (named `name`) is a decision record: it
/// sits under a decision directory or is named like one. The documentation
/// walk asks this of every Markdown file it reads (ADR-0065 decision 6).
pub(crate) fn is_decision_record(rel: &str, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if !name.ends_with(".md") {
        return false;
    }
    let in_decision_dir = rel.split('/').any(|component| {
        matches!(
            component.to_ascii_lowercase().as_str(),
            "adr" | "adrs" | "decision" | "decisions" | "rfc" | "rfcs"
        )
    });
    in_decision_dir || name.starts_with("adr-") || name.starts_with("rfc-")
}

fn heading(markdown: &str, fallback: &str) -> String {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn field(markdown: &str, name: &str) -> Option<String> {
    let needle = format!("{}:", name.to_ascii_lowercase());
    markdown.lines().find_map(|line| {
        let normalized = line
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .replace("**", "");
        let lower = normalized.to_ascii_lowercase();
        lower
            .starts_with(&needle)
            .then(|| normalized[needle.len()..].trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn list_field(markdown: &str, name: &str) -> Vec<String> {
    field(markdown, name)
        .into_iter()
        .flat_map(|value| {
            value
                .split([',', ';'])
                .map(|item| {
                    item.trim()
                        .trim_matches(['`', '[', ']', '(', ')', ' '])
                        .to_string()
                })
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn field_target_span(markdown: &str, name: &str, target: &str) -> Option<(usize, usize)> {
    let needle = format!("{}:", name.to_ascii_lowercase());
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let normalized = line
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .replace("**", "");
        if normalized.to_ascii_lowercase().starts_with(&needle) {
            let start = line.find(target).map(|local| offset + local)?;
            return Some((start, start + target.len()));
        }
        offset += line.len();
    }
    None
}

fn inline_code(markdown: &str) -> Vec<(String, usize, usize)> {
    let bytes = markdown.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(open) = markdown[cursor..].find('`').map(|offset| cursor + offset) else {
            break;
        };
        let Some(close) = markdown[open + 1..]
            .find('`')
            .map(|offset| open + 1 + offset)
        else {
            break;
        };
        if close > open + 1 {
            spans.push((markdown[open + 1..close].to_string(), open + 1, close));
        }
        cursor = close + 1;
    }
    spans
}

fn evidence(repo: &str, path: &str, commit: &str, start: usize, end: usize) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: commit.into(),
    }
}

/// Parse Markdown ADR/RFC files under `root` and confirm only governed target
/// ids that are explicitly declared with `Governs:` or cited in backticks.
pub fn extract_found_adrs(
    root: &Path,
    repo: &str,
    commit: &str,
    candidates: &[Node],
) -> std::io::Result<AdrFacts> {
    let ids: BTreeSet<&str> = candidates.iter().map(|node| node.id.as_str()).collect();
    extract_found_adrs_among(root, repo, commit, |id| ids.contains(id))
}

/// [`extract_found_adrs`] that asks `is_candidate` whether an id names a
/// recovered fact, so a caller holding packed facts never decodes them: a
/// governed target is matched by id alone (#1574).
pub fn extract_found_adrs_among(
    root: &Path,
    repo: &str,
    commit: &str,
    is_candidate: impl Fn(&str) -> bool,
) -> std::io::Result<AdrFacts> {
    read_found_adrs_among(root, repo, commit, is_candidate).map(|read| read.facts)
}

/// [`extract_found_adrs_among`] that also reports how many ADR files it read
/// and which it skipped, so the coverage inventory can name them (#1847).
///
/// The stored `body`, `title` and `status` pass the shared free-text
/// redaction ([`core_redact::redact_text`]) before they enter the graph, as
/// every other reader that stores target text does; a node whose text was
/// redacted carries the replacement count as `redactions`. Evidence spans
/// still cite the file's own bytes.
pub fn read_found_adrs_among(
    root: &Path,
    repo: &str,
    commit: &str,
    is_candidate: impl Fn(&str) -> bool,
) -> std::io::Result<FoundAdrRead> {
    let cases = crate::DocumentationCases {
        documents: false,
        api_references: &[],
        decisions: Some(&is_candidate),
    };
    crate::read_documentation(root, repo, commit, &cases).map(|read| read.decisions)
}

/// Record a found decision file the documentation walk could not read.
pub(crate) fn skip_found_adr(read: &mut FoundAdrRead, path: &str, reason: UnreadAdrReason) {
    read.unread.push(UnreadAdr {
        path: path.to_string(),
        reason,
    });
}

/// Read one found ADR/RFC file the documentation walk has read as
/// `markdown`: its `ADR` node and the `DECIDES` edges of the ids it governs.
pub(crate) fn read_found_adr(
    read: &mut FoundAdrRead,
    repo: &str,
    commit: &str,
    relative: &str,
    markdown: &str,
    is_candidate: &dyn Fn(&str) -> bool,
) {
    let facts = &mut read.facts;
    read.files += 1;
    // Stored text is redacted; identity, spans and governed ids are read
    // from the file as written.
    let (body, mut redactions) = core_redact::redact_text(markdown);
    let file_name = relative.rsplit('/').next().unwrap_or(relative);
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled decision");
    let (title, title_redactions) = core_redact::redact_text(&heading(markdown, stem));
    let (status, status_redactions) =
        core_redact::redact_text(&field(markdown, "Status").unwrap_or_else(|| "Found".into()));
    redactions += title_redactions + status_redactions;
    let adr_id = format!("adr:{repo}@{relative}");
    let file_evidence = evidence(repo, relative, commit, 0, markdown.len());
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![file_evidence.clone()],
        FOUND_ADR_EXTRACTOR_ID,
        serde_json::to_vec(&(&adr_id, markdown))
            .expect("ADR identity serializes")
            .as_slice(),
    )
    .expect("Confirmed is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(FOUND_ADR_EXTRACTOR_ID));
    let forbids = list_field(markdown, "Forbids")
        .into_iter()
        .filter(|label| {
            label
                .chars()
                .all(|character| character.is_ascii_uppercase() || character == '_')
        })
        .collect::<Vec<_>>();
    let mut props = serde_json::json!({
        "title": title,
        "status": status,
        "body": body,
        "path": relative,
        "origin": "found",
        "recovered": false,
        "forbids": forbids,
        "prov": provenance,
    });
    // Present only when something was replaced, so an ADR without a
    // recognized secret keeps the fact it had before #1847.
    if redactions > 0 {
        props["redactions"] = redactions.into();
    }
    facts.nodes.push(Node {
        id: adr_id.clone(),
        label: "ADR".into(),
        props,
    });

    let mut mentions: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for target in list_field(markdown, "Governs") {
        if is_candidate(target.as_str())
            && let Some(span) = field_target_span(markdown, "Governs", &target)
        {
            mentions.insert(target, span);
        }
    }
    for (target, start, end) in inline_code(markdown) {
        if is_candidate(target.as_str()) {
            mentions.entry(target).or_insert((start, end));
        }
    }
    for (target, (start, end)) in mentions {
        let edge_provenance = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence(repo, relative, commit, start, end)],
            FOUND_ADR_EXTRACTOR_ID,
            format!("{adr_id} DECIDES {target}").as_bytes(),
        )
        .expect("Confirmed is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(FOUND_ADR_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: adr_id.clone(),
            dst: target,
            label: "DECIDES".into(),
            props: serde_json::json!({"prov": edge_provenance}),
        });
    }
}

/// Put a found-ADR read in its stored order.
pub(crate) fn finish_found_adrs(read: &mut FoundAdrRead) {
    read.facts
        .nodes
        .sort_by(|left, right| left.id.cmp(&right.id));
    read.facts.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    read.unread.sort();
}

fn typed_provenance(value: &serde_json::Value) -> Option<Provenance> {
    Provenance::deserialize(&*crate::stored_prov(value))
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

type EvidenceKey<'a> = (&'a str, &'a str, u64, u64, &'a str);
fn evidence_key(evidence: &EvidenceRef) -> EvidenceKey<'_> {
    (
        &evidence.repo,
        &evidence.path,
        evidence.byte_start,
        evidence.byte_end,
        &evidence.commit_sha,
    )
}

// Sort/deduplicate borrowed spans. Charge a lower bound on their eventual output
// before each map insertion, so a rejected candidate never clones large evidence.
fn combined_evidence<'a>(
    provenances: impl IntoIterator<Item = &'a Provenance>,
    budget: &Budget,
) -> Result<Vec<&'a EvidenceRef>, AdrLimitExceeded> {
    let mut evidence = BTreeMap::new();
    let mut counter = budget.fragment_budget();
    for provenance in provenances {
        for item in &provenance.evidence {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                evidence.entry(evidence_key(item))
            {
                budget.fragment(&mut counter, item)?;
                entry.insert(item);
            }
        }
    }
    Ok(evidence.into_values().collect())
}

fn recovered_adrs(
    nodes: &[&Node],
    edges: &[&Edge],
    budget: &mut Budget,
) -> Result<AdrFacts, AdrLimitExceeded> {
    let governed: BTreeSet<&str> = edges
        .iter()
        .filter(|edge| edge.label == "DECIDES")
        .filter(|edge| {
            nodes.iter().any(|node| {
                node.id == edge.src
                    && node.label == "ADR"
                    && node.props["origin"].as_str() == Some("found")
            })
        })
        .map(|edge| edge.dst.as_str())
        .collect();
    let mut facts = AdrFacts::default();
    for channel in nodes.iter().filter(|node| node.label == "Channel") {
        if governed.contains(channel.id.as_str()) {
            continue;
        }
        // Typed provenance is parsed only for edges into this channel; an
        // edge has one target, so each is parsed at most once (#725).
        let related: Vec<_> = edges
            .iter()
            .filter(|edge| {
                edge.dst == channel.id
                    && matches!(edge.label.as_str(), "PUBLISHES" | "SUBSCRIBES" | "BACKS")
            })
            .filter_map(|edge| {
                typed_provenance(&edge.props)
                    .filter(|p| p.confidence_tier != ConfidenceTier::Gap)
                    .map(|p| (edge, p))
            })
            .collect();
        if related.is_empty() {
            continue;
        }
        let channel_provenance = typed_provenance(&channel.props);
        if !related.iter().any(|(_, p)| !p.evidence.is_empty())
            && channel_provenance
                .as_ref()
                .is_none_or(|p| p.evidence.is_empty())
        {
            continue;
        }
        budget.ensure_rows(2)?;
        let confidence = if related
            .iter()
            .any(|(_, p)| p.confidence_tier == ConfidenceTier::InferredWeak)
        {
            ConfidenceTier::InferredWeak
        } else {
            ConfidenceTier::InferredStrong
        };
        let cited = combined_evidence(
            related
                .iter()
                .map(|(_, p)| p)
                .chain(channel_provenance.iter()),
            budget,
        )?;
        let mut edge_keys: Vec<_> = related
            .iter()
            .map(|(edge, _)| (&edge.src, &edge.label, &edge.dst))
            .collect();
        edge_keys.sort();
        let hash = canonical_hash(&("async-messaging", &channel.id, edge_keys));
        let adr_id = format!("adr:recovered:async:{}", &hash[..16]);
        let name = channel.props["identity"]
            .as_str()
            .or_else(|| channel.props["name"].as_str())
            .unwrap_or(&channel.id);
        let provenance = ProvenanceView::new(
            Tier::Semantic,
            confidence,
            &cited,
            RECOVERED_ADR_EXTRACTOR_ID,
            &hash,
        );
        let node = NodeView {
            id: &adr_id,
            label: "ADR",
            props: RecoveredProps {
                title: Text::RecoveredTitle(name),
                status: "Recovered / Inferred",
                body: Text::RecoveredBody(&channel.id),
                origin: "recovered",
                recovered: true,
                prov: &provenance,
            },
        };
        let edge_hash = text_hash(&Text::OffendingEdge {
            src: &adr_id,
            label: "DECIDES",
            dst: &channel.id,
        });
        let edge_provenance = ProvenanceView::new(
            Tier::Semantic,
            confidence,
            &cited,
            RECOVERED_ADR_EXTRACTOR_ID,
            &edge_hash,
        );
        let edge = EdgeView {
            src: &adr_id,
            dst: &channel.id,
            label: "DECIDES",
            props: OnlyProvenance {
                prov: &edge_provenance,
            },
        };
        budget.admit(&[&node], &[&edge])?;
        facts.nodes.push(node.into_owned());
        facts.edges.push(edge.into_owned());
    }
    Ok(facts)
}

fn drift_facts(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    budget: &mut Budget,
) -> Result<AdrFacts, AdrLimitExceeded> {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
    let mut facts = AdrFacts::default();
    // Typed provenance is parsed only for found ADRs and the edges they
    // forbid, not for the whole graph (#725).
    for adr in nodes
        .iter()
        .filter(|node| node.label == "ADR" && node.props["origin"].as_str() == Some("found"))
    {
        let forbidden: BTreeSet<_> = adr.props["forbids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        if forbidden.is_empty() {
            continue;
        }
        let adr_provenance = typed_provenance(&adr.props);
        let governed: BTreeSet<_> = edges
            .iter()
            .filter(|edge| edge.label == "DECIDES" && edge.src == adr.id)
            .map(|edge| edge.dst.as_str())
            .collect();
        for edge in edges.iter().filter(|edge| {
            forbidden.contains(edge.label.as_str())
                && (governed.contains(edge.src.as_str()) || governed.contains(edge.dst.as_str()))
        }) {
            let edge_provenance = typed_provenance(&edge.props);
            let Some(edge_provenance) = edge_provenance
                .as_ref()
                .filter(|p| p.confidence_tier != ConfidenceTier::Gap)
            else {
                continue;
            };
            let Some(adr_provenance) = adr_provenance.as_ref() else {
                continue;
            };
            if adr_provenance.evidence.is_empty() && edge_provenance.evidence.is_empty() {
                continue;
            }
            let targets: Vec<_> = [
                ("DRIFTS_FROM", adr.id.as_str()),
                ("CONFLICTS", edge.dst.as_str()),
            ]
            .into_iter()
            .filter(|(_, target)| by_id.contains_key(target))
            .collect();
            budget.ensure_rows(1 + targets.len())?;
            let cited = combined_evidence([adr_provenance, edge_provenance], budget)?;
            let hash = canonical_hash(&("adr-drift", &adr.id, &edge.src, &edge.label, &edge.dst));
            let drift_id = format!("drift:{}", &hash[..16]);
            let mut triggers = BTreeSet::new();
            let mut counter = budget.fragment_budget();
            for flow in flows.iter().filter(|flow| {
                flow.hops.iter().any(|hop| {
                    hop.src == edge.src && hop.label == edge.label && hop.dst == edge.dst
                })
            }) {
                if !triggers.contains(flow.trigger.as_str()) {
                    budget.fragment(&mut counter, &flow.trigger)?;
                    triggers.insert(flow.trigger.as_str());
                }
            }
            let flow_triggers: Vec<_> = triggers.into_iter().collect();
            let adr_title = adr.props["title"].as_str().unwrap_or(&adr.id);
            let tier = match edge_provenance.confidence_tier {
                ConfidenceTier::Confirmed => edge_provenance.tier,
                ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak => Tier::Semantic,
                ConfidenceTier::Gap => unreachable!("gap facts were filtered above"),
            };
            let extractor = match tier {
                Tier::Deterministic => "t0.adr-drift",
                Tier::Dynamic => "t1.adr-drift",
                Tier::Semantic => "t2.adr-drift",
                Tier::Agentic => "t3.adr-drift",
            };
            let provenance = ProvenanceView::new(
                tier,
                edge_provenance.confidence_tier,
                &cited,
                extractor,
                &hash,
            );
            let node = NodeView {
                id: &drift_id,
                label: "Drift",
                props: DriftProps {
                    kind: "drift",
                    title: Text::DriftTitle {
                        adr: adr_title,
                        label: &edge.label,
                        src: &edge.src,
                        dst: &edge.dst,
                    },
                    adr_id: &adr.id,
                    offending_edge: Text::OffendingEdge {
                        label: &edge.label,
                        src: &edge.src,
                        dst: &edge.dst,
                    },
                    flow_triggers: &flow_triggers,
                    prov: &provenance,
                },
            };
            let derived_edges: Vec<_> = targets
                .iter()
                .map(|(label, target)| EdgeView {
                    src: &drift_id,
                    dst: target,
                    label,
                    props: OnlyProvenance { prov: &provenance },
                })
                .collect();
            budget.admit(&[&node], &derived_edges)?;
            facts.nodes.push(node.into_owned());
            facts
                .edges
                .extend(derived_edges.into_iter().map(EdgeView::into_owned));
        }
    }
    Ok(facts)
}

/// ADR facts over borrowed graph facts, so a caller holding a projection of
/// references never clones the graph to derive them (#725).
pub(crate) fn derive_inner(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    limits: Option<AdrLimits>,
) -> Result<AdrFacts, AdrLimitExceeded> {
    let mut budget = Budget::new(limits)?;
    let mut recovered = recovered_adrs(nodes, edges, &mut budget)?;
    let drift = {
        let projected_nodes: Vec<&Node> = nodes.iter().copied().chain(&recovered.nodes).collect();
        let projected_edges: Vec<&Edge> = edges.iter().copied().chain(&recovered.edges).collect();
        drift_facts(&projected_nodes, &projected_edges, flows, &mut budget)?
    };
    recovered.nodes.extend(drift.nodes);
    recovered.edges.extend(drift.edges);
    recovered
        .nodes
        .sort_by(|left, right| left.id.cmp(&right.id));
    recovered.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    Ok(recovered)
}

/// Add evidence-backed recovered ADR drafts and explicit ADR/code drift to a
/// disposable compilation projection. Confirmed graph input is never mutated.
pub fn derive_adr_facts(nodes: &[Node], edges: &[Edge], flows: &[Flow]) -> AdrFacts {
    derive_inner(&refs(nodes), &refs(edges), flows, None)
        .expect("unbounded ADR derivation has no admission limit")
}

/// Derive complete ADR facts within cumulative row and exact serialized output
/// limits. Inputs must be admitted separately; a failure returns no partial facts.
/// Every nonlinear occurrence is admitted before owning output evidence, strings
/// or JSON. The ordinary API shares this exact derivation and ordering.
pub fn derive_adr_facts_bounded(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    limits: AdrLimits,
) -> Result<AdrFacts, AdrLimitExceeded> {
    derive_inner(&refs(nodes), &refs(edges), flows, Some(limits))
}

fn refs<T>(facts: &[T]) -> Vec<&T> {
    facts.iter().collect()
}

pub(crate) fn logical_workspace_bytes(
    bytes: usize,
    rows: usize,
    values: usize,
    trace: flowtracer::TraceLimits,
    limits: AdrLimits,
) -> Option<usize> {
    use crate::workbench::accounting::{json, json_bytes, map_slots, set_slots, slots, sum};
    let projected_bytes = bytes.checked_add(limits.max_output_bytes)?;
    let projected_rows = rows.checked_add(limits.max_rows)?;
    let projected_values = values.checked_add(limits.max_output_bytes)?;
    sum(&[
        // An upper bound: derive_inner now borrows the base graph and
        // recovered rows for drift (#725) rather than cloning them.
        json(projected_bytes, projected_values)?,
        slots::<Node>(projected_rows)?,
        slots::<Edge>(projected_rows)?,
        json_bytes(projected_bytes)?, // once-per-row typed provenance arrays
        slots::<Option<Provenance>>(projected_rows)?,
        map_slots::<&str, &Node>(projected_rows)?,
        set_slots::<&str>(projected_rows)?,   // governed targets
        set_slots::<&str>(projected_values)?, // forbidden labels
        slots::<(&Edge, &Provenance)>(projected_rows)?,
        slots::<(&String, &String, &String)>(projected_rows)?,
        map_slots::<EvidenceKey<'_>, &EvidenceRef>(projected_values)?,
        slots::<&EvidenceRef>(projected_values)?,
        set_slots::<&str>(trace.max_flows)?,
        slots::<&str>(trace.max_flows)?, // triggers
        slots::<(&str, &str)>(2)?,       // literal drift targets
        slots::<EdgeView<'_, OnlyProvenance<'_>>>(2)?,
        slots::<Node>(limits.max_rows)?,
        slots::<Edge>(limits.max_rows)?, // final sort scratch, one at a time but sum named roles
        json_bytes(projected_bytes)?,    // one current input parse/identity view
        std::mem::size_of::<Budget>(),
        std::mem::size_of::<AdrFacts>(),
    ])
}

#[cfg(test)]
#[path = "adr/bounded_tests.rs"]
mod bounded_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(path: &str, fact: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![evidence("local/shop", path, "abc123", 0, 10)],
                "t0.test",
                fact.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn weak_prov(path: &str, fact: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Semantic,
                ConfidenceTier::InferredWeak,
                vec![evidence("local/shop", path, "abc123", 0, 10)],
                "t2.test",
                fact.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn found_adr_links_only_explicit_existing_targets() {
        // AC-0036 (T-0036): found Markdown decisions and governed targets are
        // Confirmed with exact file/span provenance.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(
            dir.path().join("docs/adr/ADR-0001-orders.md"),
            "# Queue orders\n\nThe chan:orders identifier is discussed here.\n\n- **Status:** Accepted\n- **Governs:** `chan:orders`\n- **Forbids:** CALLS\n",
        )
        .unwrap();
        let targets = vec![Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "channel")}),
        }];
        let facts = extract_found_adrs(dir.path(), "local/shop", "abc123", &targets).unwrap();
        assert_eq!(facts.nodes.len(), 1);
        assert_eq!(facts.edges.len(), 1);
        assert_eq!(facts.edges[0].label, "DECIDES");
        assert_eq!(facts.edges[0].dst, "chan:orders");
        assert_eq!(facts.nodes[0].props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(facts.nodes[0].props["forbids"][0], "CALLS");
        let expected_start =
            std::fs::read_to_string(dir.path().join("docs/adr/ADR-0001-orders.md"))
                .unwrap()
                .find("`chan:orders`")
                .unwrap()
                + 1;
        assert_eq!(
            facts.edges[0].props["prov"]["evidence"][0]["byte_start"],
            expected_start as u64
        );
    }

    #[test]
    fn found_adr_text_is_redacted_and_unreadable_files_are_reported() {
        // AC-0685 (T-0685, #1847): stored ADR text passes the shared
        // redaction; a too-large or non-UTF-8 file is skipped and reported,
        // and the other ADRs are still read.
        let dir = tempfile::tempdir().unwrap();
        let adr = dir.path().join("docs/adr");
        std::fs::create_dir_all(&adr).unwrap();
        let secret = "sk-abcdefghijklmnop1234";
        std::fs::write(
            adr.join("ADR-0001-keys.md"),
            format!(
                "# Rotate keys\n\n- **Status:** Accepted\n- **Governs:** `chan:orders`\n\nUse api_key={secret} until rotation.\n"
            ),
        )
        .unwrap();
        std::fs::write(
            adr.join("ADR-0002-clean.md"),
            "# Clean\n\nNothing secret.\n",
        )
        .unwrap();
        std::fs::write(adr.join("ADR-0003-latin1.md"), b"# Caf\xe9\n").unwrap();
        std::fs::write(
            adr.join("ADR-0004-huge.md"),
            "x".repeat(MAX_FOUND_ADR_BYTES as usize + 1),
        )
        .unwrap();
        let read =
            read_found_adrs_among(dir.path(), "local/shop", "abc123", |id| id == "chan:orders")
                .unwrap();
        assert_eq!(read.files, 2);
        assert_eq!(
            read.unread,
            vec![
                UnreadAdr {
                    path: "docs/adr/ADR-0003-latin1.md".into(),
                    reason: UnreadAdrReason::NotUtf8,
                },
                UnreadAdr {
                    path: "docs/adr/ADR-0004-huge.md".into(),
                    reason: UnreadAdrReason::TooLarge,
                },
            ]
        );
        assert_eq!(read.facts.nodes.len(), 2);
        let keys = &read.facts.nodes[0];
        let stored = serde_json::to_string(&keys.props).unwrap();
        assert!(!stored.contains(secret), "secret stored: {stored}");
        assert!(keys.props["body"].as_str().unwrap().contains("[REDACTED"));
        assert!(keys.props["redactions"].as_u64().unwrap() >= 1);
        // Spans and governed links still cite the file's own bytes.
        let raw = std::fs::read_to_string(adr.join("ADR-0001-keys.md")).unwrap();
        assert_eq!(
            keys.props["prov"]["evidence"][0]["byte_end"],
            raw.len() as u64
        );
        assert_eq!(read.facts.edges.len(), 1);
        assert_eq!(read.facts.edges[0].dst, "chan:orders");
        // An ADR with nothing to redact keeps the props it had before.
        let clean = &read.facts.nodes[1];
        assert_eq!(clean.props["body"], "# Clean\n\nNothing secret.\n");
        assert!(clean.props.get("redactions").is_none());
    }

    #[test]
    fn recovered_adrs_and_drift_are_cited_and_mapped() {
        // AC-0037/AC-0038 (T-0037/T-0038): recovered ADRs remain inferred;
        // explicit ADR/code conflicts name the offending edge and flow.
        let channel = Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"identity": "orders", "prov": prov("publisher.ts", "channel")}),
        };
        let producer = Node {
            id: "sym:publish".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("publisher.ts", "producer")}),
        };
        let found = Node {
            id: "adr:found:no-sync".into(),
            label: "ADR".into(),
            props: serde_json::json!({
                "title": "No synchronous calls",
                "origin": "found",
                "forbids": ["CALLS"],
                "prov": prov("docs/adr/no-sync.md", "adr")
            }),
        };
        let target = Node {
            id: "sym:handler".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("handler.ts", "handler")}),
        };
        let callee = Node {
            id: "sym:remote".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("remote.ts", "remote")}),
        };
        let edges = vec![
            Edge {
                src: producer.id.clone(),
                dst: channel.id.clone(),
                label: "PUBLISHES".into(),
                props: serde_json::json!({"prov": prov("publisher.ts", "publishes")}),
            },
            Edge {
                src: found.id.clone(),
                dst: target.id.clone(),
                label: "DECIDES".into(),
                props: serde_json::json!({"prov": prov("docs/adr/no-sync.md", "decides")}),
            },
            Edge {
                src: target.id.clone(),
                dst: callee.id.clone(),
                label: "CALLS".into(),
                props: serde_json::json!({"prov": prov("handler.ts", "calls")}),
            },
        ];
        let flow = Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![flowtracer::Hop {
                label: "CALLS".into(),
                src: target.id.clone(),
                dst: callee.id.clone(),
                src_name: "handler".into(),
                dst_name: "remote".into(),
                tier: "Deterministic".into(),
                confidence: "Confirmed".into(),
                evidence: Some("handler.ts bytes 0..10".into()),
                provenance: serde_json::from_value(prov("handler.ts", "calls")).unwrap(),
                gap_reason: None,
                attempted_tiers: vec![],
            }],
            status: flowtracer::FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        };
        let facts = derive_adr_facts(&[channel, producer, found, target, callee], &edges, &[flow]);
        let recovered = facts
            .nodes
            .iter()
            .find(|node| node.props["origin"] == "recovered")
            .unwrap();
        assert_eq!(recovered.props["prov"]["tier"], "Semantic");
        assert_eq!(recovered.props["prov"]["confidence_tier"], "InferredStrong");
        // AC-0244 (#601 review): a rule-derived semantic candidate names its
        // versioned rule as the producing method, apart from tier.
        assert_eq!(
            recovered.props["prov"]["method"],
            serde_json::json!({"kind": "rule", "rule": RECOVERED_ADR_EXTRACTOR_ID, "version": crate::SPEC_RULE_VERSION})
        );
        let decides = facts
            .edges
            .iter()
            .find(|edge| edge.src == recovered.id && edge.label == "DECIDES")
            .unwrap();
        assert_eq!(decides.props["prov"]["method"]["kind"], "rule");
        let drift = facts
            .nodes
            .iter()
            .find(|node| node.label == "Drift")
            .unwrap();
        assert_eq!(drift.props["prov"]["method"]["rule"], "t0.adr-drift");
        assert_eq!(
            drift.props["offending_edge"],
            "sym:handler CALLS sym:remote"
        );
        assert_eq!(drift.props["flow_triggers"][0], "ep:orders");
        assert!(
            facts
                .edges
                .iter()
                .any(|edge| edge.src == drift.id && edge.label == "CONFLICTS")
        );
    }

    #[test]
    fn recovered_adr_never_upgrades_weak_support() {
        // AC-0037 (T-0037): a recovered decision inherits the weakest
        // supporting confidence instead of making an inferred edge stronger.
        let channel = Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "channel")}),
        };
        let producer = Node {
            id: "sym:publish".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "producer")}),
        };
        let edge = Edge {
            src: producer.id.clone(),
            dst: channel.id.clone(),
            label: "PUBLISHES".into(),
            props: serde_json::json!({"prov": weak_prov("orders.ts", "publishes")}),
        };

        let facts = derive_adr_facts(&[channel, producer], &[edge], &[]);
        let recovered = facts
            .nodes
            .iter()
            .find(|node| node.props["origin"] == "recovered")
            .unwrap();
        assert_eq!(recovered.props["prov"]["tier"], "Semantic");
        assert_eq!(recovered.props["prov"]["confidence_tier"], "InferredWeak");
        assert!(
            facts
                .edges
                .iter()
                .all(|edge| { edge.props["prov"]["confidence_tier"] == "InferredWeak" })
        );
    }
}
