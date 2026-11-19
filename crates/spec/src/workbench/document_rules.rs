//! D1 §1.5 arranged per capability (#1443): the rejection exits a
//! capability's handlers reach.
//!
//! For each derived Capability, its cited handler Symbols are followed along
//! admitted `CALLS` edges to Symbols, at most [`CALL_DEPTH`] hops. Every
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

use crate::workbench::capabilities::{Stories, Support};
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

/// One rejection exit, ready to print.
struct Rejection<'a> {
    rule: &'a Node,
    owner: &'a str,
    when: String,
    outcome: String,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `new FooError(...)` or `new FooException(...)`.
fn constructs_error(value: &SourceExpression) -> bool {
    value
        .display
        .as_str()
        .strip_prefix("new ")
        .is_some_and(|rest| {
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

fn table(
    rows: &[Rejection<'_>],
    assertions: &mut Assertions,
    asserted: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    out.push_str("| Where | When | Outcome | Citation |\n|---|---|---|---|\n")?;
    for row in rows {
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
    }
    Ok(rows.len())
}

/// The rejection exits of the governed callables `owners`, in that order.
fn rows_of<'a>(
    owners: impl Iterator<Item = &'a str>,
    governs: &BTreeMap<&'a str, Vec<&'a Node>>,
    by_id: &BTreeMap<&'a str, &'a Node>,
) -> Vec<Rejection<'a>> {
    let mut rows = Vec::new();
    for owner in owners {
        let name = by_id
            .get(owner)
            .and_then(|n| n.props["name"].as_str())
            .unwrap_or(owner);
        for rule in governs.get(owner).into_iter().flatten() {
            rows.extend(rejection(rule, name));
        }
    }
    rows
}

/// The rejection exits each Capability reaches, then the shared ones.
/// Returns the number of rows printed; prints nothing when no capability
/// reaches a rejection.
pub(super) fn rules_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
    let mut calls: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut governs: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    for edge in edges {
        let (Some(src), Some(dst)) = (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
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
    if governs.is_empty() {
        return Ok(0);
    }
    let capabilities: Vec<_> = knowledge
        .included
        .iter()
        .filter(|s| s.node.label == "Capability")
        .collect();
    // The governed callables each capability reaches, nearest first.
    let mut reached: Vec<Vec<&str>> = Vec::with_capacity(capabilities.len());
    let mut reach_count: BTreeMap<&str, usize> = BTreeMap::new();
    for story in &capabilities {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut order: Vec<&str> = Vec::new();
        let mut front: Vec<&str> = story
            .support
            .iter()
            .filter_map(|(_, fact)| match fact {
                Support::Node(n) if n.label == "Symbol" => Some(n.id.as_str()),
                _ => None,
            })
            .filter(|id| seen.insert(id))
            .collect();
        order.extend(&front);
        for _ in 0..CALL_DEPTH {
            let mut next = Vec::new();
            for id in &front {
                for callee in calls.get(id).into_iter().flatten() {
                    if seen.insert(callee) {
                        next.push(*callee);
                    }
                }
            }
            order.extend(&next);
            front = next;
        }
        order.retain(|id| governs.contains_key(id));
        for id in &order {
            *reach_count.entry(id).or_default() += 1;
        }
        reached.push(order);
    }
    let shared: Vec<&str> = reach_count
        .iter()
        .filter(|(_, n)| **n > SHARED_AFTER)
        .map(|(id, _)| *id)
        .collect();
    let per: Vec<Vec<Rejection<'_>>> = reached
        .iter()
        .map(|order| {
            rows_of(
                order
                    .iter()
                    .copied()
                    .filter(|id| reach_count[id] <= SHARED_AFTER),
                &governs,
                &by_id,
            )
        })
        .collect();
    let shared_rows = rows_of(shared.iter().copied(), &governs, &by_id);
    let total: usize = per.iter().map(Vec::len).sum::<usize>() + shared_rows.len();
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
    for (story, rows) in capabilities.iter().zip(&per) {
        if rows.is_empty() {
            continue;
        }
        writeln!(
            out,
            "### {}\n",
            Literal(crate::workbench::render::NodeName(story.node))
        )?;
        printed += table(rows, assertions, asserted, out)?;
        out.push_str("\n")?;
    }
    if !shared_rows.is_empty() {
        out.push_str("### Shared rejections\n\n")?;
        printed += table(&shared_rows, assertions, asserted, out)?;
        out.push_str("\n")?;
    }
    Ok(printed)
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
    return new OrderLimitError({ max: 10 });
  }
  return qty;
}
";

    fn render(capabilities: usize) -> (String, usize, usize) {
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
        let cite = BaseRef::Node(handler.id.clone());
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
                    support: vec![(&cite, Support::Node(handler))],
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
        let (text, printed, cited) = render(1);
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
    }

    // AC-0849: a callable more than three capabilities reach is listed once.
    #[test]
    fn rejections_reached_by_many_capabilities_are_listed_once_as_shared() {
        let (text, printed, cited) = render(SHARED_AFTER + 1);
        assert_eq!((printed, cited), (2, 2), "{text}");
        assert!(!text.contains("### Orders"), "{text}");
        assert!(text.contains("### Shared rejections"), "{text}");
        let (_, printed, _) = render(0);
        assert_eq!(printed, 0);
    }
}
