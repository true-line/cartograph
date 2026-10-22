//! Shared native artifact renderers with per-occurrence output admission.
use super::render::{Assertions, Budget, Join, Json, NodeName, RenderLimit, Replace, Safe, Text};
use super::*;
use std::fmt::{self, Write};

pub(super) struct Output {
    pub content: String,
    pub assertions: Vec<SpecAssertion>,
}
fn finish(mut content: Text, assertions: Assertions) -> Result<Output, RenderLimit> {
    render::inline(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
    })
}
fn nongap(node: &&Node) -> bool {
    provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap
}
fn edge_nongap(edge: &&Edge) -> bool {
    provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
}
fn strings<'a>(node: &'a Node, key: &str) -> Vec<&'a str> {
    node.props[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

/// A relation field's metadata as text: a string as it is, a boolean (the
/// owning side) as its JSON spelling, an absent value as nothing.
fn relation_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn stories(nodes: &[&Node], max: Option<usize>) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    let capabilities: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Capability")
        .filter(nongap)
        .collect();
    for node in &capabilities {
        assertions.node(node, None)?;
    }
    let mut content = Text::new(&budget, "# Recovered user stories\n\n")?;
    if capabilities.is_empty() {
        content.push_str("No Capability facts have been recovered yet.\n")?;
    }
    for (index, node) in capabilities.iter().enumerate() {
        writeln!(
            content,
            "## US-R-{:04} — {}\n\n- Recovered capability assertion: `{}`\n",
            index + 1,
            NodeName(node),
            node.id
        )?;
    }
    finish(content, assertions)
}
pub(super) fn flow(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let origins = render::Origins::new(nodes, edges);
    let budget = Budget::new(max)?;
    let mut assertions = Assertions::new(&budget);
    for flow in flows {
        for (index, hop) in flow.hops.iter().enumerate() {
            assertions.hop(flow, hop, index, &origins)?;
        }
    }
    let mut content = Text::new(&budget, "")?;
    diagrams::flows(&mut content, flows)?;
    finish(content, assertions)
}
pub(super) fn topology(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut selected_edges: Vec<Edge> = edges
        .iter()
        .copied()
        .filter(|e| TOPOLOGY_EDGE_LABELS.contains(&e.label.as_str()))
        .filter(edge_nongap)
        .cloned()
        .collect();
    let backed: BTreeSet<_> = selected_edges
        .iter()
        .filter(|e| e.label == "BACKS")
        .map(|e| e.dst.as_str())
        .collect();
    let selected_nodes: Vec<Node> = nodes
        .iter()
        .copied()
        .filter(|n| {
            n.label == "Resource" || (n.label == "Channel" && backed.contains(n.id.as_str()))
        })
        .filter(nongap)
        .cloned()
        .collect();
    let ids: BTreeSet<_> = selected_nodes.iter().map(|n| n.id.as_str()).collect();
    selected_edges.retain(|e| ids.contains(e.src.as_str()) && ids.contains(e.dst.as_str()));
    let mut assertions = Assertions::new(&budget);
    for n in &selected_nodes {
        assertions.node(n, None)?;
    }
    for e in &selected_edges {
        assertions.edge(e)?;
    }
    let mut content = Text::new(&budget, "# Resource topology\n\n```mermaid\n")?;
    diagrams::topology(&mut content, &selected_nodes, &selected_edges)?;
    content.push_str("```\n")?;
    finish(content, assertions)
}
struct ModelName<'a>(&'a Node);
impl fmt::Display for ModelName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.label != "DataEntity" {
            write!(f, "{}: ", self.0.label)?;
        }
        write!(f, "{}", NodeName(self.0))
    }
}
pub(super) fn data(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let by_id: BTreeMap<_, _> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
    let entities: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "DataEntity")
        .filter(nongap)
        .collect();
    let entity_ids: BTreeSet<_> = entities.iter().map(|n| n.id.as_str()).collect();
    let mappings: Vec<_> = edges
        .iter()
        .copied()
        .filter(|e| DATA_EDGE_LABELS.contains(&e.label.as_str()))
        .filter(edge_nongap)
        .filter(|e| entity_ids.contains(e.src.as_str()) || entity_ids.contains(e.dst.as_str()))
        .filter(|e| by_id.contains_key(e.src.as_str()) && by_id.contains_key(e.dst.as_str()))
        .collect();
    let ids: BTreeSet<_> = entity_ids
        .iter()
        .copied()
        .chain(
            mappings
                .iter()
                .flat_map(|e| [e.src.as_str(), e.dst.as_str()]),
        )
        .collect();
    let model_nodes: Vec<_> = ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
    let aliases: BTreeMap<_, _> = model_nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &model_nodes {
        assertions.node(n, None)?;
    }
    for e in &mappings {
        assertions.edge(e)?;
    }
    let mut content = Text::new(
        &budget,
        "# Recovered data model\n\n```mermaid\nflowchart LR\n",
    )?;
    for n in &model_nodes {
        writeln!(
            content,
            "    d{}[\"{}\"]",
            aliases[n.id.as_str()],
            Replace(ModelName(n), &[('\r', " "), ('\n', " "), ('"', "'")])
        )?;
    }
    for e in &mappings {
        writeln!(
            content,
            "    d{} -->|{}| d{}",
            aliases[e.src.as_str()],
            e.label,
            aliases[e.dst.as_str()]
        )?;
    }
    content.push_str("```\n")?;
    if entities.is_empty() {
        content.push_str("\nNo DataEntity facts have been recovered yet.\n")?;
    }
    if !mappings.is_empty() {
        content.push_str(
            "\n## Access and mapping relations\n\n| Source | Relation | Target |\n|---|---|---|\n",
        )?;
        for e in &mappings {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                Safe(&e.src),
                e.label,
                Safe(&e.dst)
            )?;
        }
    }
    // One relation edge can carry several fields — several JPA relations to
    // the same target are one fact — so every field it declares is listed.
    let relation_fields: Vec<_> = mappings
        .iter()
        .filter_map(|e| {
            let relations = e.props.get("relations")?.as_array()?;
            Some((e, relations))
        })
        .collect();
    if !relation_fields.is_empty() {
        content.push_str(
            "\n## Relation fields\n\n| Source | Field | Relation | Cardinality | Owning | Target |\n|---|---|---|---|---|---|\n",
        )?;
        for (e, relations) in relation_fields {
            for relation in relations {
                writeln!(
                    content,
                    "| `{}` | `{}` | {} | {} | {} | `{}` |",
                    Safe(&e.src),
                    Safe(relation_value(&relation["field"])),
                    Safe(relation_value(&relation["relation"])),
                    Safe(relation_value(&relation["cardinality"])),
                    Safe(relation_value(&relation["owning"])),
                    Safe(&e.dst)
                )?;
            }
        }
    }
    finish(content, assertions)
}
pub(super) fn adrs(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let adrs: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "ADR")
        .filter(nongap)
        .collect();
    let ids: BTreeSet<_> = adrs.iter().map(|n| n.id.as_str()).collect();
    let decisions: Vec<_> = edges
        .iter()
        .copied()
        .filter(|e| e.label == "DECIDES")
        .filter(edge_nongap)
        .filter(|e| ids.contains(e.src.as_str()))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &adrs {
        assertions.node(n, None)?;
    }
    for e in &decisions {
        assertions.edge(e)?;
    }
    let mut content = Text::new(&budget, "# Found and recovered ADRs\n\n")?;
    for n in &adrs {
        writeln!(content, "## {}\n", NodeName(n))?;
        if let Some(v) = n.props["origin"].as_str() {
            writeln!(content, "**Origin:** {v}\n")?;
        }
        if let Some(v) = n.props["status"].as_str() {
            writeln!(content, "**Status:** {v}\n")?;
        }
        if let Some(v) = text_prop(n, &["body", "content", "decision"]) {
            writeln!(content, "{v}\n")?;
        }
    }
    if adrs.is_empty() {
        content.push_str("No found or recovered ADR facts have been recovered yet.\n")?;
    }
    if !decisions.is_empty() {
        content.push_str("\n## Decision links\n\n| ADR | Relation | Subject |\n|---|---|---|\n")?;
        for e in &decisions {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                Safe(&e.src),
                e.label,
                Safe(&e.dst)
            )?;
        }
    }
    finish(content, assertions)
}
pub(super) fn drift(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let findings: Vec<_> = nodes.iter().copied().filter(|n| is_drift_node(n)).collect();
    let mut assertions = Assertions::new(&budget);
    for n in &findings {
        assertions.node(n, None)?;
    }
    for e in edges.iter().copied().filter(|e| is_drift_edge(e)) {
        assertions.edge(e)?;
    }
    let adr_findings: Vec<_> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() != Some("contract-drift"))
        .collect();
    let mut contract_findings: Vec<_> = findings
        .iter()
        .copied()
        .filter(|n| n.props["kind"].as_str() == Some("contract-drift"))
        .collect();
    let mut contract_gaps: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| is_gap_node(n) && n.props["kind"].as_str() == Some("contract-doc-gap"))
        .collect();
    let has_declared_docs = !contract_findings.is_empty()
        || !contract_gaps.is_empty()
        || nodes.iter().any(|n| n.label == "DocumentedRoute");

    let mut content = Text::new(
        &budget,
        "# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n",
    )?;
    for n in &adr_findings {
        let triggers = strings(n, "flow_triggers");
        let p = provenance(&n.props, &n.id);
        write!(
            content,
            "| {} | `{}` | `{}` | ",
            Safe(NodeName(n)),
            Safe(n.props["adr_id"].as_str().unwrap_or("—")),
            Safe(n.props["offending_edge"].as_str().unwrap_or("—"))
        )?;
        if triggers.iter().all(|s| s.is_empty()) && triggers.len() < 2 {
            write!(content, "—")?;
        } else {
            write!(content, "{}", Safe(Join(&triggers, ", ")))?;
        }
        writeln!(content, " | {:?} |", p.confidence_tier)?;
    }
    if adr_findings.is_empty() {
        content.push_str("| — | No ADR/code conflicts recovered |\n")?;
    }
    if has_declared_docs {
        let by_id: BTreeMap<&str, &Node> =
            nodes.iter().copied().map(|n| (n.id.as_str(), n)).collect();
        content.push_str(
            "\n## API contract drift\n\n| Finding | Endpoint | Documented | Served | Confidence |\n|---|---|---|---|---|\n",
        )?;
        contract_findings.sort_by(|a, b| {
            let ak = a.props["drift_kind"].as_str().unwrap_or("");
            let bk = b.props["drift_kind"].as_str().unwrap_or("");
            let ae = a.props["key"]
                .as_str()
                .or_else(|| a.props["canonical"].as_str())
                .unwrap_or(&a.id);
            let be = b.props["key"]
                .as_str()
                .or_else(|| b.props["canonical"].as_str())
                .unwrap_or(&b.id);
            (ak, ae, a.id.as_str()).cmp(&(bk, be, b.id.as_str()))
        });
        for n in &contract_findings {
            let drift_kind = n.props["drift_kind"].as_str().unwrap_or("contract-drift");
            let endpoint = n.props["key"]
                .as_str()
                .or_else(|| n.props["canonical"].as_str())
                .unwrap_or(&n.id);
            let documented = contract_side_cell(n, "documented", "DOCUMENTED_AS", edges, &by_id);
            let served = contract_side_cell(n, "served", "SERVED_AS", edges, &by_id);
            let p = provenance(&n.props, &n.id);
            writeln!(
                content,
                "| {} | `{}` | {} | {} | {:?} |",
                Safe(drift_kind),
                Safe(endpoint),
                Safe(&documented),
                Safe(&served),
                p.confidence_tier,
            )?;
        }
        contract_gaps.sort_by(|a, b| {
            let ad = a.props["doc_path"].as_str().unwrap_or("");
            let bd = b.props["doc_path"].as_str().unwrap_or("");
            let ar = a.props["reason"].as_str().unwrap_or("");
            let br = b.props["reason"].as_str().unwrap_or("");
            (ad, ar, a.id.as_str()).cmp(&(bd, br, b.id.as_str()))
        });
        for g in &contract_gaps {
            let doc_path = g.props["doc_path"].as_str().unwrap_or("—");
            let reason = g.props["reason"].as_str().unwrap_or(&g.id);
            writeln!(
                content,
                "| gap | `{}` | {} | — | Gap |",
                Safe(doc_path),
                Safe(reason),
            )?;
        }
        if contract_findings.is_empty() && contract_gaps.is_empty() {
            content.push_str("| — | No API contract drift recovered | — | — | — |\n")?;
        }
    }
    finish(content, assertions)
}
fn contract_side_cell(
    drift: &Node,
    prop_name: &str,
    edge_label: &str,
    edges: &[&Edge],
    by_id: &BTreeMap<&str, &Node>,
) -> String {
    if let Some(s) = drift.props[prop_name].as_str().filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    let mut parts = Vec::new();
    for edge in edges
        .iter()
        .copied()
        .filter(|e| e.src == drift.id && e.label == edge_label)
    {
        if let Some(target) = by_id.get(edge.dst.as_str()) {
            parts.push(format!("{} ({})", NodeName(target), target.id));
        } else {
            parts.push(edge.dst.clone());
        }
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(", ")
    }
}
fn list(out: &mut Text, values: &[&str]) -> fmt::Result {
    if values.iter().all(|s| s.is_empty()) && values.len() < 2 {
        out.push_str("—")
    } else {
        write!(out, "{}", Safe(Join(values, ", ")))
    }
}
pub(super) fn security(nodes: &[&Node], max: Option<usize>) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let findings: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Finding" && n.props["kind"].as_str() == Some("security"))
        .collect();
    let mut assertions = Assertions::new(&budget);
    for n in &findings {
        assertions.node(n, None)?;
    }
    let mut content = Text::new(
        &budget,
        "# Security findings\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n",
    )?;
    for n in &findings {
        write!(
            content,
            "| {} | `{}` | `{}` | ",
            Safe(NodeName(n)),
            Safe(n.props["category"].as_str().unwrap_or("security")),
            Safe(n.props["subject_id"].as_str().unwrap_or("—"))
        )?;
        list(&mut content, &strings(n, "resource_scope"))?;
        content.push_str(" | ")?;
        list(&mut content, &strings(n, "actions"))?;
        writeln!(
            content,
            " | {} / {} | {:?} |",
            Safe(n.props["us_id"].as_str().unwrap_or("US-0015")),
            Safe(n.props["ac_id"].as_str().unwrap_or("—")),
            provenance(&n.props, &n.id).confidence_tier
        )?;
    }
    if findings.is_empty() {
        content.push_str("| — | — | — | — | — | No explicit security findings | — |\n")?;
    }
    finish(content, assertions)
}
struct Settings<'a>(&'a Node);
impl fmt::Display for Settings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.props["settings_behind_code"].as_bool() == Some(true) {
            return f.write_str("settings live in code — detected by presence only");
        }
        let Some(values) = self.0.props["settings"]
            .as_object()
            .filter(|v| !v.is_empty())
        else {
            return f.write_str("—");
        };
        for (i, (k, v)) in values.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{k}={}", Json(v))?;
        }
        Ok(())
    }
}
pub(super) fn toolchain(
    nodes: &[&Node],
    edges: &[&Edge],
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let tools: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|n| n.label == "Tool")
        .collect();
    let paths: Vec<Vec<&str>> = tools
        .iter()
        .map(|n| {
            let mut paths: Vec<_> = edges
                .iter()
                .filter(|e| e.label == "DEFINED_IN" && e.src == n.id)
                .map(|e| e.dst.split_once('@').map(|(_, p)| p).unwrap_or(&e.dst))
                .collect();
            paths.sort();
            paths.dedup();
            paths
        })
        .collect();
    let mut assertions = Assertions::new(&budget);
    for (n, p) in tools.iter().zip(&paths) {
        assertions.tool(n, p)?;
    }
    let mut content = Text::new(
        &budget,
        "# Toolchain\n\n| Tool | Category | Defined in | Settings | Confidence |\n|---|---|---|---|---|\n",
    )?;
    for (n, paths) in tools.iter().zip(&paths) {
        writeln!(
            content,
            "| {} | {} | {} | {} | {:?} |",
            Safe(n.props["display"].as_str().unwrap_or(&n.id)),
            Safe(n.props["category"].as_str().unwrap_or("—")),
            Safe(Join(paths, ", ")),
            Safe(Settings(n)),
            provenance(&n.props, &n.id).confidence_tier
        )?;
    }
    if tools.is_empty() {
        content.push_str("| — | — | — | No toolchain facts recovered | — |\n")?;
    }
    finish(content, assertions)
}

/// Explicit borrowed index roles; input properties are not cloned by these
/// vectors/maps. Topology's owned selected graph is charged by the caller.
pub(super) fn logical_index_bytes(rows: usize, values: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    let data = sum(&[
        map_slots::<&str, &Node>(rows)?, // by_id
        slots::<&Node>(rows)?,
        set_slots::<&str>(rows)?, // entities/entity_ids
        slots::<&Edge>(rows)?,
        set_slots::<&str>(rows)?, // mappings/ids
        slots::<&Node>(rows)?,
        map_slots::<&str, usize>(rows)?, // model_nodes/aliases
        slots::<(&&Edge, &Vec<serde_json::Value>)>(rows)?, // relation_fields
    ])?;
    let tools = sum(&[
        slots::<&Node>(rows)?,
        slots::<Vec<&str>>(rows)?,
        slots::<&str>(rows)?,
        slots::<&str>(rows)?,
    ])?; // paths + one sort scratch
    let simple = sum(&[
        slots::<&Node>(rows)?,
        slots::<&Edge>(rows)?,
        set_slots::<&str>(rows)?,
        slots::<&str>(values)?,
    ])?;
    Some(data.max(tools).max(simple))
}
