//! Deterministic discovery by identifier, name or text (AC-0232, AC-0233).
//!
//! Discovery finds candidate nodes under a declared, versioned matching
//! policy and returns bounded handles. It never ranks by similarity, never
//! merges same-name hits and never turns a candidate into a binding or a
//! relationship: a candidate only says which node matched, where it came
//! from and why.

use super::{
    ContextError, ContextFact, ContextSnapshot, FactReference, MAX_RESPONSE_BYTES,
    content_identity, validate_limit,
};
use core_prov::ConfidenceTier;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Version of the matching rules below. A cursor issued under one version is
/// rejected under any other.
pub const DISCOVERY_POLICY: &str = "discovery-v1";
/// Maximum returned candidates in a page.
pub const MAX_CANDIDATES: usize = 200;
/// Maximum UTF-8 bytes of a query term.
pub const MAX_TERM_BYTES: usize = 256;
/// Node properties that carry a name under [`DISCOVERY_POLICY`], in the order
/// a match reason reports them.
pub const NAME_FIELDS: &[&str] = &["name", "route", "path", "identity", "title"];

/// What to look for. Every term is matched exactly as the policy states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum DiscoveryQuery {
    /// A node whose identity equals `id`, byte for byte.
    Identifier {
        /// Exact node identity.
        id: String,
    },
    /// Nodes with a name field equal to `name`, byte for byte (case and
    /// whitespace significant).
    Name {
        /// Exact name.
        name: String,
    },
    /// Nodes whose identity or a name field contains `text`, compared after
    /// Unicode lowercasing; `text` is trimmed first.
    Text {
        /// Substring to find.
        text: String,
    },
}

impl DiscoveryQuery {
    /// The query as matched: trimmed and lowercased text, other terms
    /// unchanged. Cursors bind this form.
    fn normalized(&self) -> Result<Self, ContextError> {
        let (term, normalized) = match self {
            Self::Identifier { id } => (id.as_str(), self.clone()),
            Self::Name { name } => (name.as_str(), self.clone()),
            Self::Text { text } => (
                text.as_str(),
                Self::Text {
                    text: text.trim().to_lowercase(),
                },
            ),
        };
        let matched = match &normalized {
            Self::Text { text } => text.as_str(),
            _ => term,
        };
        if matched.is_empty() {
            return Err(ContextError::EmptyDiscoveryTerm);
        }
        if term.len() > MAX_TERM_BYTES {
            return Err(ContextError::InvalidLimit {
                name: "term_bytes",
                requested: term.len(),
                maximum: MAX_TERM_BYTES,
            });
        }
        Ok(normalized)
    }
}

/// How a candidate matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// The node identity equals the identifier.
    ExactIdentifier,
    /// A name field equals the name.
    ExactName,
    /// The identity or a name field contains the text.
    TextSubstring,
}

/// Why one candidate matched: the kind, and the field and value that did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchReason {
    /// Match kind under the policy.
    pub kind: MatchKind,
    /// `id`, or the name field that matched.
    pub field: String,
    /// The matched field's value, as stored.
    pub value: String,
}

/// Where a candidate's fact came from: its primary provenance evidence, or
/// nothing when that provenance could not be validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSource {
    /// Repo of the first evidence reference.
    pub repo: Option<String>,
    /// Repo-relative path of the first evidence reference.
    pub path: Option<String>,
    /// Commit of the first evidence reference.
    pub commit_sha: Option<String>,
}

/// A bounded handle on one matching node. It is not a binding: two
/// same-name nodes from different repos are two candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryCandidate {
    /// The matching node.
    pub reference: FactReference,
    /// Node label.
    pub label: String,
    /// Where the node's fact came from.
    pub source: CandidateSource,
    /// Why it matched.
    pub match_reason: MatchReason,
    /// Stored edges leaving the node.
    pub outgoing_edges: usize,
    /// Stored edges entering the node.
    pub incoming_edges: usize,
    /// Validated confidence, or `Gap` when provenance is unusable.
    pub confidence_tier: ConfidenceTier,
}

/// What the policy examined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryCoverage {
    /// Nodes the policy examined after the label filter.
    pub nodes_examined: usize,
    /// Fields the policy compared, in order.
    pub fields: Vec<String>,
    /// Examined nodes with no name field, so only their identity was
    /// compared.
    pub nodes_without_name_fields: usize,
}

/// Something the policy deliberately does not search, stated so that an
/// empty result is never read as "nothing exists".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryOmission {
    /// What was not searched.
    pub scope: String,
    /// Why.
    pub reason: String,
}

/// A continuation bound to one snapshot, normalized query and policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryCursor {
    /// Snapshot the cursor was issued for.
    pub snapshot_id: String,
    /// Identity of the normalized query and label filter.
    pub query_id: String,
    /// Matching policy version.
    pub policy: String,
    /// Candidates preceding the next page.
    pub offset: usize,
}

/// A bounded discovery request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryRequest {
    /// What to look for.
    pub query: DiscoveryQuery,
    /// Exact node labels to examine; empty examines every node.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Maximum returned candidates, from one through [`MAX_CANDIDATES`].
    pub max_candidates: usize,
    /// Maximum compact JSON response bytes, from one through
    /// [`MAX_RESPONSE_BYTES`].
    pub max_bytes: usize,
    /// Continue an earlier response.
    #[serde(default)]
    pub cursor: Option<DiscoveryCursor>,
}

/// One deterministic page of candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    /// Snapshot searched.
    pub snapshot_id: String,
    /// Matching policy version applied.
    pub policy: String,
    /// Identity of the normalized query and label filter.
    pub query_id: String,
    /// Total candidates across all pages.
    pub total_candidates: usize,
    /// Candidates by match kind, then node identity.
    pub candidates: Vec<DiscoveryCandidate>,
    /// What the policy examined.
    pub coverage: DiscoveryCoverage,
    /// What the policy does not search.
    pub omissions: Vec<DiscoveryOmission>,
    /// Continuation when more candidates remain.
    pub next_cursor: Option<DiscoveryCursor>,
}

#[derive(Serialize)]
struct QueryIdentity<'a> {
    policy: &'a str,
    query: &'a DiscoveryQuery,
    labels: &'a BTreeSet<String>,
}

fn omissions() -> Vec<DiscoveryOmission> {
    vec![
        DiscoveryOmission {
            scope: "edge facts".into(),
            reason: "discovery-v1 returns nodes only; read a candidate's edges with a \
                     neighborhood query"
                .into(),
        },
        DiscoveryOmission {
            scope: "properties outside the policy fields".into(),
            reason: "discovery-v1 compares the node identity and its name fields only".into(),
        },
        DiscoveryOmission {
            scope: "source file contents".into(),
            reason: "discovery reads the recovered graph, never evidence files".into(),
        },
    ]
}

/// The first policy match for `fact`, if any.
fn match_fact(query: &DiscoveryQuery, fact: &ContextFact, id: &str) -> Option<MatchReason> {
    let reason = |kind, field: &str, value: &str| MatchReason {
        kind,
        field: field.to_string(),
        value: value.to_string(),
    };
    let names = || {
        NAME_FIELDS.iter().filter_map(|field| {
            fact.properties[*field]
                .as_str()
                .map(|value| (*field, value))
        })
    };
    match query {
        DiscoveryQuery::Identifier { id: wanted } => {
            (id == wanted).then(|| reason(MatchKind::ExactIdentifier, "id", id))
        }
        DiscoveryQuery::Name { name } => names()
            .find(|(_, value)| value == name)
            .map(|(field, value)| reason(MatchKind::ExactName, field, value)),
        DiscoveryQuery::Text { text } => std::iter::once(("id", id))
            .chain(names())
            .find(|(_, value)| value.to_lowercase().contains(text.as_str()))
            .map(|(field, value)| reason(MatchKind::TextSubstring, field, value)),
    }
}

impl ContextSnapshot {
    /// Find candidate nodes under [`DISCOVERY_POLICY`], one bounded page at a
    /// time. A candidate that cannot fit the byte budget fails explicitly;
    /// it is never skipped to fit later ones.
    pub fn discover(&self, request: DiscoveryRequest) -> Result<DiscoveryResponse, ContextError> {
        validate_limit("max_candidates", request.max_candidates, MAX_CANDIDATES)?;
        validate_limit("max_bytes", request.max_bytes, MAX_RESPONSE_BYTES)?;
        let query = request.query.normalized()?;
        let labels: BTreeSet<String> = request.labels.into_iter().collect();
        let query_id = content_identity(
            "discovery-query-v1",
            &QueryIdentity {
                policy: DISCOVERY_POLICY,
                query: &query,
                labels: &labels,
            },
        )?;
        let offset = match request.cursor {
            Some(cursor) => {
                if cursor.snapshot_id != self.id {
                    return Err(ContextError::CursorSnapshotMismatch);
                }
                if cursor.policy != DISCOVERY_POLICY {
                    return Err(ContextError::CursorPolicyMismatch);
                }
                if cursor.query_id != query_id {
                    return Err(ContextError::CursorSelectionMismatch);
                }
                Some(cursor.offset)
            }
            None => None,
        };

        let mut degree: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for fact in &self.facts {
            if let FactReference::Edge {
                source,
                destination,
                ..
            } = &fact.reference
            {
                degree.entry(source).or_default().0 += 1;
                degree.entry(destination).or_default().1 += 1;
            }
        }
        let mut coverage = DiscoveryCoverage {
            nodes_examined: 0,
            fields: std::iter::once("id")
                .chain(NAME_FIELDS.iter().copied())
                .map(String::from)
                .collect(),
            nodes_without_name_fields: 0,
        };
        let mut matched = Vec::new();
        for fact in &self.facts {
            let FactReference::Node { id } = &fact.reference else {
                continue;
            };
            if !labels.is_empty() && !labels.contains(&fact.label) {
                continue;
            }
            coverage.nodes_examined += 1;
            if NAME_FIELDS
                .iter()
                .all(|field| fact.properties[*field].as_str().is_none())
            {
                coverage.nodes_without_name_fields += 1;
            }
            if let Some(match_reason) = match_fact(&query, fact, id) {
                matched.push((match_reason.kind, id.as_str(), fact, match_reason));
            }
        }
        matched.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));

        let offset = offset.unwrap_or(0);
        if offset > matched.len() {
            return Err(ContextError::InvalidCursorOffset {
                offset,
                total_selected: matched.len(),
            });
        }
        let cursor_at = |offset| {
            (offset < matched.len()).then(|| DiscoveryCursor {
                snapshot_id: self.id.clone(),
                query_id: query_id.clone(),
                policy: DISCOVERY_POLICY.to_string(),
                offset,
            })
        };
        let mut response = DiscoveryResponse {
            snapshot_id: self.id.clone(),
            policy: DISCOVERY_POLICY.to_string(),
            query_id: query_id.clone(),
            total_candidates: matched.len(),
            candidates: Vec::new(),
            coverage,
            omissions: omissions(),
            next_cursor: cursor_at(offset),
        };
        for (_, id, fact, match_reason) in matched.iter().skip(offset).take(request.max_candidates)
        {
            let evidence = fact
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.evidence.first());
            let (outgoing_edges, incoming_edges) = degree.get(id).copied().unwrap_or_default();
            response.candidates.push(DiscoveryCandidate {
                reference: fact.reference.clone(),
                label: fact.label.clone(),
                source: CandidateSource {
                    repo: evidence.map(|evidence| evidence.repo.clone()),
                    path: evidence.map(|evidence| evidence.path.clone()),
                    commit_sha: evidence.map(|evidence| evidence.commit_sha.clone()),
                },
                match_reason: match_reason.clone(),
                outgoing_edges,
                incoming_edges,
                confidence_tier: fact.confidence_tier,
            });
            response.next_cursor = cursor_at(offset + response.candidates.len());
            let bytes = serde_json::to_vec(&response)?.len();
            if bytes > request.max_bytes {
                response.candidates.pop();
                if response.candidates.is_empty() {
                    return Err(ContextError::ResponseBudgetExceeded {
                        max_bytes: request.max_bytes,
                        required_bytes: bytes,
                    });
                }
                response.next_cursor = cursor_at(offset + response.candidates.len());
                return Ok(response);
            }
        }
        let bytes = serde_json::to_vec(&response)?.len();
        if bytes > request.max_bytes {
            return Err(ContextError::ResponseBudgetExceeded {
                max_bytes: request.max_bytes,
                required_bytes: bytes,
            });
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests;
