//! Exact compact output admission over borrowed security findings.
use super::support::{Evidence, Support};
use super::{
    SECURITY_EXTRACTOR_T0, SECURITY_EXTRACTOR_T1, SECURITY_EXTRACTOR_T2, explicit_auth_state,
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Tier};
use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::{fmt, io};

/// Explicit admission for a complete security projection, never a partial result.
#[derive(Debug, Clone, Copy)]
pub struct SecurityLimits {
    /// Maximum complete finding rows after stable identity deduplication.
    pub max_rows: usize,
    /// Complete compact JSON `Vec<Node>`, including punctuation and escaping.
    pub max_output_bytes: usize,
}
/// Resource preventing security output materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityLimit {
    /// Finding rows.
    Rows,
    /// Complete serialized finding bytes.
    OutputBytes,
}
/// Fixed resource failure without input data or partial findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityLimitExceeded {
    /// Failed resource.
    pub limit: SecurityLimit,
    /// Caller ceiling for this resource.
    pub maximum: usize,
}
/// Complete output shape, or a checked source-derived upper bound on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityProjectionSize {
    /// Finding rows.
    pub rows: usize,
    /// Compact serialized bytes, including array punctuation.
    pub bytes: usize,
}

struct Text<'a>(&'a dyn fmt::Display);
impl Serialize for Text<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self.0)
    }
}
#[derive(Clone, Copy)]
enum Name<'a> {
    Endpoint(&'a str, &'a str),
    Id(&'a str),
}
impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(method, path) => write!(f, "{method} {path}"),
            Self::Id(id) => f.write_str(id),
        }
    }
}
enum Claim<'a> {
    Endpoint {
        id: &'a str,
        name: Name<'a>,
    },
    Grant {
        src: &'a str,
        dst: &'a str,
        actions: Vec<&'a str>,
        scopes: Vec<&'a str>,
    },
}
struct Finding<'a> {
    claim: Claim<'a>,
    support: Support<'a>,
    hash: String,
    tier: Tier,
    extractor: &'static str,
}
struct Id<'a>(&'a str);
impl fmt::Display for Id<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "finding:security:{}", &self.0[..16])
    }
}
struct Title<'a>(&'a Claim<'a>);
impl fmt::Display for Title<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Claim::Endpoint { name, .. } => write!(f, "Unauthenticated endpoint: {name}"),
            Claim::Grant { src, scopes, .. } => {
                write!(f, "Over-broad IAM grant: {src} → ")?;
                for (i, scope) in scopes.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(scope)?;
                }
                Ok(())
            }
        }
    }
}
struct Subject<'a>(&'a Claim<'a>);
impl fmt::Display for Subject<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Claim::Endpoint { id, .. } => f.write_str(id),
            Claim::Grant { src, dst, .. } => write!(f, "{src} GRANTS {dst}"),
        }
    }
}
#[derive(Serialize)]
struct Rule {
    kind: &'static str,
    rule: &'static str,
    version: &'static str,
}
#[derive(Serialize)]
struct Provenance<'a> {
    confidence_tier: ConfidenceTier,
    content_hash: &'a str,
    evidence: &'a [Evidence<'a>],
    extractor_id: &'static str,
    method: Rule,
    tier: Tier,
}
struct Scopes<'a>(&'a Claim<'a>);
impl Serialize for Scopes<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Claim::Endpoint { name, .. } => [Text(name)].serialize(s),
            Claim::Grant { scopes, .. } => scopes.serialize(s),
        }
    }
}
#[derive(Serialize)]
struct Props<'a> {
    ac_id: &'static str,
    actions: &'a [&'a str],
    category: &'static str,
    kind: &'static str,
    prov: Provenance<'a>,
    resource_scope: Scopes<'a>,
    severity: &'static str,
    subject_id: Text<'a>,
    title: Text<'a>,
    us_id: &'static str,
}
#[derive(Serialize)]
struct View<'a> {
    id: Text<'a>,
    label: &'static str,
    props: Props<'a>,
}
impl Serialize for Finding<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (ac_id, category, actions) = match &self.claim {
            Claim::Endpoint { .. } => ("AC-0041", "unauthenticated_endpoint", &[][..]),
            Claim::Grant { actions, .. } => ("AC-0042", "over_broad_grant", actions.as_slice()),
        };
        View {
            id: Text(&Id(&self.hash)),
            label: "Finding",
            props: Props {
                ac_id,
                actions,
                category,
                kind: "security",
                prov: Provenance {
                    confidence_tier: self.support.confidence_tier,
                    content_hash: &self.hash,
                    evidence: &self.support.evidence,
                    extractor_id: self.extractor,
                    method: Rule {
                        kind: "rule",
                        rule: self.extractor,
                        version: crate::SPEC_RULE_VERSION,
                    },
                    tier: self.tier,
                },
                resource_scope: Scopes(&self.claim),
                severity: "high",
                subject_id: Text(&Subject(&self.claim)),
                title: Text(&Title(&self.claim)),
                us_id: "US-0015",
            },
        }
        .serialize(s)
    }
}
fn strings(value: &serde_json::Value) -> Vec<&str> {
    let mut values: Vec<_> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    if let Some(value) = value.as_str() {
        values.push(value);
    }
    values.sort();
    values.dedup();
    values
}
fn hash(value: &impl Serialize) -> String {
    struct Hash(blake3::Hasher);
    impl io::Write for Hash {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Hash(blake3::Hasher::new());
    serde_json::to_writer(&mut output, value).expect("security identity serializes");
    output.0.finalize().to_hex().to_string()
}
fn findings<'a, N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &'a [N],
    edges: &'a [E],
) -> Vec<Finding<'a>> {
    let mut result = Vec::new();
    for node in nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| node.label == "Endpoint" && explicit_auth_state(node) == Some(false))
    {
        let Some(support) = Support::from_endpoint_props(&node.props) else {
            continue;
        };
        let name = match (node.props["method"].as_str(), node.props["path"].as_str()) {
            (Some(method), Some(path)) => Name::Endpoint(method, path),
            _ => Name::Id(&node.id),
        };
        let content_hash = hash(&("unauthenticated-endpoint", &node.id, Text(&name)));
        result.push(finding(
            Claim::Endpoint { id: &node.id, name },
            support,
            content_hash,
        ));
    }
    for edge in edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| edge.label == "GRANTS")
    {
        let actions = strings(&edge.props["actions"]);
        let mut scopes = strings(&edge.props["resource_scopes"]);
        if scopes.is_empty() {
            scopes.push(&edge.dst);
        }
        if !actions
            .iter()
            .chain(scopes.iter())
            .any(|value| value.contains('*'))
        {
            continue;
        }
        let Some(support) = Support::from_props(&edge.props) else {
            continue;
        };
        let content_hash = hash(&("over-broad-grant", &edge.src, &edge.dst, &actions, &scopes));
        result.push(finding(
            Claim::Grant {
                src: &edge.src,
                dst: &edge.dst,
                actions,
                scopes,
            },
            support,
            content_hash,
        ));
    }
    result.sort_by(|left, right| left.hash[..16].cmp(&right.hash[..16]));
    result.dedup_by(|left, right| left.hash[..16] == right.hash[..16]);
    result
}
fn finding<'a>(claim: Claim<'a>, support: Support<'a>, hash: String) -> Finding<'a> {
    let tier = match support.confidence_tier {
        ConfidenceTier::Confirmed => support.tier,
        _ => Tier::Semantic,
    };
    let extractor = match tier {
        Tier::Deterministic => SECURITY_EXTRACTOR_T0,
        Tier::Dynamic => SECURITY_EXTRACTOR_T1,
        Tier::Semantic | Tier::Agentic => SECURITY_EXTRACTOR_T2,
    };
    Finding {
        claim,
        support,
        hash,
        tier,
        extractor,
    }
}
struct Counter {
    used: usize,
    maximum: usize,
}
impl io::Write for Counter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.used = self
            .used
            .checked_add(b.len())
            .filter(|size| *size <= self.maximum)
            .ok_or_else(|| io::Error::other("security output bound"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn size(
    findings: &[Finding<'_>],
    maximum: usize,
) -> Result<SecurityProjectionSize, SecurityLimitExceeded> {
    let mut count = Counter { used: 0, maximum };
    serde_json::to_writer(&mut count, findings).map_err(|_| SecurityLimitExceeded {
        limit: SecurityLimit::OutputBytes,
        maximum,
    })?;
    Ok(SecurityProjectionSize {
        rows: findings.len(),
        bytes: count.used,
    })
}
/// Count complete sorted/deduplicated security output without owning output
/// strings, evidence or JSON nodes. Input descriptors and evidence references
/// consume source-bounded workspace; this is not an RSS measurement.
pub fn security_projection_size(nodes: &[Node], edges: &[Edge]) -> Option<SecurityProjectionSize> {
    size(&findings(nodes, edges), usize::MAX).ok()
}
/// Admit the entire security output before any finding node is materialized.
/// Success exactly preserves ordinary finding IDs, properties and stable order.
pub fn derive_security_findings_bounded(
    nodes: &[Node],
    edges: &[Edge],
    limits: SecurityLimits,
) -> Result<Vec<Node>, SecurityLimitExceeded> {
    derive_bounded(nodes, edges, limits)
}

/// [`derive_security_findings_bounded`] over borrowed facts (#725).
pub(crate) fn derive_bounded<N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &[N],
    edges: &[E],
    limits: SecurityLimits,
) -> Result<Vec<Node>, SecurityLimitExceeded> {
    let findings = findings(nodes, edges);
    if findings.len() > limits.max_rows {
        return Err(SecurityLimitExceeded {
            limit: SecurityLimit::Rows,
            maximum: limits.max_rows,
        });
    }
    size(&findings, limits.max_output_bytes)?;
    Ok(findings
        .iter()
        .map(|finding| {
            #[cfg(test)]
            MATERIALIZED.with(|count| count.set(count.get() + 1));
            serde_json::from_value(
                serde_json::to_value(finding).expect("admitted security serializes"),
            )
            .expect("security view is Node")
        })
        .collect())
}
#[cfg(test)]
thread_local! {pub(crate) static MATERIALIZED:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}

#[cfg(test)]
#[path = "bounded_tests.rs"]
mod tests;
