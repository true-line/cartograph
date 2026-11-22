//! Pure node-only projection of bounded traces, not a business-intent claim.
use super::EvidenceKey;
use core_graph::{
    Node, RevisionedSnapshot,
    overlay::{BaseRef, MAX_FACT_BYTES, OverlayFact, OverlayItem},
};
use core_prov::{ConfidenceTier, EvidenceBasis, EvidenceRef, ProducingMethod, Provenance, Tier};
use flowtracer::{TraceLimit, TraceLimitExceeded, TraceLimits};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

/// Stable rule identifier stored in candidate provenance.
pub const BUSINESS_FLOW_RULE: &str = "knowledge.business-flow.traced-path";
/// Version of the traced-path interpretation and identity contract.
pub const BUSINESS_FLOW_RULE_VERSION: &str = "1";

/// Producer-local omission: the coordinator may still publish other producers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusinessFlowOmission {
    /// The complete trace exceeded a caller-supplied limit.
    Trace(TraceLimitExceeded),
    /// Support decoding and candidate output exhausted the projection allowance.
    ProjectionBytes,
}
impl BusinessFlowOmission {
    /// Stable source-free reason suitable for a producer omission record.
    pub fn code(self) -> &'static str {
        match self {
            Self::Trace(e) => match e.limit {
                TraceLimit::Flows => "business-flow-trace-flows",
                TraceLimit::Hops => "business-flow-trace-hops",
                TraceLimit::OutputBytes => "business-flow-trace-bytes",
            },
            Self::ProjectionBytes => "business-flow-projection-bytes",
        }
    }
}
impl std::fmt::Display for BusinessFlowOmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for BusinessFlowOmission {}

// Nonrefundable producer allowance: count borrowed provenance before decoding,
// then count final facts before retaining them. No sizing buffer is allocated.
fn charge(
    value: &impl serde::Serialize,
    remaining: &mut usize,
) -> Result<(), BusinessFlowOmission> {
    struct Counter<'a>(&'a mut usize);
    impl Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            *self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("projection limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(remaining), value)
        .map_err(|_| BusinessFlowOmission::ProjectionBytes)
}
#[cfg(test)]
thread_local! { static DECODED_SUPPORTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Per-flow omissions; the coordinator separately admits aggregate publication.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BusinessFlowReport {
    /// Supported candidates admitted within both producer bounds.
    pub facts: Vec<OverlayFact>,
    /// Triggers without a recovered hop.
    pub empty_flows: usize,
    /// Flows with missing, ambiguous, or invalid supporting facts.
    pub invalid_flows: usize,
    /// Candidates exceeding the existing per-fact JSON byte cap.
    pub oversized_flows: usize,
}
fn support(props: &serde_json::Value) -> Option<Provenance> {
    #[cfg(test)]
    DECODED_SUPPORTS.with(|n| n.set(n.get() + 1));
    props.get("prov")?;
    let p = Provenance::deserialize(&*crate::stored_prov(props)).ok()?;
    (p.validate().is_ok()
        // Match Provenance::basis() without cloning artifact refs: absent T1
        // resolves to Observation; every other absent basis resolves to Source.
        && match p.basis.as_ref() {
            Some(EvidenceBasis::Source) => true,
            None => p.tier != Tier::Dynamic,
            _ => false,
        }
        && !p.extractor_id.trim().is_empty()
        && !p.content_hash.trim().is_empty()
        && !p.evidence.is_empty()
        && p.evidence.iter().all(|e| {
            !e.repo.trim().is_empty()
                && !e.path.trim().is_empty()
                && !e.commit_sha.trim().is_empty()
                && e.byte_start < e.byte_end
        }))
    .then_some(p)
}
/// Trace admission failure returns no candidates. Each successful candidate
/// retains the tracer's complete ordered representation, including partial state.
/// The caller admits input and lookup/serialization workspace; this is not an
/// aggregate overlay admission or provider/ingest operation. `max_output_bytes`
/// is also a separate cumulative projection allowance: borrowed support-provenance
/// JSON plus final candidate JSON, including invalid/oversized work, without
/// refunds. Exhaustion discards the entire report, before another support decode.
pub fn derive_business_flows(
    base: &RevisionedSnapshot,
    limits: TraceLimits,
) -> Result<BusinessFlowReport, BusinessFlowOmission> {
    let flows = flowtracer::trace_bounded(&base.graph.0, &base.graph.1, limits)
        .map_err(BusinessFlowOmission::Trace)?;
    let mut remaining = limits.max_output_bytes;
    // Repeated anchors may yield identical traces, but their duplicated node id
    // becomes None here. Every candidate cites its trigger, so both are omitted.
    // Distinct trigger ids are part of the hashed trace/cites and cannot alias.
    let mut nodes = BTreeMap::new();
    for node in &base.graph.0 {
        nodes
            .entry(node.id.as_str())
            .and_modify(|n| *n = None)
            .or_insert(Some(node));
    }
    let mut edges = BTreeMap::new();
    for edge in &base.graph.1 {
        edges
            .entry((edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
            .and_modify(|e| *e = None)
            .or_insert(Some(edge));
    }
    let mut report = BusinessFlowReport::default();
    for flow in flows {
        if flow.hops.is_empty() {
            report.empty_flows += 1;
            continue;
        }
        let mut cites = BTreeSet::from([BaseRef::Node(flow.trigger.clone())]);
        for hop in &flow.hops {
            let (src, label, dst) = hop.stored_edge();
            cites.insert(BaseRef::Edge(src.into(), dst.into(), label.into()));
            cites.insert(BaseRef::Node(hop.src.clone()));
            cites.insert(BaseRef::Node(hop.dst.clone()));
        }
        let mut evidence: BTreeMap<EvidenceKey, EvidenceRef> = BTreeMap::new();
        let mut valid = true;
        for cite in &cites {
            let props = match cite {
                BaseRef::Node(id) if !id.is_empty() => {
                    nodes.get(id.as_str()).and_then(|n| *n).map(|n| &n.props)
                }
                BaseRef::Edge(src, dst, label) => edges
                    .get(&(src.as_str(), dst.as_str(), label.as_str()))
                    .and_then(|e| *e)
                    .map(|e| &e.props),
                _ => None,
            };
            let Some(props) = props else {
                valid = false;
                break;
            };
            charge(&crate::stored_prov(props), &mut remaining)?;
            let Some(p) = support(props) else {
                valid = false;
                break;
            };
            for e in p.evidence {
                evidence
                    .entry((
                        e.repo.clone(),
                        e.path.clone(),
                        e.byte_start,
                        e.byte_end,
                        e.commit_sha.clone(),
                    ))
                    .or_insert(e);
            }
        }
        if !valid {
            report.invalid_flows += 1;
            continue;
        }
        let evidence: Vec<_> = evidence.into_values().collect();
        let mut props = serde_json::json!({"name":format!("Traced flow from {}", flow.trigger_name), "trace":flow});
        // Bind emitted trace, exact citation closure and copied evidence. Input
        // order and unrelated base properties cannot change this identity.
        let content = serde_json::to_vec(&(
            BUSINESS_FLOW_RULE,
            BUSINESS_FLOW_RULE_VERSION,
            &props,
            &cites,
            &evidence,
        ))
        .expect("trace content serializes");
        let hash = core_prov::content_hash(&content);
        props["prov"] = serde_json::json!(
            Provenance::new(
                Tier::Semantic,
                ConfidenceTier::InferredWeak,
                evidence,
                "t2.knowledge.business-flow.traced-path",
                &content
            )
            .expect("semantic weak is within ceiling")
            .with_method(ProducingMethod::Rule {
                rule: BUSINESS_FLOW_RULE.into(),
                version: BUSINESS_FLOW_RULE_VERSION.into()
            })
        );
        let fact = OverlayFact {
            item: OverlayItem::Node(Node {
                id: format!("business-flow:traced-path:{hash}"),
                label: "BusinessFlow".into(),
                props,
            }),
            cites,
        };
        let before = remaining;
        charge(&fact, &mut remaining)?;
        let bytes = before
            .checked_sub(remaining)
            .expect("counter only decreases");
        if bytes > MAX_FACT_BYTES {
            report.oversized_flows += 1;
        } else {
            report.facts.push(fact);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
