//! Selected register rendering, with admission before returned text ownership.
use super::*;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::Write;

pub(super) struct RegisterOutput {
    pub(super) markdown: Option<String>,
    pub(super) index: Option<String>,
    pub(super) assertions: Vec<SpecAssertion>,
    /// The fact-key shape of each assertion's subject (ADR-0058).
    pub(super) subjects: Vec<fact_index::Subject>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegisterLimit;

// Only short generated delimiters and decimal ordinals are owned here. Fact
// strings remain borrowed until the complete selected output has been admitted.
#[derive(Clone, Eq)]
struct Text<'a>(Vec<Cow<'a, str>>);
impl<'a> Text<'a> {
    fn new(parts: impl IntoIterator<Item = &'a str>) -> Self {
        Self(parts.into_iter().map(Cow::Borrowed).collect())
    }
    fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().flat_map(|part| part.bytes())
    }
}
/// Most parts any Candidate subject id holds; bounds `supports_listed` scratch.
const SUBJECT_ID_PARTS: usize = 5;
fn node_subject(id: &str) -> Text<'_> {
    Text::new([id])
}
fn link_subject<'a>(src: &'a str, label: &'a str, dst: &'a str) -> Text<'a> {
    let parts: [&'a str; SUBJECT_ID_PARTS] = [src, " ", label, " ", dst];
    Text::new(parts)
}
impl PartialEq for Text<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes().eq(other.bytes())
    }
}
impl Ord for Text<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}
impl PartialOrd for Text<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl std::fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in &self.0 {
            f.write_str(part)?;
        }
        Ok(())
    }
}
impl Serialize for Text<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
#[derive(Serialize)]
struct Candidate<'a> {
    id: Text<'a>,
    subject_id: Text<'a>,
    subject_kind: &'a str,
    summary: Text<'a>,
    provenance: Provenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    omission: Option<render::SharedOmission>,
    /// How `subject_id` maps onto a fact key; never serialized (ADR-0058).
    #[serde(skip)]
    subject: fact_index::Subject,
}
impl Candidate<'_> {
    /// The owned assertion and its fact key, moving the decoded provenance
    /// instead of copying it, so a register's rows and its assertions never hold two
    /// copies of every row's provenance at once (#2215).
    fn into_owned(self) -> (SpecAssertion, fact_index::Subject) {
        #[cfg(test)]
        OWNED_OUTPUTS.with(|n| n.set(n.get() + 1));
        let assertion = SpecAssertion {
            id: self.id.to_string(),
            subject_id: self.subject_id.to_string(),
            subject_kind: self.subject_kind.to_string(),
            summary: self.summary.to_string(),
            provenance: self.provenance,
            edge_label: self.edge_label.map(String::from),
            omission: self.omission.as_deref().cloned(),
        };
        (assertion, self.subject)
    }
}
fn candidate_provenance(props: &serde_json::Value, identity: &Text<'_>) -> Provenance {
    serde_json::from_value::<Provenance>(crate::stored_prov(props).into_owned())
        .ok()
        .filter(|p| p.validate().is_ok())
        .unwrap_or_else(|| {
            let mut hash = blake3::Hasher::new();
            for fragment in &identity.0 {
                hash.update(fragment.as_bytes());
            }
            Provenance {
                tier: Tier::Deterministic,
                confidence_tier: ConfidenceTier::Gap,
                evidence: Vec::new(),
                extractor_id: "spec.invalid-provenance".into(),
                content_hash: hash.finalize().to_hex().to_string(),
                method: core_prov::ProducingMethod::Unrecorded,
                basis: None,
            }
        })
}
fn node_name_text(node: &Node) -> Text<'_> {
    if node.label == "Endpoint"
        && let (Some(method), Some(path)) =
            (node.props["method"].as_str(), node.props["path"].as_str())
    {
        return Text::new([method, " ", path]);
    }
    Text::new([text_prop(
        node,
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
    .unwrap_or(&node.id)])
}
fn node_candidate<'a>(node: &'a Node, labels: &GapEdgeLabels<'a>) -> Candidate<'a> {
    let mut summary = Text::new([node.label.as_str(), ": "]);
    summary.0.extend(node_name_text(node).0);
    let provenance = candidate_provenance(&node.props, &Text::new(["node:", &node.id]));
    let omission = render::shared_gap(&provenance, &node.props, &node_name_text(node).to_string());
    Candidate {
        id: Text::new(["node:", &node.id]),
        subject_id: node_subject(&node.id),
        subject_kind: &node.label,
        summary,
        provenance,
        omission,
        edge_label: gap_node_edge_label(node, labels),
        subject: fact_index::Subject::Node,
    }
}
fn edge_candidate(edge: &Edge) -> Candidate<'_> {
    let subject_id = link_subject(&edge.src, &edge.label, &edge.dst);
    let mut id = Text::new(["edge:"]);
    id.0.extend(subject_id.0.clone());
    let provenance = candidate_provenance(&edge.props, &id);
    let summary = if provenance.confidence_tier == ConfidenceTier::Gap
        && let Some(reason) = edge.props["reason"].as_str()
    {
        Text::new([edge.label.as_str(), ": ", reason])
    } else {
        subject_id.clone()
    };
    let omission = render::shared_gap(
        &provenance,
        &edge.props,
        &format!("unresolved {} edge", edge.label),
    );
    Candidate {
        id,
        subject_id,
        omission,
        subject_kind: &edge.label,
        summary,
        provenance,
        edge_label: Some(&edge.label),
        subject: fact_index::Subject::edge(&edge.src, &edge.label),
    }
}
fn hop_candidate<'a>(
    flow: &'a Flow,
    hop: &'a Hop,
    ordinal: usize,
    origins: &render::Origins<'_>,
) -> Candidate<'a> {
    let mut id = Text::new(["flow:", &flow.trigger, ":"]);
    id.0.push(Cow::Owned(ordinal.to_string()));
    id.0.extend([
        Cow::Borrowed(":"),
        Cow::Borrowed(hop.label.as_str()),
        Cow::Borrowed(":"),
        Cow::Borrowed(hop.dst.as_str()),
    ]);
    Candidate {
        id,
        subject_id: link_subject(&hop.src, &hop.label, &hop.dst),
        subject_kind: "FlowHop",
        summary: Text::new([
            hop.label.as_str(),
            ": ",
            &hop.src_name,
            " → ",
            &hop.dst_name,
        ]),
        provenance: hop.provenance.clone(),
        edge_label: None,
        omission: origins.omission(hop),
        // A hop names the relation it walked, or no stored fact at all (a
        // projection Gap); either way it is cited by its own key.
        subject: fact_index::Subject::edge(&hop.src, &hop.label),
    }
}

// The same rendering pass first counts without retaining bytes and then emits.
// Both fmt and serde writes fail before crossing the cumulative UTF-8 ceiling.
struct Sink {
    bytes: usize,
    limit: usize,
    output: Option<String>,
}
impl Sink {
    fn count(limit: Option<usize>) -> Self {
        Self {
            bytes: 0,
            limit: limit.unwrap_or(usize::MAX),
            output: None,
        }
    }
    fn emit(bytes: usize) -> Result<Self, RegisterLimit> {
        #[cfg(test)]
        OWNED_OUTPUTS.with(|n| n.set(n.get() + 1));
        let mut output = String::new();
        output.try_reserve_exact(bytes).map_err(|_| RegisterLimit)?;
        Ok(Self {
            bytes: 0,
            limit: bytes,
            output: Some(output),
        })
    }
    fn json(&mut self, value: &impl Serialize) -> Result<(), RegisterLimit> {
        serde_json::to_writer(self, value).map_err(|_| RegisterLimit)
    }
}
impl std::fmt::Write for Sink {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let next = self
            .bytes
            .checked_add(text.len())
            .filter(|n| *n <= self.limit)
            .ok_or(std::fmt::Error)?;
        if let Some(output) = &mut self.output {
            output.push_str(text);
        }
        self.bytes = next;
        Ok(())
    }
}
impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        std::fmt::Write::write_str(
            self,
            std::str::from_utf8(bytes).map_err(std::io::Error::other)?,
        )
        .map_err(|_| std::io::Error::other("register byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl From<std::fmt::Error> for RegisterLimit {
    fn from(_: std::fmt::Error) -> Self {
        Self
    }
}
struct Safe<'a, T: ?Sized>(&'a T);
struct Escape<'a, 'b>(&'a mut std::fmt::Formatter<'b>);
impl std::fmt::Write for Escape<'_, '_> {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        for ch in value.chars() {
            match ch {
                '|' => self.0.write_str("\\|")?,
                '\r' | '\n' => self.0.write_str(" ")?,
                _ => self.0.write_char(ch)?,
            }
        }
        Ok(())
    }
}
impl<T: std::fmt::Display + ?Sized> std::fmt::Display for Safe<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(Escape(f), "{}", self.0)
    }
}
struct Evidence<'a>(&'a Provenance);
impl std::fmt::Display for Evidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
struct Method<'a>(&'a core_prov::ProducingMethod);
impl std::fmt::Display for Method<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use core_prov::ProducingMethod::*;
        match self.0 {
            Rule { rule, version } => write!(f, "rule {rule}@{version}"),
            Model { model } => write!(f, "model {model}"),
            Human { actor } => write!(f, "human {actor}"),
            Unrecorded => f.write_str("not recorded"),
        }
    }
}
fn assertion_rows(out: &mut Sink, assertions: &[&Candidate<'_>]) -> Result<(), RegisterLimit> {
    out.write_str("\n## Assertions and inline provenance\n\n")?;
    if assertions.is_empty() {
        out.write_str("No graph-backed assertions were recovered for this artifact.\n")?;
        return Ok(());
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
            Safe(&Method(&p.method)),
            Safe(&Evidence(p)),
            Safe(&p.extractor_id),
            Safe(&p.content_hash)
        )?;
    }
    Ok(())
}
fn matrix_row_selected(out: &mut Sink, edge: &Edge) -> Result<(), RegisterLimit> {
    writeln!(
        out,
        "| `{}` | {} | `{}` |",
        Safe(&edge.src),
        edge.label,
        Safe(&edge.dst)
    )?;
    Ok(())
}

type MatrixClasses<'a> = Vec<((&'a str, &'a str), Vec<usize>)>;
#[derive(Serialize)]
struct MatrixJson<'a, 'b> {
    schema: &'static str,
    mode: ExportMode,
    links: usize,
    classes: Vec<MatrixJsonClass<'a, 'b>>,
}
#[derive(Serialize)]
struct MatrixJsonClass<'a, 'b> {
    id: String,
    relation: &'a str,
    target: &'a str,
    links: usize,
    members: Vec<&'b Text<'a>>,
}
fn matrix_json<'a>(
    out: &mut Sink,
    rows: &[(&'a Edge, Candidate<'a>)],
    classes: &MatrixClasses<'a>,
    mode: ExportMode,
) -> Result<(), RegisterLimit> {
    out.json(&MatrixJson {
        schema: "cartograph.us-tm-index/v1",
        mode,
        links: rows.len(),
        classes: classes
            .iter()
            .enumerate()
            .map(|(i, ((relation, target), members))| MatrixJsonClass {
                id: matrix_class_id(i),
                relation,
                target,
                links: members.len(),
                members: members.iter().map(|i| &rows[*i].1.id).collect(),
            })
            .collect(),
    })?;
    out.write_char('\n')?;
    Ok(())
}
fn matrix_markdown(
    out: &mut Sink,
    rows: &[(&Edge, Candidate<'_>)],
    classes: &MatrixClasses<'_>,
) -> Result<(), RegisterLimit> {
    out.write_str("# Recovered US traceability matrix\n\n")?;
    if rows.len() <= US_TM_FLAT_LIMIT {
        out.write_str("| Source | Relation | Target |\n|---|---|---|\n")?;
        for (edge, _) in rows {
            matrix_row_selected(out, edge)?;
        }
        if rows.is_empty() {
            out.write_str("| — | No recovered mappings | — |\n")?;
        }
        assertion_rows(out, &rows.iter().map(|(_, a)| a).collect::<Vec<_>>())?;
    } else {
        let shown = &classes[..classes.len().min(US_TM_MAX_CLASSES)];
        writeln!(
            out,
            "{} recovered links in {} relation × target classes, largest first. Each class lists up to {US_TM_CLASS_REPRESENTATIVES} representative links; `US-TM.json` lists every link by class, and each link keeps its inline provenance in the bundle's assertions.\n",
            rows.len(),
            classes.len()
        )?;
        out.write_str("| Class | Relation | Target | Links |\n|---|---|---|---|\n")?;
        for (i, ((relation, target), members)) in shown.iter().enumerate() {
            writeln!(
                out,
                "| {} | {relation} | `{}` | {} |",
                matrix_class_id(i),
                Safe(target),
                members.len()
            )?;
        }
        let hidden = &classes[shown.len()..];
        if !hidden.is_empty() {
            writeln!(
                out,
                "| … | {} more classes, not detailed here — listed in `US-TM.json` | — | {} |",
                hidden.len(),
                hidden
                    .iter()
                    .map(|(_, members)| members.len())
                    .sum::<usize>()
            )?;
        }
        let mut representatives = Vec::new();
        for (i, ((relation, target), members)) in shown.iter().enumerate() {
            writeln!(
                out,
                "\n## {} — {relation} `{}` ({} links)\n\n| Source | Relation | Target |\n|---|---|---|",
                matrix_class_id(i),
                Safe(target),
                members.len()
            )?;
            for ordinal in members.iter().take(US_TM_CLASS_REPRESENTATIVES) {
                let (edge, assertion) = &rows[*ordinal];
                matrix_row_selected(out, edge)?;
                representatives.push(assertion);
            }
            let more = members.len().saturating_sub(US_TM_CLASS_REPRESENTATIVES);
            if more > 0 {
                writeln!(
                    out,
                    "\n… {more} more links in this class — listed in `US-TM.json` under `{}`.",
                    matrix_class_id(i)
                )?;
            }
        }
        assertion_rows(out, &representatives)?;
        writeln!(
            out,
            "\nInline provenance is shown for the {} representative links above; the other {} carry theirs in the bundle's structured assertions.",
            representatives.len(),
            rows.len() - representatives.len()
        )?;
    }
    Ok(())
}
/// The limit covers selected content plus each selected artifact's serialized
/// assertion array. Only Markdown owns the complete assertion array.
pub(super) fn matrix_selected(
    edges: &[&Edge],
    mode: ExportMode,
    markdown: bool,
    index: bool,
    max_bytes: Option<usize>,
) -> Result<RegisterOutput, RegisterLimit> {
    if !markdown && !index {
        return Ok(RegisterOutput {
            markdown: None,
            index: None,
            assertions: Vec::new(),
            subjects: Vec::new(),
        });
    }
    let rows: Vec<_> = edges
        .iter()
        .copied()
        .filter(|e| TRACE_LABELS.contains(&e.label.as_str()))
        .map(|e| (e, edge_candidate(e)))
        .filter(|(_, a)| a.provenance.confidence_tier != ConfidenceTier::Gap)
        .collect();
    let classes = largest_groups(0..rows.len(), |i| {
        (rows[*i].0.label.as_str(), rows[*i].0.dst.as_str())
    });
    let mut count = Sink::count(max_bytes);
    let mut markdown_bytes = 0;
    let mut index_bytes = 0;
    if markdown {
        matrix_markdown(&mut count, &rows, &classes)?;
        markdown_bytes = count.bytes;
        count.json(&rows.iter().map(|(_, a)| a).collect::<Vec<_>>())?;
    }
    if index {
        let start = count.bytes;
        matrix_json(&mut count, &rows, &classes, mode)?;
        index_bytes = count.bytes - start;
        count.json(&Vec::<SpecAssertion>::new())?;
    }
    let markdown = if markdown {
        let mut out = Sink::emit(markdown_bytes)?;
        matrix_markdown(&mut out, &rows, &classes)?;
        out.output
    } else {
        None
    };
    let index = if index {
        let mut out = Sink::emit(index_bytes)?;
        matrix_json(&mut out, &rows, &classes, mode)?;
        out.output
    } else {
        None
    };
    let (assertions, subjects) = if markdown.is_some() {
        rows.into_iter().map(|(_, a)| a.into_owned()).unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(RegisterOutput {
        markdown,
        index,
        assertions,
        subjects,
    })
}

struct CauseClass<'a> {
    cause: Text<'a>,
    extractor: &'a str,
    next_tier: &'static str,
    members: Vec<usize>,
}
fn cause<'a>(assertion: &Candidate<'a>) -> Text<'a> {
    if assertion.subject_kind != "Gap" {
        return Text::new(["unresolved ", assertion.subject_kind, " edge"]);
    }
    let mut result = assertion.summary.clone();
    if result.bytes().take(5).eq(b"Gap: ".iter().copied()) {
        let mut skip = 5;
        result.0 = result
            .0
            .into_iter()
            .filter_map(|part| {
                if skip >= part.len() {
                    skip -= part.len();
                    None
                } else {
                    let start = skip;
                    skip = 0;
                    Some(match part {
                        Cow::Borrowed(s) => Cow::Borrowed(&s[start..]),
                        Cow::Owned(s) => Cow::Owned(s[start..].to_string()),
                    })
                }
            })
            .collect();
    }
    result
}
// Match the historical subject-id split, including ids containing spaces.
fn supports_listed(assertion: &Candidate<'_>, listed: &BTreeSet<Text<'_>>) -> bool {
    let mut token = Vec::new();
    let mut ordinal = 0;
    for part in &assertion.subject_id.0 {
        for (i, fragment) in part.split(' ').enumerate() {
            if i > 0 {
                if (ordinal == 0 || ordinal == 2) && listed.contains(&Text(token.clone())) {
                    return true;
                }
                token.clear();
                ordinal += 1;
            }
            token.push(Cow::Borrowed(fragment));
        }
    }
    (ordinal == 0 || ordinal == 2) && listed.contains(&Text(token))
}
fn gap_groups<'a, 'b>(
    rows: &'b [Candidate<'a>],
) -> (Vec<CauseClass<'b>>, Vec<&'b Text<'a>>, usize) {
    let listed: BTreeSet<_> = rows
        .iter()
        .filter(|a| a.id.bytes().take(5).eq(b"node:".iter().copied()))
        .map(|a| a.subject_id.clone())
        .collect();
    let mut grouped: BTreeMap<(Text<'_>, &str), CauseClass<'_>> = BTreeMap::new();
    let mut supporting = Vec::new();
    let mut findings = 0;
    for (i, a) in rows.iter().enumerate() {
        let finding = a.id.bytes().take(5).eq(b"node:".iter().copied())
            || (a.id.bytes().take(5).eq(b"edge:".iter().copied()) && !supports_listed(a, &listed));
        if !finding {
            supporting.push(&a.id);
            continue;
        }
        findings += 1;
        let cause = cause(a);
        let extractor = a.provenance.extractor_id.as_str();
        grouped
            .entry((cause.clone(), extractor))
            .or_insert_with(|| CauseClass {
                cause,
                extractor,
                next_tier: next_tier(&a.provenance),
                members: Vec::new(),
            })
            .members
            .push(i);
    }
    let mut classes: Vec<_> = grouped.into_values().collect();
    classes.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.cause.cmp(&b.cause))
            .then_with(|| a.extractor.cmp(b.extractor))
    });
    (classes, supporting, findings)
}
struct Short<'a>(&'a Text<'a>);
impl std::fmt::Display for Short<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut chars = self.0.0.iter().flat_map(|s| s.chars());
        for ch in chars.by_ref().take(GAP_CAUSE_MAX_CHARS) {
            f.write_char(ch)?;
        }
        if chars.next().is_some() {
            f.write_char('…')?;
        }
        Ok(())
    }
}
#[derive(Serialize)]
struct GapJson<'a, 'b> {
    schema: &'static str,
    mode: ExportMode,
    findings: usize,
    instances: usize,
    supporting: Vec<&'b Text<'a>>,
    by_omission: Vec<OmissionRollup>,
    classes: Vec<GapJsonClass<'a, 'b>>,
}
#[derive(Serialize)]
struct GapJsonClass<'a, 'b> {
    id: String,
    cause: &'b Text<'a>,
    extractor: &'b str,
    next_tier: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    omission_class: Option<OmissionClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seam: Option<FillSeam>,
    instances: usize,
    members: Vec<&'b Text<'a>>,
}
fn gap_json(
    out: &mut Sink,
    rows: &[Candidate<'_>],
    classes: &[CauseClass<'_>],
    supporting: &[&Text<'_>],
    findings: usize,
    mode: ExportMode,
) -> Result<(), RegisterLimit> {
    out.json(&GapJson {
        schema: "cartograph.gap-register-index/v1",
        mode,
        findings,
        instances: rows.len(),
        supporting: supporting.to_vec(),
        by_omission: omission_rollup(rows, classes),
        classes: classes
            .iter()
            .enumerate()
            .map(|(i, c)| GapJsonClass {
                id: class_id(i),
                cause: &c.cause,
                extractor: c.extractor,
                next_tier: c.next_tier,
                omission_class: uniform_omission(rows, c).map(|o| o.class),
                seam: uniform_omission(rows, c).map(|o| o.seam),
                instances: c.members.len(),
                members: c.members.iter().map(|i| &rows[*i].id).collect(),
            })
            .collect(),
    })?;
    out.write_char('\n')?;
    Ok(())
}
#[derive(Serialize)]
struct OmissionRollup {
    class: OmissionClass,
    seam: FillSeam,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    unclassified: bool,
    findings: usize,
}
fn omission_rollup(rows: &[Candidate<'_>], classes: &[CauseClass<'_>]) -> Vec<OmissionRollup> {
    let mut counts: BTreeMap<(OmissionClass, FillSeam, bool), usize> = BTreeMap::new();
    for i in classes.iter().flat_map(|c| &c.members) {
        if let Some(o) = rows[*i].omission.as_deref() {
            *counts
                .entry((o.class, o.seam, o.is_unclassified()))
                .or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|((class, seam, unclassified), findings)| OmissionRollup {
            class,
            seam,
            unclassified,
            findings,
        })
        .collect()
}
fn uniform_omission<'a>(rows: &'a [Candidate<'_>], class: &CauseClass<'_>) -> Option<&'a Omission> {
    let first = rows[*class.members.first()?].omission.as_deref()?;
    class
        .members
        .iter()
        .all(|i| {
            rows[*i].omission.as_deref().is_some_and(|o| {
                (o.class, o.seam, o.is_unclassified())
                    == (first.class, first.seam, first.is_unclassified())
            })
        })
        .then_some(first)
}
fn omission_cells(omission: Option<&Omission>) -> (String, &'static str) {
    omission.map_or(("—".into(), "—"), |o| (o.class_label(), o.seam.label()))
}
fn render_omission_rollup(out: &mut Sink, rollup: &[OmissionRollup]) -> Result<(), RegisterLimit> {
    if rollup.is_empty() {
        return Ok(());
    }
    out.write_str("\n## By omission class\n\nWhy each finding is missing, and the seam that would fill it.\n\n| Omission class | Filled by | Findings |\n|---|---|---|\n")?;
    for row in rollup {
        let class = if row.unclassified {
            format!("unclassified ({})", row.class.label())
        } else {
            row.class.label().to_string()
        };
        writeln!(out, "| {class} | {} | {} |", row.seam.label(), row.findings)?;
    }
    Ok(())
}
fn gap_row(out: &mut Sink, a: &Candidate<'_>) -> Result<(), RegisterLimit> {
    let (class, seam) = omission_cells(a.omission.as_deref());
    writeln!(
        out,
        "| `{}` | {} | {class} | {seam} |",
        Safe(&a.subject_id),
        Safe(&a.summary)
    )?;
    Ok(())
}
fn gap_markdown(
    out: &mut Sink,
    rows: &[Candidate<'_>],
    classes: &[CauseClass<'_>],
    supporting: usize,
    lane_rows: usize,
    findings: usize,
) -> Result<(), RegisterLimit> {
    out.write_str("# Gap register\n\n")?;
    if rows.len() <= GAP_REGISTER_FLAT_LIMIT {
        out.write_str("| Subject | Reason | Omission class | Filled by |\n|---|---|---|---|\n")?;
        for a in rows {
            gap_row(out, a)?;
        }
        if rows.is_empty() {
            out.write_str("| — | No unresolved facts | — | — |\n")?;
        }
        render_omission_rollup(out, &omission_rollup(rows, classes))?;
        assertion_rows(out, &rows.iter().collect::<Vec<_>>())?;
    } else {
        let shown = &classes[..classes.len().min(GAP_REGISTER_MAX_CLASSES)];
        writeln!(
            out,
            "{findings} open findings · {} register rows: {} findings in {} cause classes (stop reason × extractor), largest first, plus {} supporting rows that restate a listed gap. Each class lists up to {GAP_CLASS_REPRESENTATIVES} representative instances; `gap_register.json` lists every instance by class, and each instance keeps its inline provenance in the bundle's assertions.\n",
            rows.len(),
            lane_rows,
            classes.len(),
            supporting
        )?;
        out.write_str(
            "| Class | Cause | Extractor | Next tier | Omission class | Filled by | Instances |\n|---|---|---|---|---|---|---|\n",
        )?;
        for (i, c) in shown.iter().enumerate() {
            let (omission, seam) = uniform_omission(rows, c)
                .map_or(("mixed".into(), "mixed"), |o| omission_cells(Some(o)));
            writeln!(
                out,
                "| {} | {} | `{}` | {} | {omission} | {seam} | {} |",
                class_id(i),
                Safe(&Short(&c.cause)),
                Safe(c.extractor),
                c.next_tier,
                c.members.len()
            )?;
        }
        let hidden = &classes[shown.len()..];
        if !hidden.is_empty() {
            writeln!(
                out,
                "| … | {} more classes, not detailed here — listed in `gap_register.json` | — | — | — | — | {} |",
                hidden.len(),
                hidden.iter().map(|c| c.members.len()).sum::<usize>()
            )?;
        }
        render_omission_rollup(out, &omission_rollup(rows, classes))?;
        let mut representatives = Vec::new();
        for (i, c) in shown.iter().enumerate() {
            writeln!(
                out,
                "\n## {} — {} ({} instances)\n\n| Subject | Reason | Omission class | Filled by |\n|---|---|---|---|",
                class_id(i),
                Safe(&Short(&c.cause)),
                c.members.len()
            )?;
            for ordinal in c.members.iter().take(GAP_CLASS_REPRESENTATIVES) {
                let member = &rows[*ordinal];
                gap_row(out, member)?;
                representatives.push(member);
            }
            let more = c.members.len().saturating_sub(GAP_CLASS_REPRESENTATIVES);
            if more > 0 {
                writeln!(
                    out,
                    "\n… {more} more instances in this class — listed in `gap_register.json` under `{}`.",
                    class_id(i)
                )?;
            }
        }
        assertion_rows(out, &representatives)?;
        writeln!(
            out,
            "\nInline provenance is shown for the {} representative instances above; the other {} carry theirs in the bundle's structured assertions.",
            representatives.len(),
            rows.len() - representatives.len()
        )?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(super) fn gap_selected(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    findings: usize,
    mode: ExportMode,
    markdown: bool,
    index: bool,
    max_bytes: Option<usize>,
) -> Result<RegisterOutput, RegisterLimit> {
    if !markdown && !index {
        return Ok(RegisterOutput {
            markdown: None,
            index: None,
            assertions: Vec::new(),
            subjects: Vec::new(),
        });
    }
    let origins = render::Origins::new(nodes, edges);
    let labels = GapEdgeLabels::new(edges);
    let mut rows: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| is_gap_node(n))
        .map(|n| node_candidate(n, &labels))
        .collect();
    rows.extend(
        edges
            .iter()
            .copied()
            .filter(|e| is_gap_edge(e))
            .map(edge_candidate),
    );
    rows.extend(flows.iter().flat_map(|f| {
        let origins = &origins;
        f.hops
            .iter()
            .enumerate()
            .filter(|(_, h)| h.provenance.confidence_tier == ConfidenceTier::Gap)
            .map(move |(i, h)| hop_candidate(f, h, i, origins))
    }));
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.dedup_by(|a, b| a.id == b.id);
    let (classes, supporting, lane_rows) = gap_groups(&rows);
    let mut count = Sink::count(max_bytes);
    let mut markdown_bytes = 0;
    let mut index_bytes = 0;
    if markdown {
        gap_markdown(
            &mut count,
            &rows,
            &classes,
            supporting.len(),
            lane_rows,
            findings,
        )?;
        markdown_bytes = count.bytes;
        count.json(&rows)?;
    }
    if index {
        let start = count.bytes;
        gap_json(&mut count, &rows, &classes, &supporting, findings, mode)?;
        index_bytes = count.bytes - start;
        count.json(&Vec::<SpecAssertion>::new())?;
    }
    let markdown = if markdown {
        let mut out = Sink::emit(markdown_bytes)?;
        gap_markdown(
            &mut out,
            &rows,
            &classes,
            supporting.len(),
            lane_rows,
            findings,
        )?;
        out.output
    } else {
        None
    };
    let index = if index {
        let mut out = Sink::emit(index_bytes)?;
        gap_json(&mut out, &rows, &classes, &supporting, findings, mode)?;
        out.output
    } else {
        None
    };
    let (assertions, subjects) = if markdown.is_some() {
        rows.into_iter().map(Candidate::into_owned).unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(RegisterOutput {
        markdown,
        index,
        assertions,
        subjects,
    })
}

#[cfg(test)]
thread_local! { static OWNED_OUTPUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(super) fn logical_workspace_bytes(
    bytes: usize,
    rows: usize,
    values: usize,
    flow_bytes: usize,
    hops: usize,
    flows: usize,
) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum, typed};
    use std::mem::size_of;
    let candidates = rows.checked_add(hops)?;
    let digits = usize::MAX.to_string().len();
    let parts = 8usize.checked_add(5)?.checked_add(5)?; // hop id / subject / summary constructors
    let row_headers = sum(&[
        slots::<Candidate<'_>>(candidates)?,
        slots::<Candidate<'_>>(candidates)?, // rows + stable-sort scratch
        slots::<Cow<'_, str>>(candidates.checked_mul(parts)?)?,
        slots::<Provenance>(rows)?,
        // Graph evidence by admitted values; copied hop evidence by the
        // smallest serialized evidence element of the flow output.
        slots::<core_prov::EvidenceRef>(
            values.checked_add(flowtracer::output_evidence_bound(flow_bytes)?)?,
        )?,
        slots::<Omission>(candidates)?,
        super::render::logical_origin_bytes(rows)?,
        map_slots::<&str, &str>(rows)?, // gap edge labels by destination
        map_slots::<&str, &str>(rows)?, // gap edge labels by source
    ])?;
    let groups = sum(&[
        set_slots::<Text<'_>>(rows)?,
        slots::<Cow<'_, str>>(rows.checked_mul(5)?)?, // listed subjects
        map_slots::<(Text<'_>, &str), CauseClass<'_>>(candidates)?,
        slots::<CauseClass<'_>>(candidates)?,
        slots::<CauseClass<'_>>(candidates)?, // classes + sorting
        slots::<Cow<'_, str>>(candidates.checked_mul(5)?.checked_mul(2)?)?, // map key and class cause
        slots::<usize>(candidates)?,
        slots::<&Text<'_>>(candidates)?, // member/supporting
        slots::<GapJsonClass<'_, '_>>(candidates)?,
        slots::<&Text<'_>>(candidates)?,
        slots::<OmissionRollup>(candidates)?,
        map_slots::<(OmissionClass, FillSeam, bool), usize>(candidates)?,
        map_slots::<(&str, &str), Vec<usize>>(rows)?,
        slots::<((&str, &str), Vec<usize>)>(rows)?,
        slots::<MatrixJsonClass<'_, '_>>(rows)?,
        slots::<usize>(rows)?,
        slots::<&Text<'_>>(rows)?,
    ])?;
    // supports_listed clears its token at every space, so between clears it
    // keeps at most the last fragment of each subject part; the lookup clone
    // is no longer. Neither vector owns those substrings.
    let token = slots::<Cow<'_, str>>(SUBJECT_ID_PARTS)?.checked_mul(2)?;
    sum(&[
        row_headers,
        groups,
        token,
        bytes, // graph candidate provenance payload
        rows.checked_mul(typed::fallback_provenance_heap())?, // fixed Gap fallback strings
        bytes, // cached original omissions
        bytes, // direct node/edge omission details
        typed::omission_heap(0, candidates)?, // fixed fallback/classifier text per omission
        slots::<[usize; 2]>(candidates)?, // shared omission Arc counters
        flow_bytes, // copied hop provenance
        flow_bytes, // uncached hop omission/fallback detail
        hops.checked_mul(digits)?, // owned hop ordinal fragments
        candidates.checked_mul(digits.checked_add("G-".len())?)?, // generated class IDs
        rows.checked_mul(digits.checked_add("M-".len())?)?,
        hops.checked_mul("unresolved  hop".len())?, // fallback fixed text; label payload is in flow_bytes
        flows.checked_mul(size_of::<&str>())?,
        typed::provenance_decode(bytes, values)?, // one current candidate provenance decode
        bytes,                                    // current owned name for the Gap fallback
        size_of::<GapJson<'_, '_>>(),
        size_of::<MatrixJson<'_, '_>>(),
        size_of::<Sink>(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0523: the token probe keeps the historical flattened space split,
    // and no subject id outgrows the parts its scratch is charged for.
    #[test]
    fn subject_token_scratch_is_bounded_by_parts_and_keeps_split() {
        let historical = |subject: &str, listed: &BTreeSet<String>| {
            subject
                .split(' ')
                .enumerate()
                .any(|(i, word)| (i == 0 || i == 2) && listed.contains(word))
        };
        let names = [
            "a",
            "",
            " ",
            "a b",
            "  é ",
            "ü  x",
            " lead",
            "trail ",
            "日本 語",
        ];
        let mut checked = 0;
        for src in names {
            for label in ["CALLS", "", "X Y"] {
                for dst in names {
                    let edge = Edge {
                        src: src.into(),
                        dst: dst.into(),
                        label: label.into(),
                        props: serde_json::json!({}),
                    };
                    let candidate = edge_candidate(&edge);
                    assert!(candidate.subject_id.0.len() <= SUBJECT_ID_PARTS);
                    let flat = candidate.subject_id.to_string();
                    for word in flat.split(' ').chain(names) {
                        let listed = BTreeSet::from([word.to_string()]);
                        let texts = BTreeSet::from([Text::new([word])]);
                        assert_eq!(
                            supports_listed(&candidate, &texts),
                            historical(&flat, &listed),
                            "{flat:?} listed {word:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1000);
        assert_eq!(node_subject("a b c d e f").0.len(), 1);
        assert_eq!(link_subject("a", "b", "c").0.len(), SUBJECT_ID_PARTS);
    }
    fn prov(confidence: ConfidenceTier) -> Provenance {
        Provenance::new(
            Tier::Deterministic,
            confidence,
            vec![core_prov::EvidenceRef {
                repo: "r|é".into(),
                path: "x\n.rs".into(),
                byte_start: 1,
                byte_end: 3,
                commit_sha: "abc".into(),
            }],
            "register-test",
            b"fixture",
        )
        .unwrap()
    }
    fn edge_fixture(i: usize, gap: bool) -> Edge {
        Edge {
            src: format!("src:{i:04}"),
            dst: format!("target:{}", i % 61),
            label: if gap { "CALLS" } else { "MAPS_TO" }.into(),
            props: serde_json::json!({"prov": prov(if gap { ConfidenceTier::Gap } else { ConfidenceTier::Confirmed }), "reason":"why | é\nreason"}),
        }
    }
    fn node_fixture(i: usize) -> Node {
        Node {
            id: format!("gap:{i:04}"),
            label: "Gap".into(),
            props: serde_json::json!({"reason":format!("cause:{} | é\nreason", i % 61), "prov": prov(ConfidenceTier::Gap)}),
        }
    }
    fn total(out: &RegisterOutput) -> usize {
        out.markdown.as_ref().map_or(0, |s| {
            s.len() + serde_json::to_vec(&out.assertions).unwrap().len()
        }) + out.index.as_ref().map_or(0, |s| s.len() + 2)
    }
    #[test]
    fn selected_matrix_parity_and_exact_cumulative_bound() {
        for (count, digest) in [
            (
                0,
                "44d2ab0e30c41e89ff54d7498ef5e55256151d02ab6ddbb3330c347b2be4f901",
            ),
            (
                3,
                "cd090e8e6fa0224fac1381465afca63c4a901a43e362a0ceda7e4ed75dc5e432",
            ),
            (
                201,
                "4b3d53b2b9a9b4d1c97a16d01301f3816eefaa88575698cbcc237453d1c0721c",
            ),
            (
                400,
                "8f7f0706f3962e4797428ead5edcaf5a4cf9eb626d926007ee4ea5fa8db1f6ee",
            ),
        ] {
            let edges: Vec<_> = (0..count).map(|i| edge_fixture(i, false)).collect();
            let refs: Vec<_> = edges.iter().collect();
            let selected =
                matrix_selected(&refs, ExportMode::BestEffort, true, true, None).unwrap();
            // Frozen original full (Markdown, index, assertions) serde bytes.
            let ordinary = (
                selected.markdown.as_ref().unwrap().clone(),
                selected.index.as_ref().unwrap().clone(),
                selected.assertions.clone(),
            );
            assert_eq!(
                blake3::hash(&serde_json::to_vec(&ordinary).unwrap())
                    .to_hex()
                    .as_str(),
                digest
            );
            let bytes = total(&selected);
            assert!(
                matrix_selected(&refs, ExportMode::BestEffort, true, true, Some(bytes)).is_ok()
            );
            assert!(
                matrix_selected(&refs, ExportMode::BestEffort, true, true, Some(bytes - 1))
                    .is_err()
            );
            let only_index = matrix_selected(
                &refs,
                ExportMode::BestEffort,
                false,
                true,
                Some(ordinary.1.len() + 2),
            )
            .unwrap();
            assert!(only_index.markdown.is_none());
            assert!(only_index.assertions.is_empty());
            assert_eq!(only_index.index.as_ref().unwrap(), &ordinary.1);
            let only_markdown = matrix_selected(
                &refs,
                ExportMode::BestEffort,
                true,
                false,
                Some(ordinary.0.len() + serde_json::to_vec(&ordinary.2).unwrap().len()),
            )
            .unwrap();
            assert!(only_markdown.index.is_none());
            assert_eq!(only_markdown.markdown.as_ref().unwrap(), &ordinary.0);
        }
    }
    #[test]
    fn selected_gap_parity_and_exact_cumulative_bound() {
        for (count, digest) in [
            (
                0,
                "8d8d79dd20e85a56c261da17ffc5ef78d95d0ccbffebdafc3f4f28a54deebdd7",
            ),
            (
                3,
                "621a51634b79ef297b8f115db6f1dea99778c362bf186407f02875ea33958cf1",
            ),
            (
                13,
                "5d5974479181b30d43c3d8c23739e4f6d28426b924dfd4f79321b027901d6068",
            ),
            (
                400,
                "382f6ece74ba0643e6d1f346cd5f9a35d566dbcd666b145f4b972c613c546ad9",
            ),
        ] {
            let nodes: Vec<_> = (0..count).map(node_fixture).collect();
            let nodes: Vec<_> = nodes.iter().collect();
            let mut edges: Vec<_> = (0..count).map(|i| edge_fixture(i, true)).collect();
            if count > 0 {
                edges[0].dst = "gap:0000".into();
            }
            let edges: Vec<_> = edges.iter().collect();
            let selected = gap_selected(
                &nodes,
                &edges,
                &[],
                count * 2,
                ExportMode::VerifiedOnly,
                true,
                true,
                None,
            )
            .unwrap();
            // Frozen original full (Markdown, index, assertions) serde bytes.
            let ordinary = (
                selected.markdown.as_ref().unwrap().clone(),
                selected.index.as_ref().unwrap().clone(),
                selected.assertions.clone(),
            );
            // Complete tuple independently matched native #603 03d776d
            // register rendering, incorporating the existing method column.
            assert_eq!(
                blake3::hash(&serde_json::to_vec(&ordinary).unwrap())
                    .to_hex()
                    .as_str(),
                digest
            );
            let bytes = total(&selected);
            assert!(
                gap_selected(
                    &nodes,
                    &edges,
                    &[],
                    count * 2,
                    ExportMode::VerifiedOnly,
                    true,
                    true,
                    Some(bytes)
                )
                .is_ok()
            );
            assert!(
                gap_selected(
                    &nodes,
                    &edges,
                    &[],
                    count * 2,
                    ExportMode::VerifiedOnly,
                    true,
                    true,
                    Some(bytes - 1)
                )
                .is_err()
            );
            let only_index = gap_selected(
                &nodes,
                &edges,
                &[],
                count * 2,
                ExportMode::VerifiedOnly,
                false,
                true,
                Some(ordinary.1.len() + 2),
            )
            .unwrap();
            assert!(only_index.markdown.is_none());
            assert!(only_index.assertions.is_empty());
            assert_eq!(only_index.index.as_ref().unwrap(), &ordinary.1);
        }
    }
    #[test]
    fn failed_admission_never_owns_returned_text_or_assertions() {
        let huge = "é\"\n|".repeat(80_000);
        let mut edge = edge_fixture(0, false);
        edge.src = huge.clone();
        let mut node = node_fixture(0);
        node.props["reason"] = serde_json::json!(huge);
        for (markdown, index) in [(true, false), (false, true), (true, true)] {
            OWNED_OUTPUTS.with(|n| n.set(0));
            assert!(
                matrix_selected(
                    &[&edge],
                    ExportMode::VerifiedOnly,
                    markdown,
                    index,
                    Some(1_000)
                )
                .is_err()
            );
            OWNED_OUTPUTS.with(|n| assert_eq!(n.get(), 0));
            OWNED_OUTPUTS.with(|n| n.set(0));
            assert!(
                gap_selected(
                    &[&node],
                    &[],
                    &[],
                    1,
                    ExportMode::VerifiedOnly,
                    markdown,
                    index,
                    Some(1_000)
                )
                .is_err()
            );
            OWNED_OUTPUTS.with(|n| assert_eq!(n.get(), 0));
        }
        // The grouped Markdown hides most rows, but the complete returned
        // assertions still have to fit before owning even the first one.
        let mut nodes: Vec<_> = (0..20).map(node_fixture).collect();
        nodes[19].props["reason"] = node.props["reason"].clone();
        let refs: Vec<_> = nodes.iter().collect();
        OWNED_OUTPUTS.with(|n| n.set(0));
        assert!(
            gap_selected(
                &refs,
                &[],
                &[],
                20,
                ExportMode::VerifiedOnly,
                true,
                false,
                Some(50_000)
            )
            .is_err()
        );
        OWNED_OUTPUTS.with(|n| assert_eq!(n.get(), 0));
    }
    #[test]
    fn flow_hops_are_deduplicated_sorted_supporting_rows() {
        let hop = Hop {
            src: "source".into(),
            dst: "gap:0000".into(),
            label: "CALLS".into(),
            src_name: "source | é".into(),
            dst_name: "gap\nname".into(),
            tier: "T0".into(),
            confidence: "Gap".into(),
            evidence: None,
            provenance: prov(ConfidenceTier::Gap),
            gap_reason: Some("why".into()),
            attempted_tiers: vec![],
        };
        let flow = Flow {
            trigger: "trigger".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "name".into(),
            hops: vec![hop; 12],
            status: FlowStatus::Partial,
            score: 0.0,
            depth_limited: false,
        };
        let flows = vec![flow.clone(), flow];
        let node = node_fixture(0);
        let result = gap_selected(
            &[&node],
            &[],
            &flows,
            1,
            ExportMode::VerifiedOnly,
            true,
            true,
            None,
        )
        .unwrap();
        assert_eq!(result.assertions.len(), 13);
        let json: serde_json::Value = serde_json::from_str(result.index.as_ref().unwrap()).unwrap();
        assert_eq!(json["supporting"].as_array().unwrap().len(), 12);
        assert_eq!(json["classes"].as_array().unwrap().len(), 1);
        let hop = &result.assertions[0];
        assert_eq!(hop.id, "flow:trigger:0:CALLS:gap:0000");
        assert_eq!(hop.subject_id, "source CALLS gap:0000");
        assert_eq!(hop.subject_kind, "FlowHop");
        assert_eq!(hop.summary, "CALLS: source | é → gap\nname");
        assert_eq!(hop.provenance, prov(ConfidenceTier::Gap));
        assert_eq!(hop.edge_label, None);
        assert_eq!(result.assertions[1].id, "flow:trigger:10:CALLS:gap:0000");
        let bytes = total(&result);
        assert!(
            gap_selected(
                &[&node],
                &[],
                &flows,
                1,
                ExportMode::VerifiedOnly,
                true,
                true,
                Some(bytes)
            )
            .is_ok()
        );
        assert!(
            gap_selected(
                &[&node],
                &[],
                &flows,
                1,
                ExportMode::VerifiedOnly,
                true,
                true,
                Some(bytes - 1)
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_provenance_fallback_hashes_streamed_complete_id() {
        let node = Node {
            id: "gap:é".into(),
            label: "Gap".into(),
            props: serde_json::json!({"reason":"missing provenance"}),
        };
        let edge = Edge {
            src: "source".into(),
            label: "CALLS".into(),
            dst: "gap:é".into(),
            props: serde_json::json!({}),
        };
        let result = gap_selected(
            &[&node],
            &[&edge],
            &[],
            1,
            ExportMode::VerifiedOnly,
            true,
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            result.assertions[0].provenance.content_hash,
            core_prov::content_hash("edge:source CALLS gap:é".as_bytes())
        );
        assert_eq!(
            result.assertions[1].provenance.content_hash,
            core_prov::content_hash("node:gap:é".as_bytes())
        );
        for a in result.assertions {
            assert_eq!(a.provenance.extractor_id, "spec.invalid-provenance");
            assert_eq!(a.provenance.confidence_tier, ConfidenceTier::Gap);
        }
        assert!(
            matrix_selected(&[&edge], ExportMode::VerifiedOnly, true, true, None)
                .unwrap()
                .assertions
                .is_empty()
        );
    }
    #[test]
    fn disabled_registers_accept_zero_budget() {
        assert!(matrix_selected(&[], ExportMode::VerifiedOnly, false, false, Some(0)).is_ok());
        assert!(
            gap_selected(
                &[],
                &[],
                &[],
                0,
                ExportMode::VerifiedOnly,
                false,
                false,
                Some(0)
            )
            .is_ok()
        );
    }
}
