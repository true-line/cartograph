//! Borrowed output views and admission shared by both tracing entry points.

use crate::{Flow, FlowStatus, Hop};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier};
use serde::{Deserialize, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::io;

/// Cumulative bounds on the complete tracing result. Zero is a real limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceLimits {
    /// Maximum number of returned flows, including triggers with no hops.
    pub max_flows: usize,
    /// Maximum deduplicated hops summed across every returned flow.
    pub max_hops: usize,
    /// Maximum compact UTF-8 JSON bytes of the complete returned flow array.
    pub max_output_bytes: usize,
}

/// The resource that prevented a complete tracing result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceLimit {
    /// Flow count.
    Flows,
    /// Cumulative hop count.
    Hops,
    /// Complete serialized output bytes.
    OutputBytes,
}

/// An explicit limit failure; no partial result accompanies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceLimitExceeded {
    /// Resource whose next admission would exceed its maximum.
    pub limit: TraceLimit,
    /// Configured maximum for that resource.
    pub maximum: usize,
}

impl fmt::Display for TraceLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "flow tracing {:?} limit exceeded (maximum {})",
            self.limit, self.maximum
        )
    }
}
impl std::error::Error for TraceLimitExceeded {}

pub(super) struct TraceBudget {
    limits: Option<TraceLimits>,
    flows: usize,
    hops: usize,
    bytes: usize,
}

impl TraceBudget {
    pub(super) fn new(limits: Option<TraceLimits>) -> Result<Self, TraceLimitExceeded> {
        if let Some(limits) = limits
            && limits.max_output_bytes < 2
        {
            return Err(TraceLimitExceeded {
                limit: TraceLimit::OutputBytes,
                maximum: limits.max_output_bytes,
            });
        }
        Ok(Self {
            limits,
            flows: 0,
            hops: 0,
            bytes: 2,
        })
    }

    pub(super) fn start_flow(&self) -> Result<(), TraceLimitExceeded> {
        if let Some(limits) = self.limits
            && self.flows >= limits.max_flows
        {
            return Err(TraceLimitExceeded {
                limit: TraceLimit::Flows,
                maximum: limits.max_flows,
            });
        }
        Ok(())
    }

    pub(super) fn admit_hop(&mut self) -> Result<(), TraceLimitExceeded> {
        if let Some(limits) = self.limits
            && self.hops >= limits.max_hops
        {
            return Err(TraceLimitExceeded {
                limit: TraceLimit::Hops,
                maximum: limits.max_hops,
            });
        }
        self.hops += 1;
        Ok(())
    }

    pub(super) fn admit_flow(&mut self, flow: &FlowView<'_, '_>) -> Result<(), TraceLimitExceeded> {
        if let Some(limits) = self.limits {
            let remaining = limits.max_output_bytes - self.bytes;
            let mut writer = CountingWriter {
                remaining,
                written: 0,
            };
            let comma = usize::from(self.flows > 0);
            if writer.write_count(comma).is_err()
                || serde_json::to_writer(&mut writer, flow).is_err()
            {
                return Err(TraceLimitExceeded {
                    limit: TraceLimit::OutputBytes,
                    maximum: limits.max_output_bytes,
                });
            }
            self.bytes += writer.written;
        }
        self.flows += 1;
        Ok(())
    }
}

struct CountingWriter {
    remaining: usize,
    written: usize,
}
impl CountingWriter {
    fn write_count(&mut self, len: usize) -> io::Result<()> {
        if len > self.remaining {
            return Err(io::Error::other("flow output byte limit"));
        }
        self.remaining -= len;
        self.written += len;
        Ok(())
    }
}
impl io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_count(bytes.len())?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct Candidate<'a> {
    pub(super) edge: &'a Edge,
    pub(super) reversed: bool,
}
impl<'a> Candidate<'a> {
    pub(super) fn src(self) -> &'a str {
        if self.reversed {
            &self.edge.dst
        } else {
            &self.edge.src
        }
    }
    pub(super) fn dst(self) -> &'a str {
        if self.reversed {
            &self.edge.src
        } else {
            &self.edge.dst
        }
    }
}

#[derive(Clone, Copy)]
enum Name<'a> {
    Node(&'a Node),
    Missing(&'a str),
}
impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Node(node) = self else {
            let Self::Missing(id) = self else {
                unreachable!()
            };
            return f.write_str(id);
        };
        let p = &node.props;
        match node.label.as_str() {
            "Screen" => match p["route"].as_str() {
                Some(route) => write!(f, "Screen {route}"),
                None => f.write_str(&node.id),
            },
            "Endpoint" => match (p["method"].as_str(), p["path"].as_str()) {
                (Some(method), Some(path)) => write!(f, "{method} {path}"),
                _ => f.write_str(&node.id),
            },
            "Symbol" | "Component" => f.write_str(
                p["name"]
                    .as_str()
                    .unwrap_or_else(|| node.id.split('#').next_back().unwrap_or(&node.id)),
            ),
            "Channel" => match (p["kind"].as_str(), p["identity"].as_str()) {
                (Some(kind), Some(identity)) => write!(f, "{kind}:{identity}"),
                _ => f.write_str(&node.id),
            },
            "ExtensionContext" => {
                let kind = p["kind"].as_str().unwrap_or("context");
                let detail = p["entries"]
                    .as_array()
                    .and_then(|entries| entries.first())
                    .and_then(|entry| entry.as_str())
                    .or_else(|| p["title"].as_str().filter(|title| !title.is_empty()));
                match detail {
                    Some(detail) => write!(f, "{kind} · {detail}"),
                    None => f.write_str(kind),
                }
            }
            "Command" => match p["name"].as_str() {
                Some(name) => write!(f, "Command {name}"),
                None => f.write_str(&node.id),
            },
            "Schedule" => match (p["name"].as_str(), p["schedule"].as_str()) {
                (Some(name), Some(schedule)) => write!(f, "Schedule {name} ({schedule})"),
                (Some(name), None) => write!(f, "Schedule {name}"),
                _ => f.write_str(&node.id),
            },
            "EntryPoint" => f.write_str(p["name"].as_str().unwrap_or(&node.id)),
            "Gap" => write!(f, "GAP: {}", p["reason"].as_str().unwrap_or("unresolved")),
            "File" => f.write_str(p["path"].as_str().unwrap_or(&node.id)),
            _ => f.write_str(&node.id),
        }
    }
}
impl Serialize for Name<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// Borrowing avoids cloning any source-authored provenance string before
// admission. The evidence-reference vector is linear in the admitted input.
#[derive(Serialize, Deserialize)]
struct EvidenceView<'a> {
    #[serde(borrow)]
    repo: &'a str,
    #[serde(borrow)]
    path: &'a str,
    byte_start: u64,
    byte_end: u64,
    #[serde(borrow)]
    commit_sha: &'a str,
}
// Match ProducingMethod's wire representation while retaining every
// source-authored label as a borrow until the complete flow is admitted.
#[derive(Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MethodView<'a> {
    Rule {
        #[serde(borrow)]
        rule: &'a str,
        #[serde(borrow)]
        version: &'a str,
    },
    Model {
        #[serde(borrow)]
        model: &'a str,
    },
    Human {
        #[serde(borrow)]
        actor: &'a str,
    },
    #[default]
    Unrecorded,
}
impl MethodView<'_> {
    fn is_unrecorded(&self) -> bool {
        matches!(self, Self::Unrecorded)
    }

    fn into_owned(self) -> ProducingMethod {
        match self {
            Self::Rule { rule, version } => ProducingMethod::Rule {
                rule: rule.into(),
                version: version.into(),
            },
            Self::Model { model } => ProducingMethod::Model {
                model: model.into(),
            },
            Self::Human { actor } => ProducingMethod::Human {
                actor: actor.into(),
            },
            Self::Unrecorded => ProducingMethod::Unrecorded,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ProvenanceView<'a> {
    tier: Tier,
    confidence_tier: ConfidenceTier,
    #[serde(borrow)]
    evidence: Vec<EvidenceView<'a>>,
    #[serde(borrow)]
    extractor_id: Cow<'a, str>,
    #[serde(borrow)]
    content_hash: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "MethodView::is_unrecorded")]
    method: MethodView<'a>,
}
impl<'a> ProvenanceView<'a> {
    // Do not construct source-bearing serde_json diagnostics or buffer an
    // internally-tagged enum's unknown fields during tracing. Match this private
    // view's accepted serde shape using borrows and closed enum/scalar checks.
    fn borrow(value: &'a serde_json::Value) -> Option<Self> {
        use serde_json::Value;
        // Serde accepts both a string and an externally tagged unit object for
        // these two public unit enums. Preserve that historical input behavior.
        fn unit(value: &Value) -> Option<&str> {
            match value {
                Value::String(name) => Some(name),
                Value::Object(map) if map.len() == 1 => {
                    let (name, value) = map.iter().next()?;
                    value.is_null().then_some(name)
                }
                _ => None,
            }
        }
        fn field<'v>(value: &'v Value, name: &str, index: usize) -> Option<&'v Value> {
            match value {
                Value::Object(map) => map.get(name),
                Value::Array(values) => values.get(index),
                _ => None,
            }
        }
        // Derived struct deserializers also accept their positional sequence
        // representation; the final method field alone has a default.
        if !value.is_object() && !value.as_array().is_some_and(|v| (5..=6).contains(&v.len())) {
            return None;
        }
        let tier = match unit(field(value, "tier", 0)?)? {
            "Deterministic" => Tier::Deterministic,
            "Dynamic" => Tier::Dynamic,
            "Semantic" => Tier::Semantic,
            "Agentic" => Tier::Agentic,
            _ => return None,
        };
        let confidence_tier = match unit(field(value, "confidence_tier", 1)?)? {
            "Confirmed" => ConfidenceTier::Confirmed,
            "InferredStrong" => ConfidenceTier::InferredStrong,
            "InferredWeak" => ConfidenceTier::InferredWeak,
            "Gap" => ConfidenceTier::Gap,
            _ => return None,
        };
        let evidence = field(value, "evidence", 2)?
            .as_array()?
            .iter()
            .map(|entry| {
                if !entry.is_object() && entry.as_array().is_none_or(|v| v.len() != 5) {
                    return None;
                }
                Some(EvidenceView {
                    repo: field(entry, "repo", 0)?.as_str()?,
                    path: field(entry, "path", 1)?.as_str()?,
                    byte_start: field(entry, "byte_start", 2)?.as_u64()?,
                    byte_end: field(entry, "byte_end", 3)?.as_u64()?,
                    commit_sha: field(entry, "commit_sha", 4)?.as_str()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let method = match field(value, "method", 5) {
            None => MethodView::Unrecorded,
            Some(method) => {
                let kind = field(method, "kind", 0)?.as_str()?;
                let sequence_len = method.as_array().map(Vec::len);
                match kind {
                    "rule" if sequence_len.is_none_or(|n| n == 3) => MethodView::Rule {
                        rule: field(method, "rule", 1)?.as_str()?,
                        version: field(method, "version", 2)?.as_str()?,
                    },
                    "model" if sequence_len.is_none_or(|n| n == 2) => MethodView::Model {
                        model: field(method, "model", 1)?.as_str()?,
                    },
                    "human" if sequence_len.is_none_or(|n| n == 2) => MethodView::Human {
                        actor: field(method, "actor", 1)?.as_str()?,
                    },
                    "unrecorded" if sequence_len.is_none_or(|n| n == 1) => MethodView::Unrecorded,
                    _ => return None,
                }
            }
        };
        Some(Self {
            tier,
            confidence_tier,
            evidence,
            extractor_id: Cow::Borrowed(field(value, "extractor_id", 3)?.as_str()?),
            content_hash: Cow::Borrowed(field(value, "content_hash", 4)?.as_str()?),
            method,
        })
    }
    // A graph read for an export holds `prov` as its compact text, which
    // needs no escape (core_graph::stored), so the view borrows from the
    // text. The decoded text is checked first, as a stored object is, so a
    // malformed one still takes the fallback without a serde diagnostic.
    fn stored(edge: &'a Edge) -> Option<Self> {
        let prov = &edge.props["prov"];
        match core_graph::stored::compact_text(prov) {
            Some(text) => {
                let decoded = serde_json::from_str::<serde_json::Value>(text).ok()?;
                ProvenanceView::borrow(&decoded)?;
                serde_json::from_str::<Self>(text).ok()
            }
            None => Self::borrow(prov),
        }
    }
    fn new(edge: &'a Edge) -> Self {
        if let Some(view) = Self::stored(edge) {
            let validation = Provenance {
                tier: view.tier,
                confidence_tier: view.confidence_tier,
                evidence: Vec::new(),
                extractor_id: String::new(),
                content_hash: String::new(),
                // Provenance::validate checks the tier/confidence ceiling;
                // it places no restrictions on method labels. Keep those
                // labels borrowed, verbatim, rather than inventing rules.
                method: ProducingMethod::Unrecorded,
                basis: None,
            };
            if validation.validate().is_ok() {
                return view;
            }
        }
        // Match the legacy fallback's exact tuple bytes without allocating
        // a potentially large canonical identity buffer.
        let mut hasher = blake3::Hasher::new();
        struct HashWriter<'a>(&'a mut blake3::Hasher);
        impl io::Write for HashWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(
            HashWriter(&mut hasher),
            &(&edge.src, &edge.label, &edge.dst),
        )
        .expect("edge identity serializes");
        Self {
            tier: Tier::Deterministic,
            confidence_tier: ConfidenceTier::Gap,
            evidence: Vec::new(),
            extractor_id: Cow::Borrowed("flowtracer.invalid-provenance"),
            content_hash: Cow::Owned(hasher.finalize().to_hex().to_string()),
            method: MethodView::Unrecorded,
        }
    }
    fn into_owned(self) -> Provenance {
        Provenance {
            tier: self.tier,
            confidence_tier: self.confidence_tier,
            evidence: self
                .evidence
                .into_iter()
                .map(|e| EvidenceRef {
                    repo: e.repo.into(),
                    path: e.path.into(),
                    byte_start: e.byte_start,
                    byte_end: e.byte_end,
                    commit_sha: e.commit_sha.into(),
                })
                .collect(),
            extractor_id: self.extractor_id.into_owned(),
            content_hash: self.content_hash.into_owned(),
            method: self.method.into_owned(),
            basis: None,
        }
    }
}

struct EvidenceDisplay<'a> {
    path: &'a str,
    start: u64,
    end: u64,
}
impl fmt::Display for EvidenceDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes {}..{}", self.path, self.start, self.end)
    }
}
impl Serialize for EvidenceDisplay<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
struct AttemptedTiers<'a>(Option<&'a Node>);
impl AttemptedTiers<'_> {
    fn iter(&self) -> impl Iterator<Item = &str> {
        self.0
            .and_then(|node| node.props["attempted_tiers"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|tier| tier.as_str())
    }
}
impl Serialize for AttemptedTiers<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

#[derive(Serialize)]
pub(super) struct HopView<'a> {
    label: &'a str,
    src: &'a str,
    dst: &'a str,
    src_name: Name<'a>,
    dst_name: Name<'a>,
    tier: &'static str,
    pub(super) confidence: &'static str,
    evidence: Option<EvidenceDisplay<'a>>,
    provenance: ProvenanceView<'a>,
    gap_reason: Option<&'a str>,
    attempted_tiers: AttemptedTiers<'a>,
}
impl<'a> HopView<'a> {
    pub(super) fn new(candidate: Candidate<'a>, by_id: &BTreeMap<&'a str, &'a Node>) -> Self {
        let provenance = ProvenanceView::new(candidate.edge);
        let tier = match provenance.tier {
            Tier::Deterministic => "Deterministic",
            Tier::Dynamic => "Dynamic",
            Tier::Semantic => "Semantic",
            Tier::Agentic => "Agentic",
        };
        let confidence = match provenance.confidence_tier {
            ConfidenceTier::Confirmed => "Confirmed",
            ConfidenceTier::InferredStrong => "InferredStrong",
            ConfidenceTier::InferredWeak => "InferredWeak",
            ConfidenceTier::Gap => "Gap",
        };
        let evidence = provenance.evidence.first().map(|e| EvidenceDisplay {
            path: e.path,
            start: e.byte_start,
            end: e.byte_end,
        });
        let name = |id| {
            by_id
                .get(id)
                .map_or(Name::Missing(id), |node| Name::Node(node))
        };
        let gap = by_id
            .get(candidate.dst())
            .copied()
            .filter(|node| node.label == "Gap");
        Self {
            label: &candidate.edge.label,
            src: candidate.src(),
            dst: candidate.dst(),
            src_name: name(candidate.src()),
            dst_name: name(candidate.dst()),
            tier,
            confidence,
            evidence,
            provenance,
            gap_reason: gap.and_then(|node| node.props["reason"].as_str()),
            attempted_tiers: AttemptedTiers(gap),
        }
    }
    fn into_owned(self) -> Hop {
        #[cfg(test)]
        OWNED_HOPS.with(|counter| counter.set(counter.get() + 1));
        Hop {
            label: self.label.into(),
            src: self.src.into(),
            dst: self.dst.into(),
            src_name: self.src_name.to_string(),
            dst_name: self.dst_name.to_string(),
            tier: self.tier.into(),
            confidence: self.confidence.into(),
            evidence: self.evidence.map(|e| e.to_string()),
            provenance: self.provenance.into_owned(),
            gap_reason: self.gap_reason.map(String::from),
            attempted_tiers: self.attempted_tiers.iter().map(String::from).collect(),
        }
    }
}

struct HopSequence<'a, 'b> {
    hops: &'b [Candidate<'a>],
    by_id: &'b BTreeMap<&'a str, &'a Node>,
}
impl Serialize for HopSequence<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.hops
                .iter()
                .map(|candidate| HopView::new(*candidate, self.by_id)),
        )
    }
}
#[derive(Serialize)]
pub(super) struct FlowView<'a, 'b> {
    trigger: &'a str,
    trigger_kind: &'a str,
    trigger_name: Name<'a>,
    hops: HopSequence<'a, 'b>,
    status: FlowStatus,
    score: f64,
    depth_limited: bool,
}
impl<'a, 'b> FlowView<'a, 'b> {
    pub(super) fn new(
        trigger: &'a Node,
        hops: &'b [Candidate<'a>],
        by_id: &'b BTreeMap<&'a str, &'a Node>,
        status: FlowStatus,
        score: f64,
        depth_limited: bool,
    ) -> Self {
        Self {
            trigger: &trigger.id,
            trigger_kind: &trigger.label,
            trigger_name: Name::Node(trigger),
            hops: HopSequence { hops, by_id },
            status,
            score,
            depth_limited,
        }
    }
    pub(super) fn into_owned(self) -> Flow {
        #[cfg(test)]
        OWNED_FLOWS.with(|counter| counter.set(counter.get() + 1));
        Flow {
            trigger: self.trigger.into(),
            trigger_kind: self.trigger_kind.into(),
            trigger_name: self.trigger_name.to_string(),
            hops: self
                .hops
                .hops
                .iter()
                .map(|candidate| HopView::new(*candidate, self.hops.by_id).into_owned())
                .collect(),
            status: self.status,
            score: self.score,
            depth_limited: self.depth_limited,
        }
    }
}

#[cfg(test)]
thread_local! { pub(super) static OWNED_FLOWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; pub(super) static OWNED_HOPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_view_borrows_strings_and_matches_the_owned_wire_shape() {
        let edge = Edge {
            src: "a".into(),
            dst: "b".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "tier": "Deterministic", "confidence_tier": "Confirmed",
                "evidence": [{"repo": "repo", "path": "large".repeat(100_000), "byte_start": 0, "byte_end": 9, "commit_sha": "sha"}],
                "extractor_id": "extractor".repeat(100_000), "content_hash": "hash".repeat(100_000),
                "ignored": "not part of provenance"
            }}),
        };
        let view = ProvenanceView::new(&edge);
        assert!(matches!(view.extractor_id, Cow::Borrowed(_)));
        assert!(matches!(view.content_hash, Cow::Borrowed(_)));
        assert_eq!(
            view.evidence[0].path.as_ptr(),
            edge.props["prov"]["evidence"][0]["path"]
                .as_str()
                .unwrap()
                .as_ptr()
        );
        let owned: Provenance = serde_json::from_value(edge.props["prov"].clone()).unwrap();
        assert_eq!(
            serde_json::to_vec(&view).unwrap(),
            serde_json::to_vec(&owned).unwrap()
        );
    }

    // AC-1208 (T-1208): a provenance a spec read holds as text borrows from it.
    #[test]
    fn a_compacted_provenance_borrows_from_its_text_and_reads_as_stored() {
        let stored = serde_json::json!({
            "tier": "Deterministic", "confidence_tier": "Confirmed",
            "evidence": [{"repo": "repo", "path": "src/a.ts", "byte_start": 0, "byte_end": 9, "commit_sha": "sha"}],
            "extractor_id": "extractor", "content_hash": "hash",
            "method": {"kind": "rule", "rule": "r", "version": "v1"}
        });
        let edge = |prov: serde_json::Value| Edge {
            src: "a".into(),
            dst: "b".into(),
            label: "CALLS".into(),
            props: serde_json::json!({ "prov": prov }),
        };
        let object = edge(stored.clone());
        let mut text = edge(stored.clone());
        core_graph::stored::compact_borrowable(&mut text.props["prov"]);
        let held = core_graph::stored::compact_text(&text.props["prov"]).unwrap();
        let view = ProvenanceView::new(&text);
        assert!(matches!(view.content_hash, Cow::Borrowed(_)));
        assert!(
            held.as_bytes()
                .as_ptr_range()
                .contains(&view.evidence[0].path.as_ptr())
        );
        assert_eq!(
            serde_json::to_vec(&view).unwrap(),
            serde_json::to_vec(&ProvenanceView::new(&object)).unwrap()
        );
        // Text of a provenance outside its tier's ceiling takes the fallback,
        // as the object does.
        let mut gap = stored;
        gap["tier"] = "Semantic".into();
        let object = edge(gap.clone());
        let mut text = edge(gap);
        core_graph::stored::compact_borrowable(&mut text.props["prov"]);
        assert_eq!(
            serde_json::to_vec(&ProvenanceView::new(&text)).unwrap(),
            serde_json::to_vec(&ProvenanceView::new(&object)).unwrap()
        );
    }

    #[test]
    fn producing_method_views_borrow_and_match_authoritative_serialization() {
        for method in [
            serde_json::json!({"kind":"rule", "rule":"large".repeat(100_000), "version":"v1"}),
            serde_json::json!({"kind":"model", "model":"large".repeat(100_000)}),
            serde_json::json!({"kind":"human", "actor":"large".repeat(100_000)}),
            serde_json::json!({"kind":"unrecorded"}),
        ] {
            let view = MethodView::deserialize(&method).unwrap();
            let labels: Vec<(&str, &str)> = match &view {
                MethodView::Rule { rule, version } => vec![(rule, "rule"), (version, "version")],
                MethodView::Model { model } => vec![(model, "model")],
                MethodView::Human { actor } => vec![(actor, "actor")],
                MethodView::Unrecorded => vec![],
            };
            for (borrowed, key) in labels {
                assert_eq!(borrowed.as_ptr(), method[key].as_str().unwrap().as_ptr());
            }
            let expected: ProducingMethod = serde_json::from_value(method.clone()).unwrap();
            assert_eq!(
                serde_json::to_vec(&view).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            assert_eq!(view.into_owned(), expected);
        }
    }
}

mod workspace;
pub use workspace::{
    TRACE_WORKSPACE_MODEL, TraceGraphBound, TraceWorkspaceBound, logical_workspace_bound,
    output_evidence_bound,
};
