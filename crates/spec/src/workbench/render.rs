//! Shared streaming formatting and before-ownership assertion admission.
use super::fact_index::Subject;
use super::*;
use serde::Serializer;
use std::{cell::Cell, fmt, io, rc::Rc};

#[derive(Debug, Clone, Copy)]
pub(super) struct RenderLimit;
impl From<fmt::Error> for RenderLimit {
    fn from(_: fmt::Error) -> Self {
        Self
    }
}
#[derive(Clone)]
pub(super) struct Budget(Rc<State>);
struct State {
    maximum: usize,
    used: Cell<usize>,
}
impl Budget {
    pub(super) fn new(max: Option<usize>) -> Result<Self, RenderLimit> {
        let result = Self(Rc::new(State {
            maximum: max.unwrap_or(usize::MAX),
            used: Cell::new(0),
        }));
        result.charge(2)?;
        Ok(result)
    }
    fn charge(&self, n: usize) -> Result<(), RenderLimit> {
        let next = self
            .0
            .used
            .get()
            .checked_add(n)
            .filter(|n| *n <= self.0.maximum)
            .ok_or(RenderLimit)?;
        self.0.used.set(next);
        Ok(())
    }
    pub(super) fn remaining(&self) -> usize {
        self.0.maximum - self.0.used.get()
    }
    pub(super) fn check<T: Serialize>(&self, value: &T) -> Result<usize, RenderLimit> {
        let mut count = Counter {
            remaining: self.remaining(),
            written: 0,
        };
        serde_json::to_writer(&mut count, value).map_err(|_| RenderLimit)?;
        Ok(count.written)
    }
    fn admit<T: Serialize>(&self, value: &T, comma: bool) -> Result<(), RenderLimit> {
        let bytes = self
            .check(value)?
            .checked_add(usize::from(comma))
            .ok_or(RenderLimit)?;
        self.charge(bytes)
    }
}
struct Counter {
    remaining: usize,
    written: usize,
}
impl io::Write for Counter {
    fn write(&mut self, s: &[u8]) -> io::Result<usize> {
        if s.len() > self.remaining {
            return Err(io::Error::other("artifact output bound"));
        }
        self.remaining -= s.len();
        self.written += s.len();
        Ok(s.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) struct Text {
    text: String,
    budget: Budget,
}
impl Text {
    pub(super) fn new(budget: &Budget, initial: &str) -> Result<Self, RenderLimit> {
        let mut out = Self {
            text: String::new(),
            budget: budget.clone(),
        };
        fmt::Write::write_str(&mut out, initial)?;
        Ok(out)
    }
    pub(super) fn push_str(&mut self, s: &str) -> fmt::Result {
        fmt::Write::write_str(self, s)
    }
    pub(super) fn finish(self) -> String {
        self.text
    }
}
impl fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.budget.charge(s.len()).map_err(|_| fmt::Error)?;
        let required = self.text.len().checked_add(s.len()).ok_or(fmt::Error)?;
        if required > self.text.capacity() {
            let ceiling = required.saturating_add(self.budget.remaining());
            let capacity =
                required.max(self.text.capacity().saturating_mul(2).max(64).min(ceiling));
            self.text
                .try_reserve_exact(capacity - self.text.len())
                .map_err(|_| fmt::Error)?;
        }
        self.text.push_str(s);
        Ok(())
    }
}
impl io::Write for Text {
    fn write(&mut self, s: &[u8]) -> io::Result<usize> {
        let s = std::str::from_utf8(s).map_err(io::Error::other)?;
        self.push_str(s)
            .map_err(|_| io::Error::other("artifact output bound"))?;
        Ok(s.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(crate) struct Safe<T>(pub T);
impl<T: fmt::Display> fmt::Display for Safe<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Escape<'a, 'b>(&'a mut fmt::Formatter<'b>);
        impl fmt::Write for Escape<'_, '_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                for c in s.chars() {
                    match c {
                        '|' => self.0.write_str("\\|")?,
                        '\r' | '\n' => self.0.write_str(" ")?,
                        c => fmt::Write::write_char(self.0, c)?,
                    }
                }
                Ok(())
            }
        }
        fmt::write(&mut Escape(f), format_args!("{}", self.0))
    }
}
pub(crate) struct Replace<T>(pub T, pub &'static [(char, &'static str)]);
impl<T: fmt::Display> fmt::Display for Replace<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Adapter<'a, 'b>(&'a mut fmt::Formatter<'b>, &'static [(char, &'static str)]);
        impl fmt::Write for Adapter<'_, '_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                for c in s.chars() {
                    if let Some((_, text)) = self.1.iter().find(|(needle, _)| *needle == c) {
                        self.0.write_str(text)?
                    } else {
                        fmt::Write::write_char(self.0, c)?
                    }
                }
                Ok(())
            }
        }
        fmt::write(&mut Adapter(f, self.1), format_args!("{}", self.0))
    }
}
/// The name a node other than an Endpoint, DocumentedRoute or Environment is
/// shown under, borrowed from the node.
pub(crate) fn plain_name(n: &Node) -> &str {
    text_prop(
        n,
        &[
            "title",
            "name",
            "identity",
            "logical_id",
            "route",
            "path",
            "reason",
        ],
    )
    .unwrap_or(&n.id)
}

pub(crate) struct NodeName<'a>(pub &'a Node);
impl fmt::Display for NodeName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        if matches!(n.label.as_str(), "Endpoint" | "DocumentedRoute")
            && let (Some(method), Some(path)) =
                (n.props["method"].as_str(), n.props["path"].as_str())
        {
            return write!(f, "{method} {path}");
        }
        // An environment is named only by what its file states (ADR-0070):
        // without a stated name it is shown by its id, never by its path.
        if n.label == "Environment" {
            return f.write_str(text_prop(n, &["name"]).unwrap_or(&n.id));
        }
        f.write_str(plain_name(n))
    }
}
pub(crate) struct EvidenceText<'a>(pub &'a Provenance);
impl fmt::Display for EvidenceText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.evidence.is_empty() {
            return f.write_str("—");
        }
        for (i, e) in self.0.evidence.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(
                f,
                "{}:{} bytes {}..{} @ {}",
                e.repo, e.path, e.byte_start, e.byte_end, e.commit_sha
            )?;
        }
        Ok(())
    }
}
pub(crate) struct MethodLabel<'a>(pub &'a core_prov::ProducingMethod);
impl fmt::Display for MethodLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use core_prov::ProducingMethod::*;
        match self.0 {
            Rule { rule, version } => write!(f, "rule {rule}@{version}"),
            Model { model } => write!(f, "model {model}"),
            Human { actor } => write!(f, "human {actor}"),
            Unrecorded => f.write_str("not recorded"),
        }
    }
}
pub(super) struct Join<'a>(pub &'a [&'a str], pub &'static str);
impl fmt::Display for Join<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(self.1)?;
            }
            f.write_str(s)?;
        }
        Ok(())
    }
}
struct Prefix<T>(&'static str, T);
impl<T: fmt::Display> fmt::Display for Prefix<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.0, self.1)
    }
}
struct EdgeName<'a>(&'a Edge);
impl fmt::Display for EdgeName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.0.src, self.0.label, self.0.dst)
    }
}
pub(super) struct DisplayValue<'a>(pub(super) &'a dyn fmt::Display);
impl Serialize for DisplayValue<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self.0)
    }
}
#[derive(Serialize)]
struct AssertionView<'a> {
    id: DisplayValue<'a>,
    subject_id: DisplayValue<'a>,
    subject_kind: &'a str,
    summary: DisplayValue<'a>,
    provenance: &'a Provenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    omission: Option<&'a Omission>,
}
pub(super) struct Assertions {
    budget: Budget,
    pub values: Vec<SpecAssertion>,
    /// The fact-key shape of each value's subject, in step with `values`
    /// (ADR-0058). Not part of any artifact.
    pub subjects: Vec<Subject>,
}
impl Assertions {
    pub(super) fn new(budget: &Budget) -> Self {
        Self {
            budget: budget.clone(),
            values: Vec::new(),
            subjects: Vec::new(),
        }
    }
    fn add(
        &mut self,
        id: &dyn fmt::Display,
        subject: &dyn fmt::Display,
        kind: &str,
        summary: &dyn fmt::Display,
        prov: Provenance,
        metadata: (Option<&str>, Option<&Omission>, Subject),
    ) -> Result<(), RenderLimit> {
        let (label, omission, shape) = metadata;
        let view = AssertionView {
            id: DisplayValue(id),
            subject_id: DisplayValue(subject),
            subject_kind: kind,
            summary: DisplayValue(summary),
            provenance: &prov,
            edge_label: label,
            omission,
        };
        self.budget.admit(&view, !self.values.is_empty())?;
        self.values.push(SpecAssertion {
            id: id.to_string(),
            subject_id: subject.to_string(),
            subject_kind: kind.into(),
            summary: summary.to_string(),
            provenance: prov,
            edge_label: label.map(str::to_owned),
            omission: omission.cloned(),
        });
        self.subjects.push(shape);
        Ok(())
    }
    pub(super) fn node(&mut self, node: &Node, label: Option<&str>) -> Result<(), RenderLimit> {
        let id = Prefix("node:", &node.id);
        self.budget.check(&DisplayValue(&id))?;
        let prov = provenance(&node.props, &id.to_string());
        let omission = gap_omission(&prov, &node.props, &NodeName(node).to_string());
        self.add(
            &id,
            &node.id,
            &node.label,
            &format_args!("{}: {}", node.label, NodeName(node)),
            prov,
            (label, omission.as_ref(), Subject::Node),
        )
    }
    pub(super) fn edge(&mut self, edge: &Edge) -> Result<(), RenderLimit> {
        let subject = EdgeName(edge);
        let shape = Subject::edge(&edge.src, &edge.label);
        let id = Prefix("edge:", &subject);
        self.budget.check(&DisplayValue(&id))?;
        let prov = provenance(&edge.props, &id.to_string());
        let omission = gap_omission(
            &prov,
            &edge.props,
            &format!("unresolved {} edge", edge.label),
        );
        if prov.confidence_tier == ConfidenceTier::Gap
            && let Some(reason) = edge.props["reason"].as_str()
        {
            self.add(
                &id,
                &subject,
                &edge.label,
                &format_args!("{}: {reason}", edge.label),
                prov,
                (Some(&edge.label), omission.as_ref(), shape),
            )
        } else {
            self.add(
                &id,
                &subject,
                &edge.label,
                &subject,
                prov,
                (Some(&edge.label), omission.as_ref(), shape),
            )
        }
    }
    pub(super) fn support(
        &mut self,
        id: &str,
        support: &capabilities::Support<'_>,
    ) -> Result<(), RenderLimit> {
        match support {
            capabilities::Support::Node(node) => self.add(
                &id,
                &node.id,
                &node.label,
                &format_args!("{}: {}", node.label, NodeName(node)),
                provenance(&node.props, &node.id),
                (None, None, Subject::Node),
            ),
            capabilities::Support::Edge(edge) => self.add(
                &id,
                &EdgeName(edge),
                &edge.label,
                &EdgeName(edge),
                provenance(&edge.props, &edge_identity(edge)),
                (
                    Some(&edge.label),
                    None,
                    Subject::edge(&edge.src, &edge.label),
                ),
            ),
        }
    }
    pub(super) fn hop(
        &mut self,
        flow: &Flow,
        hop: &Hop,
        index: usize,
        origins: &Origins<'_>,
    ) -> Result<(), RenderLimit> {
        let omission = origins.omission(hop);
        self.add(
            &format_args!("flow:{}:{index}:{}:{}", flow.trigger, hop.label, hop.dst),
            &format_args!("{} {} {}", hop.src, hop.label, hop.dst),
            "FlowHop",
            &format_args!("{}: {} → {}", hop.label, hop.src_name, hop.dst_name),
            hop.provenance.clone(),
            // A hop names the relation it walked, or no stored fact at all
            // (a projection Gap); either way it is cited by its own key.
            (
                None,
                omission.as_deref(),
                Subject::edge(&hop.src, &hop.label),
            ),
        )
    }
    pub(super) fn tool(&mut self, tool: &Node, paths: &[&str]) -> Result<(), RenderLimit> {
        let display = tool.props["display"].as_str().unwrap_or(&tool.id);
        let category = tool.props["category"].as_str().unwrap_or("—");
        let prov = provenance(&tool.props, &tool.id);
        let omission = gap_omission(&prov, &tool.props, TOOLCHAIN_UNRESOLVED);
        self.add(
            &format_args!("tool:{}", tool.id),
            &tool.id,
            "Tool",
            &format_args!(
                "Toolchain: {display} ({category}) defined in {}",
                Join(paths, ", ")
            ),
            prov,
            (None, omission.as_ref(), Subject::Node),
        )
    }
    /// A fact derived from a stored one and carrying its own provenance (a
    /// permission site inside an endpoint fact): cited by its own id.
    pub(super) fn derived(
        &mut self,
        id: &dyn fmt::Display,
        kind: &str,
        summary: &dyn fmt::Display,
        prov: Provenance,
    ) -> Result<(), RenderLimit> {
        self.add(id, id, kind, summary, prov, (None, None, Subject::Node))
    }
    pub(super) fn journey(
        &mut self,
        candidate: &crate::rules::journey::JourneyCandidate,
    ) -> Result<(), RenderLimit> {
        self.add(
            &candidate.id,
            &candidate.id,
            "JourneyCandidate",
            &format_args!(
                "Journey candidate: {} (basis {}, result {})",
                candidate.label,
                candidate.goal.goal_basis.as_str(),
                candidate.result.as_str()
            ),
            candidate.provenance.clone(),
            // Derived, never stored: cited by the candidate's own id.
            (None, None, Subject::Node),
        )
    }
}
pub(super) fn inline(out: &mut impl fmt::Write, assertions: &[SpecAssertion]) -> fmt::Result {
    out.write_str("\n## Assertions and inline provenance\n\n")?;
    if assertions.is_empty() {
        return out.write_str("No graph-backed assertions were recovered for this artifact.\n");
    }
    out.write_str("| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|---|---|---|---|---|---|\n")?;
    for a in assertions {
        let p = &a.provenance;
        writeln!(
            out,
            "| {} | {:?} | {:?} | {} | {} | `{}` | `{}` |",
            Safe(&a.summary),
            p.tier,
            p.confidence_tier,
            Safe(MethodLabel(&p.method)),
            Safe(EvidenceText(p)),
            Safe(&p.extractor_id),
            Safe(&p.content_hash)
        )?;
    }
    Ok(())
}
/// Literal Markdown text for recovered user stories (#1453): the rule
/// inventory's escaper plus GFM strikethrough `~`, which that shared escaper
/// leaves alone. Bare-URL autolinks are not suppressed.
pub(super) struct Literal<T>(pub T);
impl<T: fmt::Display> fmt::Display for Literal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Replace(crate::rules::render::Escaped(&self.0), &[('~', "\\~")]).fmt(f)
    }
}
/// [`inline`] for recovered user stories (#1453): every cell is [`Literal`]
/// Markdown text, so stored HTML, fences, backticks, headings, strikethrough
/// and newlines stay inert. Structured assertions keep their exact provenance.
pub(super) fn inline_literal(
    out: &mut impl fmt::Write,
    assertions: &[SpecAssertion],
) -> fmt::Result {
    out.write_str("\n## Assertions and inline provenance\n\n")?;
    if assertions.is_empty() {
        return out.write_str("No graph-backed assertions were recovered for this artifact.\n");
    }
    out.write_str("| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|---|---|---|---|---|---|\n")?;
    for a in assertions {
        let p = &a.provenance;
        writeln!(
            out,
            "| {} | {:?} | {:?} | {} | {} | {} | {} |",
            Literal(&a.summary),
            p.tier,
            p.confidence_tier,
            Literal(MethodLabel(&p.method)),
            Literal(EvidenceText(p)),
            Literal(&p.extractor_id),
            Literal(&p.content_hash)
        )?;
    }
    Ok(())
}
pub(super) struct Json<'a>(pub &'a serde_json::Value);
impl fmt::Display for Json<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Serialized(self.0).fmt(f)
    }
}
pub(super) struct Serialized<'a, T>(pub &'a T);
impl<T: Serialize> fmt::Display for Serialized<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Adapter<'a, 'b>(&'a mut fmt::Formatter<'b>);
        impl io::Write for Adapter<'_, '_> {
            fn write(&mut self, s: &[u8]) -> io::Result<usize> {
                let text = std::str::from_utf8(s).map_err(io::Error::other)?;
                self.0
                    .write_str(text)
                    .map_err(|_| io::Error::other("format limit"))?;
                Ok(s.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Adapter(f), self.0).map_err(|_| fmt::Error)
    }
}

// Parsed once per admitted source fact; cheap clones borrow the cached detail
// through admission. Only returned assertions clone the detail after admission.
#[derive(Clone)]
pub(super) struct SharedOmission(std::sync::Arc<Omission>);
impl std::ops::Deref for SharedOmission {
    type Target = Omission;
    fn deref(&self) -> &Omission {
        &self.0
    }
}
impl Serialize for SharedOmission {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(s)
    }
}
/// Fixed Gap omission detail, charged by the typed accounting.
pub(crate) const TOOLCHAIN_UNRESOLVED: &str = "toolchain fact unresolved";
pub(super) fn shared_gap(
    prov: &Provenance,
    props: &serde_json::Value,
    fallback: &str,
) -> Option<SharedOmission> {
    gap_omission(prov, props, fallback).map(|o| SharedOmission(std::sync::Arc::new(o)))
}
struct Origin {
    cached: Option<SharedOmission>,
}
impl Origin {
    fn new(props: &serde_json::Value) -> Self {
        Self {
            cached: Omission::recorded(props).map(|o| SharedOmission(std::sync::Arc::new(o))),
        }
    }
    fn resolve(&self, fallback: &str) -> SharedOmission {
        self.cached
            .clone()
            .unwrap_or_else(|| SharedOmission(std::sync::Arc::new(Omission::classify(fallback))))
    }
}
/// The stored origin of each gap fact, by its key. Only gap facts are held,
/// so a large graph's resolved facts take no entry (#2215).
pub(super) struct Origins<'a> {
    edges: BTreeMap<(&'a str, &'a str, &'a str), Origin>,
    nodes: BTreeMap<&'a str, Origin>,
}
impl<'a> Origins<'a> {
    pub(super) fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        Self {
            edges: edges
                .iter()
                .filter(|e| provenance(&e.props, "").confidence_tier == ConfidenceTier::Gap)
                .map(|e| {
                    (
                        (e.src.as_str(), e.label.as_str(), e.dst.as_str()),
                        Origin::new(&e.props),
                    )
                })
                .collect(),
            nodes: nodes
                .iter()
                .filter(|n| is_gap_node(n))
                .map(|n| (n.id.as_str(), Origin::new(&n.props)))
                .collect(),
        }
    }
    pub(super) fn omission(&self, hop: &Hop) -> Option<SharedOmission> {
        if hop.provenance.confidence_tier != ConfidenceTier::Gap {
            return None;
        }
        let generated;
        let fallback = if let Some(reason) = hop.gap_reason.as_deref() {
            reason
        } else {
            generated = format!("unresolved {} hop", hop.label);
            &generated
        };
        Some(
            self.edges
                .get(&hop.stored_edge())
                .or_else(|| self.nodes.get(hop.dst.as_str()))
                .map(|o| o.resolve(fallback))
                .unwrap_or_else(|| {
                    SharedOmission(std::sync::Arc::new(Omission::classify(fallback)))
                }),
        )
    }
}

pub(super) fn logical_fixed_headers() -> Option<usize> {
    super::accounting::sum(&[
        std::mem::size_of::<State>(),
        std::mem::size_of::<Text>(),
        std::mem::size_of::<Assertions>(),
        std::mem::size_of::<Counter>(),
        std::mem::size_of::<AssertionView<'_>>(),
    ])
}

pub(super) fn logical_origin_bytes(rows: usize) -> Option<usize> {
    use super::accounting::{map_slots, slots, sum};
    sum(&[
        std::mem::size_of::<Origins<'_>>(),
        map_slots::<(&str, &str, &str), Origin>(rows)?,
        map_slots::<&str, Origin>(rows)?,
        slots::<Omission>(rows)?,
        slots::<[usize; 2]>(rows)?, // cached Origin Arc counters, beside candidate Arcs
        super::accounting::typed::omission_heap(0, rows)?, // fixed classifier text per cached omission
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;
    #[test]
    fn origin_bound_charges_cached_arcs_beside_candidate_arcs() {
        // AC-0522: a zero-hop explicit Gap holds a cached Origin Arc and,
        // separately, its candidate's shared_gap Arc; each owns counters.
        let one = logical_origin_bytes(1).unwrap() - logical_origin_bytes(0).unwrap();
        let fixed = super::super::accounting::typed::omission_heap(0, 1).unwrap();
        assert!(one >= std::mem::size_of::<Omission>() + 2 * std::mem::size_of::<usize>() + fixed);
    }
    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn assertions_refuse_large_owned_fields_before_materialization() {
        let large = "quote\"\n\0é".repeat(30_000);
        let budget = Budget::new(Some(512)).unwrap();
        let mut assertions = Assertions::new(&budget);
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![],
            "test",
            b"test",
        )
        .unwrap();
        let mut node = Node {
            id: "node".into(),
            label: "Capability".into(),
            props: serde_json::json!({"name":large,"prov":prov}),
        };
        assert!(assertions.node(&node, None).is_err());
        assert!(assertions.values.is_empty());
        assert_eq!(budget.remaining(), 510);
        node.props["name"] = serde_json::json!("small");
        node.id = large.clone();
        assert!(assertions.node(&node, None).is_err());
        assert!(assertions.values.is_empty());
        assert_eq!(budget.remaining(), 510);
        node.id = "node".into();
        node.props["prov"]["method"] =
            serde_json::to_value(core_prov::ProducingMethod::Model { model: large }).unwrap();
        assert!(assertions.node(&node, None).is_err());
        assert!(assertions.values.is_empty());
        assert_eq!(budget.remaining(), 510);
    }
    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn cached_origin_omission_is_borrowed_through_admission() {
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Gap,
            vec![],
            "test",
            b"gap",
        )
        .unwrap();
        let detail = "quoted\"\n\0é".repeat(20_000);
        let edge = Edge {
            src: "consumer".into(),
            dst: "channel".into(),
            label: "SUBSCRIBES".into(),
            props: serde_json::json!({"prov":prov,"omission":{"class":"product_gap","seam":"requirement","detail":detail}}),
        };
        let origins = Origins::new(&[], &[&edge]);
        let hop = Hop {
            label: "SUBSCRIBES".into(),
            src: "channel".into(),
            dst: "consumer".into(),
            src_name: "channel".into(),
            dst_name: "consumer".into(),
            tier: "Deterministic".into(),
            confidence: "Gap".into(),
            evidence: None,
            provenance: prov,
            gap_reason: None,
            attempted_tiers: vec![],
        };
        let first = origins.omission(&hop).unwrap();
        let second = origins.omission(&hop).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(first.detail, detail);
        let flow = Flow {
            trigger: "channel".into(),
            trigger_kind: "Channel".into(),
            trigger_name: "channel".into(),
            hops: vec![],
            status: FlowStatus::Partial,
            score: 0.0,
            depth_limited: false,
        };
        let budget = Budget::new(Some(512)).unwrap();
        let mut assertions = Assertions::new(&budget);
        assert!(assertions.hop(&flow, &hop, 0, &origins).is_err());
        assert!(assertions.values.is_empty());
        assert_eq!(budget.remaining(), 510);
    }
    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn text_admits_before_reserve_and_streams_escaped_expansion() {
        let budget = Budget::new(Some(16)).unwrap();
        let mut text = Text::new(&budget, "").unwrap();
        assert!(text.push_str(&"x".repeat(100_000)).is_err());
        assert!(text.text.is_empty());
        assert_eq!(text.text.capacity(), 0);
        let budget = Budget::new(Some(10)).unwrap();
        let mut text = Text::new(&budget, "").unwrap();
        write!(text, "{}", Safe("||||")).unwrap();
        assert_eq!(text.finish(), "\\|\\|\\|\\|");
        assert_eq!(budget.remaining(), 0);
        let budget = Budget::new(Some(9)).unwrap();
        let mut text = Text::new(&budget, "").unwrap();
        assert!(write!(text, "{}", Safe("||||")).is_err());
    }
}
