//! D1 §1.5 arranged per capability (#1443): the rejection exits a
//! capability's handlers reach.
//!
//! For each derived Capability, the handler Symbols its cited `HANDLES`
//! edges reach are followed along admitted `CALLS` edges to Symbols, at
//! most [`CALL_DEPTH`] hops. Every
//! admitted rule observation that `GOVERNS` a reached callable and whose
//! local exit rejects (a `throw` or Python `raise`, a `return` of a
//! constructed `…Error` or `…Exception`, or a Python response tuple ending
//! in a 4xx or 5xx status) is listed under the capability with its owner,
//! the stored same-callable branch conditions and the exit. A callable that
//! more than [`SHARED_AFTER`] capabilities reach is shared plumbing: its
//! rejections are listed once, after the capabilities.
//!
//! A function a rejection's condition calls (`user.can_create_new_alias()`)
//! is followed by its name (#1998, AC-0896): when exactly one recovered
//! Symbol has that last name segment, its guarded exits and those of what it
//! calls within [`PREDICATE_DEPTH`] hops are listed under the row, marked
//! `↳` and as matched by name; otherwise a `↳` row names the predicate and
//! why it was not followed, so the condition is never left unexplained.
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

/// The byte index of the `)` closing the `(` that `text` starts with.
/// Delimiters inside a `'…'`, `"…"` or `` `…` `` literal do not count; a
/// backslash escapes the next character in one.
fn closing(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Exactly one construction `new FooError(...)` or `new FooException(...)`,
/// possibly parenthesized, whatever whitespace separates `new` from the
/// constructor: a `new_expression` or `parenthesized_expression` whose whole
/// stored display is that construction, so `new FooError() || fallback` is
/// not one.
fn constructs_error(value: &SourceExpression) -> bool {
    if !matches!(
        value.syntax_kind.as_str(),
        "new_expression" | "parenthesized_expression"
    ) {
        return false;
    }
    let mut text = value.display.as_str().trim();
    while text.starts_with('(') && closing(text) == Some(text.len() - 1) {
        text = text[1..text.len() - 1].trim();
    }
    let Some(rest) = text.strip_prefix("new") else {
        return false;
    };
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let rest = rest.trim_start();
    let name_len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(rest.len());
    let (name, args) = rest.split_at(name_len);
    let args = args.trim();
    (name.ends_with("Error") || name.ends_with("Exception"))
        && (args.is_empty() || (args.starts_with('(') && closing(args) == Some(args.len() - 1)))
}

/// A Python response tuple whose last element is an error status:
/// `jsonify(error="Forbidden"), 403` or `("gone", 410)`. Commas inside
/// brackets or quoted literals do not count.
fn returns_error_status(value: &SourceExpression) -> bool {
    let mut text = value.display.as_str().trim();
    match value.syntax_kind.as_str() {
        "expression_list" => {}
        "tuple" if text.starts_with('(') && closing(text) == Some(text.len() - 1) => {
            text = &text[1..text.len() - 1];
        }
        _ => return false,
    }
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut last = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => last = Some(i),
            _ => {}
        }
    }
    let Some(last) = last else {
        return false;
    };
    let mut tail = text[last + 1..].trim();
    if tail.is_empty() {
        // A trailing comma: the element before it.
        let head = text[..last].trim_end();
        tail = head.rsplit(',').next().unwrap_or(head).trim();
    }
    tail.parse::<u16>()
        .is_ok_and(|status| (400..600).contains(&status))
}

/// Whether `text` reads as one operand: nothing but brackets and quoted
/// literals separates its parts, or one pair of parentheses wraps it all.
fn atomic(text: &str) -> bool {
    if text.starts_with('(') && closing(text) == Some(text.len() - 1) {
        return true;
    }
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => return false,
            _ => {}
        }
    }
    true
}

/// One branch condition as it holds at the exit. A falsy branch negates
/// the whole condition, so a compound one is parenthesized (`not (a or b)`,
/// never `not a or b`), and `not` over a negation is dropped. When `alone`
/// is false the condition is joined to others with `and`, so a compound
/// truthy one is parenthesized too.
fn holds(shown: &str, polarity: BranchPolarity, alone: bool) -> String {
    match polarity {
        BranchPolarity::FalsyBranch => match shown.strip_prefix("not ") {
            Some(inner) if atomic(inner.trim_start()) => inner.trim_start().to_owned(),
            _ if atomic(shown) => format!("not {shown}"),
            _ => format!("not ({shown})"),
        },
        // `not` binds tighter than `and`: `not x and y` keeps its reading.
        BranchPolarity::TruthyBranch
            if alone
                || atomic(shown)
                || shown
                    .strip_prefix("not ")
                    .is_some_and(|inner| atomic(inner.trim_start())) =>
        {
            shown.to_owned()
        }
        BranchPolarity::TruthyBranch => format!("({shown})"),
    }
}

fn rejection<'a>(rule: &'a Node, owner: &'a str) -> Option<Rejection<'a>> {
    guarded(rule, owner, false)
}

/// A guarded exit as a row; with `any`, every exit, not only rejections.
fn guarded<'a>(rule: &'a Node, owner: &'a str, any: bool) -> Option<Rejection<'a>> {
    let parsed = GuardedExitEvidence::from_value(rule.props["rule"].clone()).ok()?;
    let outcome = match &parsed.effect {
        LocalExit::Throw { value } => format!("throws {}", one_line(value.display.as_str())),
        LocalExit::Return { value: Some(value) }
            if any || constructs_error(value) || returns_error_status(value) =>
        {
            format!("returns {}", one_line(value.display.as_str()))
        }
        LocalExit::Return { value: None } if any => "returns nothing".to_owned(),
        LocalExit::Return { .. } => return None,
    };
    let when = if parsed.conditions.is_empty() {
        "always".to_owned()
    } else {
        let alone = parsed.conditions.len() == 1;
        parsed
            .conditions
            .iter()
            .map(|c| holds(&one_line(c.expression.display.as_str()), c.polarity, alone))
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
    /// Symbols by the last segment of their name, for following a
    /// predicate a condition calls (#1998).
    by_name: BTreeMap<&'a str, Vec<&'a str>>,
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
        let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in nodes.iter().filter(|n| n.label == "Symbol") {
            if let Some(name) = node.props["name"].as_str() {
                let last = name.rsplit('.').next().unwrap_or(name);
                by_name.entry(last).or_default().push(&node.id);
            }
        }
        Self {
            by_id,
            calls,
            governs,
            by_name,
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

/// Calls a condition makes, by name: `user.can_create()` gives
/// `can_create`. Builtins and names shorter than three characters are
/// skipped; at most [`MAX_PREDICATES`].
fn called_names(when: &str) -> Vec<&str> {
    const SKIP: [&str; 12] = [
        "len",
        "isinstance",
        "getattr",
        "hasattr",
        "str",
        "int",
        "bool",
        "any",
        "all",
        "get",
        "jsonify",
        "not",
    ];
    let mut out: Vec<&str> = Vec::new();
    let bytes = when.as_bytes();
    for (i, _) in when.match_indices('(') {
        let start = when[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |p| p + 1);
        let name = &when[start..i];
        if name.len() < 3
            || name.as_bytes()[0].is_ascii_digit()
            || SKIP.contains(&name)
            || (start > 0 && bytes[start - 1] == b'"')
            || out.contains(&name)
        {
            continue;
        }
        out.push(name);
        if out.len() == MAX_PREDICATES {
            break;
        }
    }
    out
}

/// Predicates followed per rejection row.
pub(super) const MAX_PREDICATES: usize = 3;
/// Call hops followed past a predicate.
pub(super) const PREDICATE_DEPTH: usize = 2;
/// Rows printed for one followed predicate.
pub(super) const MAX_FOLLOWED_ROWS: usize = 12;

impl<'a> Index<'a> {
    /// The rows of each predicate `row`'s condition calls: the guarded
    /// exits of the one repository function of that name and of what it
    /// calls within [`PREDICATE_DEPTH`] hops, or a row saying why it is not
    /// followed. Each predicate is followed once per `followed` set.
    fn follow(
        &self,
        row: &Rejection<'_>,
        followed: &mut BTreeSet<String>,
        assertions: &mut Assertions,
        asserted: &mut BTreeSet<String>,
        out: &mut Text,
    ) -> Result<(), RenderLimit> {
        for name in called_names(&row.when) {
            if !followed.insert(name.to_owned()) {
                continue;
            }
            let targets = self
                .by_name
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let [target] = targets else {
                let why = if targets.is_empty() {
                    "no repository function of this name was recovered".to_owned()
                } else {
                    format!("{} repository functions have this name", targets.len())
                };
                writeln!(
                    out,
                    "| ↳ {}() | not followed: {why} | — | — |",
                    Literal(name)
                )?;
                continue;
            };
            let qualified = self
                .by_id
                .get(target)
                .and_then(|n| n.props["name"].as_str())
                .unwrap_or(target);
            writeln!(
                out,
                "| ↳ {}() | followed into {} (matched by name, inferred), {PREDICATE_DEPTH} \
                 call hops | its guarded exits: | — |",
                Literal(name),
                Literal(qualified),
            )?;
            let mut seen: BTreeSet<&str> = BTreeSet::from([*target]);
            let mut order = vec![*target];
            let mut front = vec![*target];
            for _ in 0..PREDICATE_DEPTH {
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
            let mut printed = 0usize;
            let mut omitted = 0usize;
            for id in order {
                let owner = self
                    .by_id
                    .get(id)
                    .and_then(|n| n.props["name"].as_str())
                    .unwrap_or(id);
                for rule in self.governs.get(id).into_iter().flatten() {
                    let Some(exit) = guarded(rule, owner, true) else {
                        continue;
                    };
                    if printed == MAX_FOLLOWED_ROWS {
                        omitted += 1;
                        continue;
                    }
                    printed += 1;
                    let indented = Rejection {
                        owner: exit.owner,
                        ..exit
                    };
                    write_row_prefixed(&indented, "↳ ", assertions, asserted, out)?;
                }
            }
            if printed == 0 {
                writeln!(
                    out,
                    "| ↳ {}() | no guarded exit recovered in it or what it calls | — | — |",
                    Literal(name)
                )?;
            }
            if omitted > 0 {
                writeln!(
                    out,
                    "| ↳ {}() | {omitted} more guarded exits not listed; `rule-evidence.md` \
                     has them | — | — |",
                    Literal(name)
                )?;
            }
        }
        Ok(())
    }
}

fn write_row(
    row: &Rejection<'_>,
    assertions: &mut Assertions,
    asserted: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<(), RenderLimit> {
    write_row_prefixed(row, "", assertions, asserted, out)
}

fn write_row_prefixed(
    row: &Rejection<'_>,
    prefix: &str,
    assertions: &mut Assertions,
    asserted: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<(), RenderLimit> {
    let id = citation_id(&FactKey::Node {
        id: row.rule.id.clone(),
    });
    writeln!(
        out,
        "| {prefix}{} | {} | {} | {id} |",
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
        "\nRejection exits per capability: {total}. Each is a stored `throw` or `raise`, or a \
         `return` of a constructed error or an error-status response, in a callable the \
         capability's handlers reach within {CALL_DEPTH} call hops (Deterministic \
         observations). The branch conditions are the ones around the exit in its own \
         callable; the complete execution predicate and what the caller does with the outcome \
         are not established. A callable more than {SHARED_AFTER} capabilities reach is listed \
         once under shared rejections. A function a condition calls is followed by name \
         (`↳` rows, inferred): the guarded exits of the one repository function of that name \
         and of what it calls within {PREDICATE_DEPTH} hops, or why it was not followed.\n"
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
        let mut followed = BTreeSet::new();
        printed += index.each(reached.into_iter(), |row| {
            write_row(&row, assertions, asserted, out)?;
            index.follow(&row, &mut followed, assertions, asserted, out)
        })?;
        out.push_str("\n")?;
    }
    if index.each(shared(), |_| Ok(()))? > 0 {
        out.push_str("### Shared rejections\n\n")?;
        out.push_str(HEADER)?;
        let mut followed = BTreeSet::new();
        printed += index.each(shared(), |row| {
            write_row(&row, assertions, asserted, out)?;
            index.follow(&row, &mut followed, assertions, asserted, out)
        })?;
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
        // Symbols by name, and one followed predicate's traversal (#1998).
        map_slots::<&str, Vec<&str>>(rows)?,
        slots::<&str>(rows)?,
        set_slots::<&str>(rows)?,
        slots::<&str>(rows)?.checked_mul(3)?,
        set_slots::<String>(rows)?,
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
  if (qty > 20) {
    return (new OrderLimitError({ max: 20 }));
  }
  if (qty > 30) {
    return new OrderLimitError() || qty;
  }
  if (qty > 40) {
    return (new OrderLimitError(\")\"));
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
        assert_eq!(printed, 4, "{text}");
        assert_eq!(cited, 4);
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
        // A parenthesized construction is one; a construction or-ed with
        // another value is not.
        assert!(
            text.contains(
                "| checkQuantity | (qty &gt; 20) | returns (new OrderLimitError({ max: 20 })) |"
            ),
            "{text}"
        );
        assert!(!text.contains("|| qty"), "{text}");
        // A delimiter inside a string literal does not end the arguments.
        assert!(
            text.contains("| (qty &gt; 40) | returns (new OrderLimitError("),
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
        assert_eq!((printed, cited), (4, 4), "{text}");
        assert!(!text.contains("### Orders"), "{text}");
        assert!(text.contains("### Shared rejections"), "{text}");
        let (_, printed, _) = render(0, true);
        assert_eq!(printed, 0);
    }

    /// The rows one Python source's `handler_name` reaches, as one capability.
    fn python_rows(source: &str, handler_name: &str) -> (String, usize) {
        let facts = adapters_lang_python::extract_source(
            source.as_bytes(),
            "app/api/alias.py",
            &adapters_lang_python::SourceId {
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
            .find(|n| n.label == "Symbol" && n.props["name"] == handler_name)
            .unwrap();
        let handles = Edge {
            src: "ep:repo@PUT:/api/aliases/<int:alias_id>".into(),
            dst: handler.id.clone(),
            label: "HANDLES".into(),
            props: serde_json::json!({}),
        };
        let cite = BaseRef::Edge(
            handles.src.clone(),
            handles.dst.clone(),
            handles.label.clone(),
        );
        let cap = Node {
            id: "capability:test:alias".into(),
            label: "Capability".into(),
            props: serde_json::json!({ "title": "Manage alias (API)" }),
        };
        let knowledge = Stories {
            included: vec![crate::workbench::capabilities::Story {
                node: &cap,
                support: vec![(&cite, Support::Edge(&handles))],
            }],
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
        (out.finish(), printed)
    }

    // AC-0852: a Flask handler's guarded `raise` and error-status returns are
    // its rejections; a success tuple and a plain return are not.
    #[test]
    fn python_raises_and_error_status_returns_are_rejections() {
        let source = r#"
from flask import jsonify

def update_alias(alias_id, user):
    alias = Alias.get(alias_id)
    if not alias or alias.user_id != user.id:
        return jsonify(error="Forbidden, really"), 403
    if alias.disabled:
        raise AliasDisabled("disabled")
    if alias.pending:
        return ("pending", 409,)
    if alias.ok:
        return jsonify(alias=alias.email), 200
    if alias.empty:
        return None
    return jsonify(ok=True)
"#;
        let (text, printed) = python_rows(source, "update_alias");
        assert_eq!(printed, 3, "{text}");
        assert!(
            text.contains(
                "| update\\_alias | not alias or alias.user\\_id \\!= user.id | returns jsonify(error=\"Forbidden, really\"), 403 |"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "| update\\_alias | alias.disabled | throws AliasDisabled(\"disabled\") |"
            ),
            "{text}"
        );
        assert!(text.contains("returns (\"pending\", 409,)"), "{text}");
        assert!(!text.contains("200"), "{text}");
        assert!(!text.contains("returns None"), "{text}");
    }

    // AC-0896: a function a rejection's condition calls is followed by name
    // into its guarded exits and those of the method it calls on `self`; a
    // name no repository function has is named as not followed.
    #[test]
    fn a_predicate_in_a_condition_is_followed_into_its_exits() {
        let source = r#"
from flask import jsonify

class User:
    def can_create_new_alias(self):
        return self.can_create_num_aliases(1)

    def can_create_num_aliases(self, n):
        if self.disabled:
            return False
        if self.premium:
            return True
        return n <= 5

def update_alias(alias_id, user):
    if not user.can_create_new_alias():
        return jsonify(error="quota"), 400
    if not outside_check(alias_id):
        return jsonify(error="no"), 403
    return jsonify(ok=True)
"#;
        let (text, printed) = python_rows(source, "update_alias");
        assert_eq!(printed, 2, "{text}");
        assert!(
            text.contains(
                "| ↳ can\\_create\\_new\\_alias() | followed into User.can\\_create\\_new\\_alias \
                 (matched by name, inferred), 2 call hops | its guarded exits: | — |"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "| ↳ User.can\\_create\\_num\\_aliases | self.disabled | returns False |"
            ),
            "{text}"
        );
        assert!(
            text.contains("| ↳ User.can\\_create\\_num\\_aliases | self.premium | returns True |"),
            "{text}"
        );
        assert!(
            text.contains(
                "| ↳ outside\\_check() | not followed: no repository function of this name was \
                 recovered | — | — |"
            ),
            "{text}"
        );
    }

    // AC-0849: a When cell states what holds at the exit. An `else` or
    // `elif` negates a compound condition whole, `not` over a negation is
    // dropped, and conditions joined by `and` keep their own grouping.
    #[test]
    fn a_negated_compound_condition_keeps_its_grouping() {
        let shown = |text: &str, polarity| holds(text, polarity, false);
        let (t, f) = (BranchPolarity::TruthyBranch, BranchPolarity::FalsyBranch);
        assert_eq!(
            shown("not user or not user.check_password(password)", f),
            "not (not user or not user.check_password(password))"
        );
        assert_eq!(shown("not user.activated", f), "user.activated");
        assert_eq!(shown("not (a or b)", f), "(a or b)");
        assert_eq!(
            shown("output.error is not None", f),
            "not (output.error is not None)"
        );
        assert_eq!(shown("user.disabled", f), "not user.disabled");
        assert_eq!(shown("a or b", t), "(a or b)");
        assert_eq!(shown("not user.activated", t), "not user.activated");
        assert_eq!(holds("a or b", t, true), "a or b");
        assert_eq!(shown("f(a, b)", t), "f(a, b)");
        assert_eq!(shown("x == \"a b\"", t), "(x == \"a b\")");
        assert!(atomic("data.get(\"a b\")"));

        let (text, printed) = python_rows(
            r#"
def login(user, password):
    if not user or not user.check_password(password):
        raise LoginError("Email or password incorrect")
    elif not user.activated:
        raise LoginError("Account not activated")
"#,
            "login",
        );
        assert_eq!(printed, 2, "{text}");
        assert!(
            text.contains(
                "| login | not (not user or not user.check\\_password(password)) and not user.activated |"
            ),
            "{text}"
        );
    }
}
