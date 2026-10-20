//! Borrowed derived rows and exact admission before owned output materialization.
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Tier};
use serde::{Serialize, Serializer};
use std::{fmt, io};

/// Cumulative limits on derived rows and the exact compact serialized AdrFacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdrLimits {
    /// Sum of derived node and edge rows. Input graph/flows are admitted separately.
    pub max_rows: usize,
    /// Complete `{"nodes":[],"edges":[]}` representation, including punctuation.
    pub max_output_bytes: usize,
}
/// Resource that prevented complete ADR derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdrLimit {
    /// Cumulative derived node plus edge rows.
    Rows,
    /// Exact compact serialized output bytes, including envelopes.
    OutputBytes,
}
/// Explicit limit failure; never carries a partial result or source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdrLimitExceeded {
    /// Resource that prevented the next occurrence from being admitted.
    pub limit: AdrLimit,
    /// Caller-supplied maximum for that resource.
    pub maximum: usize,
}
impl fmt::Display for AdrLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ADR derivation {:?} limit exceeded (maximum {})",
            self.limit, self.maximum
        )
    }
}
impl std::error::Error for AdrLimitExceeded {}

pub(super) struct Budget {
    limits: Option<AdrLimits>,
    nodes: usize,
    edges: usize,
    bytes: usize,
}
impl Budget {
    pub(super) fn new(limits: Option<AdrLimits>) -> Result<Self, AdrLimitExceeded> {
        let bytes = br#"{"nodes":[],"edges":[]}"#.len();
        if let Some(limits) = limits
            && bytes > limits.max_output_bytes
        {
            return Err(AdrLimitExceeded {
                limit: AdrLimit::OutputBytes,
                maximum: limits.max_output_bytes,
            });
        }
        Ok(Self {
            limits,
            nodes: 0,
            edges: 0,
            bytes,
        })
    }
    pub(super) fn ensure_rows(&self, count: usize) -> Result<(), AdrLimitExceeded> {
        if let Some(limits) = self.limits
            && self
                .nodes
                .checked_add(self.edges)
                .and_then(|n| n.checked_add(count))
                .is_none_or(|n| n > limits.max_rows)
        {
            return Err(AdrLimitExceeded {
                limit: AdrLimit::Rows,
                maximum: limits.max_rows,
            });
        }
        Ok(())
    }
    pub(super) fn fragment_budget(&self) -> Counter {
        Counter {
            remaining: self
                .limits
                .map_or(usize::MAX, |limits| limits.max_output_bytes - self.bytes),
            written: 0,
        }
    }
    pub(super) fn fragment<T: Serialize>(
        &self,
        counter: &mut Counter,
        value: &T,
    ) -> Result<(), AdrLimitExceeded> {
        serde_json::to_writer(counter, value).map_err(|_| self.byte_error())
    }
    fn byte_error(&self) -> AdrLimitExceeded {
        AdrLimitExceeded {
            limit: AdrLimit::OutputBytes,
            maximum: self
                .limits
                .map_or(usize::MAX, |limits| limits.max_output_bytes),
        }
    }
    pub(super) fn admit<N: Serialize, E: Serialize>(
        &mut self,
        nodes: &[N],
        edges: &[E],
    ) -> Result<(), AdrLimitExceeded> {
        self.ensure_rows(nodes.len() + edges.len())?;
        if self.limits.is_some() {
            let mut counter = self.fragment_budget();
            for (existing, items) in [(self.nodes, nodes.len()), (self.edges, edges.len())] {
                let commas = if existing > 0 {
                    items
                } else {
                    items.saturating_sub(1)
                };
                counter.charge(commas).map_err(|_| self.byte_error())?;
            }
            for node in nodes {
                self.fragment(&mut counter, node)?;
            }
            for edge in edges {
                self.fragment(&mut counter, edge)?;
            }
            self.bytes += counter.written;
            self.nodes += nodes.len();
            self.edges += edges.len();
        }
        Ok(())
    }
}
pub(super) struct Counter {
    remaining: usize,
    written: usize,
}
impl Counter {
    fn charge(&mut self, bytes: usize) -> io::Result<()> {
        if bytes > self.remaining {
            return Err(io::Error::other("ADR output byte limit"));
        }
        self.remaining -= bytes;
        self.written += bytes;
        Ok(())
    }
}
impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.charge(bytes.len())?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Serialize)]
pub(super) struct NodeView<'a, P: Serialize> {
    pub id: &'a str,
    pub label: &'static str,
    pub props: P,
}
impl<P: Serialize> NodeView<'_, P> {
    pub(super) fn into_owned(self) -> Node {
        #[cfg(test)]
        MATERIALIZED_ROWS.with(|count| count.set(count.get() + 1));
        Node {
            id: self.id.into(),
            label: self.label.into(),
            props: serde_json::to_value(self.props).expect("admitted ADR props serialize"),
        }
    }
}
#[derive(Serialize)]
pub(super) struct EdgeView<'a, P: Serialize> {
    pub src: &'a str,
    pub dst: &'a str,
    pub label: &'static str,
    pub props: P,
}
impl<P: Serialize> EdgeView<'_, P> {
    pub(super) fn into_owned(self) -> Edge {
        #[cfg(test)]
        MATERIALIZED_ROWS.with(|count| count.set(count.get() + 1));
        Edge {
            src: self.src.into(),
            dst: self.dst.into(),
            label: self.label.into(),
            props: serde_json::to_value(self.props).expect("admitted ADR props serialize"),
        }
    }
}
#[derive(Serialize)]
pub(super) struct RuleView<'a> {
    kind: &'static str,
    rule: &'a str,
    version: &'static str,
}
#[derive(Serialize)]
pub(super) struct ProvenanceView<'a> {
    pub tier: Tier,
    pub confidence_tier: ConfidenceTier,
    pub evidence: &'a [&'a EvidenceRef],
    pub extractor_id: &'a str,
    pub content_hash: &'a str,
    pub method: RuleView<'a>,
}
impl<'a> ProvenanceView<'a> {
    pub(super) fn new(
        tier: Tier,
        confidence: ConfidenceTier,
        evidence: &'a [&'a EvidenceRef],
        extractor: &'a str,
        hash: &'a str,
    ) -> Self {
        Self {
            tier,
            confidence_tier: confidence,
            evidence,
            extractor_id: extractor,
            content_hash: hash,
            method: RuleView {
                kind: "rule",
                rule: extractor,
                version: crate::SPEC_RULE_VERSION,
            },
        }
    }
}
#[derive(Serialize)]
pub(super) struct OnlyProvenance<'a> {
    pub prov: &'a ProvenanceView<'a>,
}
#[derive(Serialize)]
pub(super) struct RecoveredProps<'a> {
    pub title: Text<'a>,
    pub status: &'static str,
    pub body: Text<'a>,
    pub origin: &'static str,
    pub recovered: bool,
    pub prov: &'a ProvenanceView<'a>,
}
#[derive(Serialize)]
pub(super) struct DriftProps<'a> {
    pub kind: &'static str,
    pub title: Text<'a>,
    pub adr_id: &'a str,
    pub offending_edge: Text<'a>,
    pub flow_triggers: &'a [&'a str],
    pub prov: &'a ProvenanceView<'a>,
}
pub(super) enum Text<'a> {
    RecoveredTitle(&'a str),
    RecoveredBody(&'a str),
    DriftTitle {
        adr: &'a str,
        label: &'a str,
        src: &'a str,
        dst: &'a str,
    },
    OffendingEdge {
        label: &'a str,
        src: &'a str,
        dst: &'a str,
    },
}
impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RecoveredTitle(name) => {
                write!(f, "Recovered: use {name} for asynchronous messaging")
            }
            Self::RecoveredBody(id) => write!(
                f,
                "The recovered graph uses channel `{id}` for asynchronous messaging. This is a proposed decision, not a confirmed author statement."
            ),
            Self::DriftTitle {
                adr,
                label,
                src,
                dst,
            } => write!(f, "{adr} forbids {label} but graph contains {src} → {dst}"),
            Self::OffendingEdge { label, src, dst } => write!(f, "{src} {label} {dst}"),
        }
    }
}
impl Serialize for Text<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

pub(super) fn canonical_hash(value: &impl Serialize) -> String {
    struct Hasher(blake3::Hasher);
    impl io::Write for Hasher {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = Hasher(blake3::Hasher::new());
    serde_json::to_writer(&mut hasher, value).expect("ADR identity serializes");
    hasher.0.finalize().to_hex().to_string()
}

pub(super) fn text_hash(value: &impl fmt::Display) -> String {
    struct Hasher(blake3::Hasher);
    impl io::Write for Hasher {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = Hasher(blake3::Hasher::new());
    io::Write::write_fmt(&mut hasher, format_args!("{value}"))
        .expect("ADR identity formatting succeeds");
    hasher.0.finalize().to_hex().to_string()
}

// Scoped test instrumentation of the owned row conversion boundary; this does
// not replace the allocator or alter admission in production.
#[cfg(test)]
thread_local! { static MATERIALIZED_ROWS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)}; }
#[cfg(test)]
pub(super) fn take_materialized_rows() -> usize {
    MATERIALIZED_ROWS.with(|count| count.replace(0))
}
