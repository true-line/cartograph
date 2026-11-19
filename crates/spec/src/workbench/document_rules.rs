//! D1 §1.5 arranged per capability (#1443): the rejection exits a
//! capability's handlers reach.
//!
//! For each derived Capability, the handler Symbols its cited `HANDLES`
//! edges reach are followed along admitted `CALLS` edges to Symbols, at
//! most [`CALL_DEPTH`] hops. Every
//! admitted rule observation that `GOVERNS` a reached callable and whose
//! local exit rejects (a `throw`, or a `return` of a constructed `…Error` or
//! `…Exception`) is listed under the capability with its owner, the stored
//! same-callable branch conditions and the exit. A callable that more than
//! [`SHARED_AFTER`] capabilities reach is shared plumbing: its rejections are
//! listed once, after the capabilities.
//!
//! Only the stored, sanitized displays the rule inventory renders are shown
//! (SPEC-02); nothing is read from source. The rows are observations: the
//! complete execution predicate and the consumer effect stay unestablished,
//! and `rule-evidence.md` keeps every observation with its dependencies.

use crate::workbench::capabilities::{Stories, Story, Support};
use crate::workbench::citation_id;
use crate::workbench::render::{Assertions, Literal, RenderLimit, Text};
use core_graph::rules::{BranchPolarity, GuardedExitEvidence, LocalExit, SourceExpression};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Call hops followed from a capability's handlers: resolver, service and
/// the service's own helpers.
pub(super) const CALL_DEPTH: usize = 3;

/// A callable reached from more capabilities than this is listed once.
pub(super) const SHARED_AFTER: usize = 3;

/// One rejection exit, formatted for the row being written.
struct Rejection<'a> {
    rule: &'a Node,
    owner: &'a str,
    when: String,
    outcome: String,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `new FooError(...)` or `new FooException(...)`, whatever whitespace
/// separates `new` from the constructor.
fn constructs_error(value: &SourceExpression) -> bool {
    let mut words = value.display.as_str().split_whitespace();
    words.next() == Some("new")
        && words.next().is_some_and(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            name.ends_with("Error") || name.ends_with("Exception")
        })
}

fn rejection<'a>(rule: &'a Node, owner: &'a str) -> Option<Rejection<'a>> {
    let parsed = GuardedExitEvidence::from_value(rule.props["rule"].clone()).ok()?;
    let outcome = match &parsed.effect {
        LocalExit::Throw { value } => format!("throws {}", one_line(value.display.as_str())),
        LocalExit::Return { value: Some(value) } if constructs_error(value) => {
            format!("returns {}", one_line(value.display.as_str()))
        }
        LocalExit::Return { .. } => return None,
    };
    let when = if parsed.conditions.is_empty() {
        "always".to_owned()
    } else {
        parsed
            .conditions
            .iter()
            .map(|c| {
                let shown = one_line(c.expression.display.as_str());
                match c.polarity {
                    BranchPolarity::TruthyBranch => shown,
                    BranchPolarity::FalsyBranch => format!("not {shown}"),
                }
            })
            .collect::<Vec<_>>()
            .join(" and ")
    };
    Some(Rejection {
        rule,
        owner,
        when,
        outcome,
    })
}

/// The admitted call and governance indexes, borrowed from the graph.
struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    calls: BTreeMap<&'a str, Vec<&'a str>>,
    governs: BTreeMap<&'a str, Vec<&'a Node>>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut calls: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut governs: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
        for edge in edges {
            let (Some(src), Some(dst)) =
                (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
            else {
                continue;
            };
            match edge.label.as_str() {
                "CALLS" if src.label == "Symbol" && dst.label == "Symbol" => {
                    calls.entry(&src.id).or_default().push(&dst.id);
                }
                "GOVERNS" if src.label == "BusinessRule" => {
                    governs.entry(&dst.id).or_default().push(src)
                }
                _ => {}
            }
        }
        Self {
            by_id,
            calls,
            governs,
        }
    }

    /// The governed callables a capability's handlers reach, nearest first.
    /// Handlers are the destinations of the `HANDLES` edges it cites.
    fn reached(&self, story: &Story<'a>) -> Vec<&'a str> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut order: Vec<&str> = Vec::new();
        let mut front: Vec<&str> = story
            .support
            .iter()
            .filter_map(|(_, fact)| match fact {
                Support::Edge(e) if e.label == "HANDLES" => Some(e.dst.as_str()),
                _ => None,
            })
            .filter(|id| self.by_id.get(id).is_some_and(|n| n.label == "Symbol"))
            .filter(|id| seen.insert(id))
            .collect();
        order.extend(&front);
        for _ in 0..CALL_DEPTH {
            let mut next = Vec::new();
            for id in &front {
                for callee in self.calls.get(id).into_iter().flatten() {
                    if seen.insert(callee) {
                        next.push(*callee);
                    }
                }
            }
            order.extend(&next);
            front = next;
        }
        order.retain(|id| self.governs.contains_key(id));
        order
    }

    /// Each rejection exit of `owners`, in order, one at a time.
    fn each(
        &self,
        owners: impl Iterator<Item = &'a str>,
        mut row: impl FnMut(Rejection<'a>) -> Result<(), RenderLimit>,
    ) -> Result<usize, RenderLimit> {
        let mut n = 0;
        for owner in owners {
            let name = self
                .by_id
                .get(owner)
                .and_then(|n| n.props["name"].as_str())
                .unwrap_or(owner);
            for rule in self.governs.get(owner).into_iter().flatten() {
                if let Some(found) = rejection(rule, name) {
                    n += 1;
                    row(found)?;
                }
            }
        }
        Ok(n)
    }
}

fn write_row(
    row: &Rejection<'_>,
    assertions: &mut Assertions,
    asserted: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<(), RenderLimit> {
    let id = citation_id(&FactKey::Node {
        id: row.rule.id.clone(),
    });
    writeln!(
        out,
        "| {} | {} | {} | {id} |",
        Literal(row.owner),
        Literal(&row.when),
        Literal(&row.outcome),
    )?;
    if asserted.insert(id) {
        assertions.node(row.rule, None)?;
    }
    Ok(())
}

const HEADER: &str = "| Where | When | Outcome | Citation |\n|---|---|---|---|\n";

/// The rejection exits each Capability reaches, then the shared ones.
/// Returns the number of rows printed; prints nothing when no capability
/// reaches a rejection. Rows are formatted and written one at a time; the
/// retained workspace is [`rules_per_capability_workspace`].
pub(super) fn rules_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let index = Index::new(nodes, edges);
    if index.governs.is_empty() {
        return Ok(0);
    }
    let capabilities = || {
        knowledge
            .included
            .iter()
            .filter(|s| s.node.label == "Capability")
    };
    let mut reach_count: BTreeMap<&str, usize> = BTreeMap::new();
    for story in capabilities() {
        for id in index.reached(story) {
            *reach_count.entry(id).or_default() += 1;
        }
    }
    let own = |id: &&str| reach_count.get(id).is_some_and(|n| *n <= SHARED_AFTER);
    let shared = || {
        reach_count
            .iter()
            .filter(|(_, n)| **n > SHARED_AFTER)
            .map(|(id, _)| *id)
    };
    // Counted first so the lead states the total before any row.
    let mut total = index.each(shared(), |_| Ok(()))?;
    for story in capabilities() {
        total += index.each(index.reached(story).into_iter().filter(own), |_| Ok(()))?;
    }
    if total == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "\nRejection exits per capability: {total}. Each is a stored `throw`, or a `return` of a \
         constructed error, in a callable the capability's handlers reach within {CALL_DEPTH} \
         call hops (Deterministic observations). The branch conditions are the ones around the \
         exit in its own callable; the complete execution predicate and what the caller does \
         with the outcome are not established. A callable more than {SHARED_AFTER} \
         capabilities reach is listed once under shared rejections.\n"
    )?;
    let mut printed = 0;
    for story in capabilities() {
        let reached: Vec<&str> = index.reached(story).into_iter().filter(own).collect();
        if index.each(reached.iter().copied(), |_| Ok(()))? == 0 {
            continue;
        }
        writeln!(
            out,
            "### {}\n",
            Literal(crate::workbench::render::NodeName(story.node))
        )?;
        out.push_str(HEADER)?;
        printed += index.each(reached.into_iter(), |row| {
            write_row(&row, assertions, asserted, out)
        })?;
        out.push_str("\n")?;
    }
    if index.each(shared(), |_| Ok(()))? > 0 {
        out.push_str("### Shared rejections\n\n")?;
        out.push_str(HEADER)?;
        printed += index.each(shared(), |row| write_row(&row, assertions, asserted, out))?;
        out.push_str("\n")?;
    }
    Ok(printed)
}

/// Logical workspace of [`rules_per_capability`] beyond the borrowed graph,
/// stories and output: the borrowed id, call and governance indexes, the
/// reach counts, one capability's traversal and one row's decode and text.
/// Charged at the graph's row, byte and value ceilings; no allocator or RSS
/// claim. `None` on overflow.
pub(crate) fn rules_per_capability_workspace(graph: flowtracer::TraceGraphBound) -> Option<usize> {
    use crate::workbench::accounting::{json, map_slots, set_slots, slots, sum};
    let rows = graph.rows;
    let index = sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, Vec<&str>>(rows)?,
        slots::<&str>(rows)?,
        map_slots::<&str, Vec<&Node>>(rows)?,
        slots::<&Node>(rows)?,
    ])?;
    let reach_count = map_slots::<&str, usize>(rows)?;
    // One capability's seen set and order/front/next (or retained reach).
    let traversal = sum(&[
        set_slots::<&str>(rows)?,
        slots::<&str>(rows)?.checked_mul(3)?,
    ])?;
    // One row: the cloned payload and its typed decode, the whitespace
    // split, and the condition and outcome text (each at most the payload
    // plus a four-byte `not ` or ` and ` per value), then its citation id.
    let text = graph.bytes.checked_add(graph.values.checked_mul(5)?)?;
    let row = sum(&[
        json(graph.bytes, graph.values)?.checked_mul(2)?,
        slots::<&str>(graph.bytes)?,
        slots::<String>(graph.values)?,
        text.checked_mul(2)?,
        graph.bytes,
    ])?;
    sum(&[index, reach_count, traversal, row])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::render::Budget;
    use core_graph::overlay::BaseRef;

    const SOURCE: &str = "export class NegativeQuantityError extends Error {}
export class OrderLimitError extends Error {}
export function addItem(qty: number) {
  return checkQuantity(qty);
}
function checkQuantity(qty: number) {
  if (qty < 0) {
    throw new NegativeQuantityError();
  }
  if (qty > 10) {
    return new  OrderLimitError({ max: 10 });
  }
  return qty;
}
";

    fn render(capabilities: usize, seeded: bool) -> (String, usize, usize) {
        let facts = adapters_lang_ts::extract_source(
            SOURCE.as_bytes(),
            "src/order.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let handler = nodes
            .iter()
            .copied()
            .find(|n| n.label == "Symbol" && n.props["name"] == "addItem")
            .unwrap();
        // A Capability cites its route and the HANDLES edge to the handler;
        // a cited Symbol that is not a handler seeds nothing.
        let handles = Edge {
            src: "ep:repo@POST:/items".into(),
            dst: handler.id.clone(),
            label: "HANDLES".into(),
            props: serde_json::json!({}),
        };
        let other = nodes
            .iter()
            .copied()
            .find(|n| n.label == "Symbol" && n.props["name"] == "checkQuantity")
            .unwrap();
        let cites = [
            BaseRef::Edge(
                handles.src.clone(),
                handles.dst.clone(),
                handles.label.clone(),
            ),
            BaseRef::Node(other.id.clone()),
        ];
        let caps: Vec<Node> = (0..capabilities)
            .map(|i| Node {
                id: format!("capability:test:{i}"),
                label: "Capability".into(),
                props: serde_json::json!({ "title": format!("Orders {i}") }),
            })
            .collect();
        let knowledge = Stories {
            included: caps
                .iter()
                .map(|node| crate::workbench::capabilities::Story {
                    node,
                    support: if seeded {
                        vec![(&cites[0], Support::Edge(&handles))]
                    } else {
                        vec![(&cites[1], Support::Node(other))]
                    },
                })
                .collect(),
            filtered: 0,
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        let printed = rules_per_capability(
            (&nodes, &edges),
            &knowledge,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        (out.finish(), printed, assertions.values.len())
    }

    // AC-0849: a capability lists the rejection exits its handlers reach,
    // each cited once; plain returns stay in the rule inventory alone.
    #[test]
    fn rejections_are_listed_under_the_capability_that_reaches_them() {
        let (text, printed, cited) = render(1, true);
        assert_eq!(printed, 2, "{text}");
        assert_eq!(cited, 2);
        assert!(text.contains("### Orders 0"), "{text}");
        assert!(
            text.contains("| checkQuantity | (qty &lt; 0) | throws new NegativeQuantityError() |"),
            "{text}"
        );
        assert!(
            text.contains(
                "| checkQuantity | (qty &gt; 10) | returns new OrderLimitError({ max: 10 }) |"
            ),
            "{text}"
        );
        assert!(!text.contains("returns qty"), "{text}");
        assert!(!text.contains("Shared rejections"), "{text}");
        // A cited Symbol that no cited HANDLES edge reaches seeds nothing.
        let (_, printed, _) = render(1, false);
        assert_eq!(printed, 0);
    }

    // AC-0849: the workspace is charged at the graph ceilings and refuses
    // overflow.
    #[test]
    fn rule_arrangement_workspace_is_checked() {
        let graph = |bytes, rows, values| flowtracer::TraceGraphBound {
            bytes,
            rows,
            values,
        };
        let small = rules_per_capability_workspace(graph(1 << 10, 16, 64)).unwrap();
        let large = rules_per_capability_workspace(graph(1 << 20, 1 << 10, 1 << 12)).unwrap();
        assert!(small > 0 && large > small);
        assert!(rules_per_capability_workspace(graph(usize::MAX, 1, 1)).is_none());
        assert!(rules_per_capability_workspace(graph(1, usize::MAX, 1)).is_none());
    }

    // AC-0849: a callable more than three capabilities reach is listed once.
    #[test]
    fn rejections_reached_by_many_capabilities_are_listed_once_as_shared() {
        let (text, printed, cited) = render(SHARED_AFTER + 1, true);
        assert_eq!((printed, cited), (2, 2), "{text}");
        assert!(!text.contains("### Orders"), "{text}");
        assert!(text.contains("### Shared rejections"), "{text}");
        let (_, printed, _) = render(0, true);
        assert_eq!(printed, 0);
    }
}
