//! Shared streaming diagram writers used by ordinary and bounded artifacts.
use super::render::{EvidenceText, MethodLabel, Replace, Safe};
use core_graph::{Edge, Node};
use flowtracer::Flow;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

struct Line<'a> {
    src: &'a str,
    arrow: &'a str,
    label: &'a str,
    dst: &'a str,
}
impl Line<'_> {
    fn chars(&self) -> impl Iterator<Item = char> {
        [
            "    ", self.src, " ", self.arrow, "|", self.label, "| ", self.dst,
        ]
        .into_iter()
        .flat_map(str::chars)
    }
}

pub(crate) fn topology(out: &mut impl fmt::Write, nodes: &[Node], edges: &[Edge]) -> fmt::Result {
    let resources: BTreeMap<_, _> = nodes
        .iter()
        .filter(|n| n.label == "Resource")
        .map(|n| (n.id.as_str(), n))
        .collect();
    let channels: BTreeMap<_, _> = nodes
        .iter()
        .filter(|n| n.label == "Channel")
        .map(|n| (n.id.as_str(), n))
        .collect();
    let backed: BTreeSet<_> = edges
        .iter()
        .filter(|e| {
            e.label == "BACKS"
                && resources.contains_key(e.src.as_str())
                && channels.contains_key(e.dst.as_str())
        })
        .map(|e| e.dst.as_str())
        .collect();
    let aliases: BTreeMap<_, _> = resources
        .keys()
        .copied()
        .chain(backed.iter().copied())
        .map(|id| (id, crate::mermaid_id(id)))
        .collect();
    out.write_str("flowchart LR\n")?;
    for (id, node) in &resources {
        let display = node
            .props
            .get("logical_id")
            .and_then(|v| v.as_str())
            .unwrap_or(id.strip_prefix("res:").unwrap_or(id));
        let placeholder = node.props.get("placeholder").is_some();
        let (open, close) = if placeholder {
            ("([", "])")
        } else {
            ("[", "]")
        };
        writeln!(
            out,
            "    {}{}\"{}{}\"{}",
            aliases[id],
            open,
            display,
            if placeholder { " ?" } else { "" },
            close
        )?;
    }
    for id in &backed {
        writeln!(
            out,
            "    {}[(\"{}\")]",
            aliases[id],
            id.strip_prefix("chan:").unwrap_or(id)
        )?;
    }
    let mut lines = Vec::new();
    for edge in edges {
        let Some((_, arrow)) = crate::TOPOLOGY_EDGES.iter().find(|(l, _)| *l == edge.label) else {
            continue;
        };
        if !(resources.contains_key(edge.src.as_str())
            && (resources.contains_key(edge.dst.as_str())
                || (edge.label == "BACKS" && backed.contains(edge.dst.as_str()))))
        {
            continue;
        }
        lines.push(Line {
            src: &aliases[edge.src.as_str()],
            arrow,
            label: &edge.label,
            dst: &aliases[edge.dst.as_str()],
        });
    }
    lines.sort_by(|a, b| a.chars().cmp(b.chars()));
    lines.dedup_by(|a, b| a.chars().eq(b.chars()));
    for line in lines {
        writeln!(
            out,
            "    {} {}|{}| {}",
            line.src, line.arrow, line.label, line.dst
        )?;
    }
    Ok(())
}

pub(crate) fn flows(out: &mut impl fmt::Write, flows: &[Flow]) -> fmt::Result {
    out.write_str("# Flow dossier\n")?;
    for flow in flows {
        writeln!(
            out,
            "\n## {} — {} (score {:.2})\n",
            flow.trigger_name,
            flow.status.as_str(),
            flow.score
        )?;
        writeln!(out, "Trigger: {} `{}`", flow.trigger_kind, flow.trigger)?;
        if flow.depth_limited {
            out.write_str("\n**Truncated at the traversal depth bound** — downstream hops exist that are not shown.\n")?;
        }
        if flow.hops.is_empty() {
            out.write_str("\nNo hops resolved from this trigger.\n")?;
            continue;
        }
        let mut participants: Vec<(&str, &str)> = Vec::new();
        for hop in &flow.hops {
            for (id, name) in [(&hop.src, &hop.src_name), (&hop.dst, &hop.dst_name)] {
                if !participants.iter().any(|(pid, _)| *pid == id) {
                    participants.push((id, name));
                }
            }
        }
        out.write_str("\n```mermaid\nsequenceDiagram\n")?;
        for (i, (_, name)) in participants.iter().enumerate() {
            writeln!(
                out,
                "    participant p{i} as {}",
                Replace(name, &[('\n', " ")])
            )?;
        }
        for hop in &flow.hops {
            let a = participants
                .iter()
                .position(|(id, _)| *id == hop.src)
                .expect("participant");
            let b = participants
                .iter()
                .position(|(id, _)| *id == hop.dst)
                .expect("participant");
            writeln!(
                out,
                "    p{a}{}p{b}: {} [{}]",
                if hop.confidence == "Gap" {
                    "--x"
                } else {
                    "->>"
                },
                hop.label,
                hop.confidence
            )?;
        }
        out.write_str("```\n")?;
        out.write_str("\n| # | Hop | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|-----|------|------------|--------|----------|-----------|--------------|\n")?;
        for (i, hop) in flow.hops.iter().enumerate() {
            writeln!(
                out,
                "| {} | {} `{}` → `{}` | {} | {} | {} | {} | `{}` | `{}` |",
                i + 1,
                hop.label,
                hop.src,
                hop.dst,
                hop.tier,
                hop.confidence,
                Safe(MethodLabel(&hop.provenance.method)),
                EvidenceText(&hop.provenance),
                hop.provenance.extractor_id,
                hop.provenance.content_hash
            )?;
        }
    }
    Ok(())
}

pub(super) fn logical_index_bytes(bytes: usize, rows: usize, hops: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, &Node>(rows)?, // resources/channels
        set_slots::<&str>(rows)?,
        map_slots::<&str, String>(rows)?, // backed/aliases
        bytes,
        rows.checked_mul(crate::mermaid_id("").len())?, // alias payload + generated hash suffix
        bytes, // one current sanitized source ID during format!
        slots::<Line<'_>>(rows)?,
        slots::<Line<'_>>(rows)?,                     // lines + stable sort
        slots::<(&str, &str)>(hops.checked_mul(2)?)?, // two participants per hop
    ])
}
