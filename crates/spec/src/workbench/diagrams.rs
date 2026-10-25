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
    let configs = configuring_files(nodes, edges);
    let aliases: BTreeMap<_, _> = resources
        .keys()
        .copied()
        .chain(backed.iter().copied())
        .chain(configs.keys().copied())
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
    // Env files declaring a resource's key: parallelograms, named by path.
    // A legal filename may hold a quote or line break; neither may end the
    // label or start a diagram line.
    for (id, path) in &configs {
        writeln!(
            out,
            "    {}[/\"{}\"/]",
            aliases[id],
            Replace(path, MERMAID_LABEL)
        )?;
    }
    let mut lines = Vec::new();
    for edge in edges {
        let Some((_, arrow)) = crate::TOPOLOGY_EDGES.iter().find(|(l, _)| *l == edge.label) else {
            continue;
        };
        if !(resources.contains_key(edge.src.as_str())
            && (resources.contains_key(edge.dst.as_str())
                || (edge.label == "BACKS" && backed.contains(edge.dst.as_str()))
                || (edge.label == "CONFIGURED_IN" && configs.contains_key(edge.dst.as_str()))))
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

/// Mermaid entity codes for the characters that would break a quoted label.
const MERMAID_LABEL: &[(char, &str)] = &[('"', "#quot;"), ('\n', " "), ('\r', " ")];

/// `File` nodes a `Resource` is `CONFIGURED_IN` (#734), with their paths.
pub(crate) fn configuring_files<'a>(
    nodes: &'a [Node],
    edges: &[Edge],
) -> BTreeMap<&'a str, &'a str> {
    let resources: BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.label == "Resource")
        .map(|n| n.id.as_str())
        .collect();
    let targets: BTreeSet<_> = edges
        .iter()
        .filter(|e| e.label == "CONFIGURED_IN" && resources.contains(e.src.as_str()))
        .map(|e| e.dst.as_str())
        .collect();
    nodes
        .iter()
        .filter(|n| n.label == "File" && targets.contains(n.id.as_str()))
        .map(|n| {
            let path = n.props["path"]
                .as_str()
                .unwrap_or_else(|| n.id.split_once('@').map_or(n.id.as_str(), |(_, path)| path));
            (n.id.as_str(), path)
        })
        .collect()
}

/// Tests shown per participant before the rest are counted.
const SHOWN_TESTS: usize = 5;

/// Static test references per flow participant (#965): a test requests an
/// Endpoint (`REQUESTS`) or its own function calls a function directly
/// (`CALLS` from the test's `IMPLEMENTED_BY` symbol). Never transitive.
/// Each reference keeps every edge that proves it: the `REQUESTS`, or the
/// `IMPLEMENTED_BY` association and the `CALLS`.
pub(crate) struct TestReferences<'a> {
    pub by_participant: BTreeMap<&'a str, Vec<(&'a str, Vec<&'a Edge>)>>,
}

impl<'a> TestReferences<'a> {
    /// `None` when the repository has no recovered tests, so its dossiers
    /// render exactly as before.
    pub(crate) fn new(
        nodes: &[&'a Node],
        edges: &[&'a Edge],
        nongap: impl Fn(&Edge) -> bool,
    ) -> Option<Self> {
        let tests: BTreeSet<&str> = nodes
            .iter()
            .filter(|n| n.label == "TestCase")
            .map(|n| n.id.as_str())
            .collect();
        if tests.is_empty() {
            return None;
        }
        // Only a proven association attributes a function's calls to a test.
        let implemented: BTreeMap<&str, &Edge> = edges
            .iter()
            .copied()
            .filter(|e| e.label == "IMPLEMENTED_BY" && tests.contains(e.src.as_str()) && nongap(e))
            .map(|e| (e.dst.as_str(), e))
            .collect();
        let mut by_participant: BTreeMap<&str, Vec<(&str, Vec<&Edge>)>> = BTreeMap::new();
        for edge in edges.iter().copied().filter(|e| nongap(e)) {
            let (test, proof) = match edge.label.as_str() {
                "REQUESTS" if tests.contains(edge.src.as_str()) => (edge.src.as_str(), vec![edge]),
                "CALLS" => match implemented.get(edge.src.as_str()) {
                    Some(association) => (association.src.as_str(), vec![*association, edge]),
                    None => continue,
                },
                _ => continue,
            };
            let references = by_participant.entry(edge.dst.as_str()).or_default();
            if !references.iter().any(|(seen, _)| *seen == test) {
                references.push((test, proof));
            }
        }
        Some(Self { by_participant })
    }
}

/// A test named by its path and qualified name.
fn test_name(id: &str) -> &str {
    id.split_once('@').map_or(id, |(_, rest)| rest)
}

pub(crate) fn flows(out: &mut impl fmt::Write, flows: &[Flow]) -> fmt::Result {
    flows_with_tests(out, flows, None)
}

pub(crate) fn flows_with_tests(
    out: &mut impl fmt::Write,
    flows: &[Flow],
    tests: Option<&TestReferences<'_>>,
) -> fmt::Result {
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
            // The trigger alone is still a participant a test may reference.
            if let Some(tests) = tests {
                test_table(out, &[(&flow.trigger, &flow.trigger_name)], tests)?;
            }
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
        if let Some(tests) = tests {
            test_table(out, &participants, tests)?;
        }
    }
    Ok(())
}

/// The static test reference table for one flow's participants (#965).
fn test_table(
    out: &mut impl fmt::Write,
    participants: &[(&str, &str)],
    tests: &TestReferences<'_>,
) -> fmt::Result {
    let referenced = participants
        .iter()
        .filter(|(id, _)| tests.by_participant.contains_key(id))
        .count();
    writeln!(
        out,
        "\n**Test references (static).** {referenced} of {} participants are referenced directly by a recovered test: an endpoint it requests or a function it calls. A reference is not an execution, and reach through further calls is not counted.\n\n| Participant | Tests |\n|---|---|",
        participants.len()
    )?;
    for (id, name) in participants {
        write!(out, "| {} | ", Safe(Replace(name, &[('\n', " ")])))?;
        match tests.by_participant.get(id) {
            None => out.write_str("— no recovered test reference")?,
            Some(references) => {
                for (i, (test, _)) in references.iter().take(SHOWN_TESTS).enumerate() {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{}", Safe(test_name(test)))?;
                }
                if references.len() > SHOWN_TESTS {
                    write!(out, ", and {} more", references.len() - SHOWN_TESTS)?;
                }
            }
        }
        out.write_str(" |\n")?;
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
        set_slots::<&str>(rows)?,
        set_slots::<&str>(rows)?,
        map_slots::<&str, &str>(rows)?, // configuring_files resources/targets/configs
        bytes,
        rows.checked_mul(crate::mermaid_id("").len())?, // alias payload + generated hash suffix
        bytes, // one current sanitized source ID during format!
        slots::<Line<'_>>(rows)?,
        slots::<Line<'_>>(rows)?,                     // lines + stable sort
        slots::<(&str, &str)>(hops.checked_mul(2)?)?, // two participants per hop
        set_slots::<&str>(rows)?,                     // test references: tests
        map_slots::<&str, &Edge>(rows)?,              // implemented symbols
        map_slots::<&str, Vec<(&str, Vec<&Edge>)>>(rows)?, // by participant
        slots::<(&str, Vec<&Edge>)>(rows)?,
        slots::<&Edge>(rows.checked_mul(2)?)?, // association + call proofs
    ])
}
