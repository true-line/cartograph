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
//! With areas (#260), each area's capabilities are followed by the rules in
//! the area's own code that none of them reach (#2066): the rejections and
//! changes of every callable defined in one of the area's member files that
//! no capability of the area reaches and that is not shared plumbing, at
//! most [`MAX_AREA_OWN_ROWS`] rejections per area. Code reached only from
//! another area, a command or a job, or through injected configuration, is
//! then still stated where an area's reader, or its topic expert, looks.
//!
//! A function a rejection's condition calls (`user.can_create_new_alias()`)
//! is followed by its name (#1998, AC-0886): when exactly one recovered
//! Symbol has that last name segment, its guarded exits and those of what it
//! calls within [`PREDICATE_DEPTH`] hops are listed under the row, marked
//! `↳` and as matched by name; otherwise a `↳` row names the predicate and
//! why it was not followed, so the condition is never left unexplained.
//!
//! A configuration constant a row names (`config.MAX_NB_EMAIL_FREE_PLAN`) is
//! stated under it (#1998, AC-0904) when exactly one `ConfigRead` records a
//! literal default bound to a module constant of that name: the default and
//! the environment variable that replaces it, as a `↳` row matched by name.
//!
//! Only the stored, sanitized displays the rule inventory renders are shown
//! (SPEC-02); nothing is read from source. The rows are observations: the
//! complete execution predicate and the consumer effect stay unestablished,
//! and `rule-evidence.md` keeps every observation with its dependencies.

use super::statements::Constants;
use crate::workbench::capabilities::{Stories, Story, Support, area_name};
use crate::workbench::citation_id;
use crate::workbench::render::{Assertions, Literal, RenderLimit, Text};
use core_graph::rules::{
    BranchPolarity, DefinitionExpressionKind, DefinitionExpressionNode, GuardedExitEvidence,
    LocalExit, SourceExpression,
};
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
    /// The rule in domain terms, when its condition and outcome are within
    /// the rendering rules (#1999, AC-0905).
    statement: Option<String>,
    /// Where the rule is, in a reader's terms, when its owner's name is not
    /// (an inline function: the route it handles, #2009).
    place: Option<String>,
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
pub(super) fn constructs_error(value: &SourceExpression) -> bool {
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
        // A handler's error types (AC-1036): `raised X`, `not raised X`.
        BranchPolarity::RaisedBranch => format!("raised {shown}"),
        BranchPolarity::UnraisedBranch => format!("not raised {shown}"),
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

fn rejection<'a>(rule: &'a Node, owner: &'a str, constant: Constants<'_>) -> Option<Rejection<'a>> {
    guarded(rule, owner, false, constant).or_else(|| choice(rule, owner, constant))
}

/// An unguarded answer that states how a value is chosen (a predicate's
/// answer, a pick with a fallback, a strategy's answer: AC-0956, AC-0915)
/// as a row beside the rejections: the adapters record an exit with no
/// branch around it only for these.
fn choice<'a>(rule: &'a Node, owner: &'a str, constant: Constants<'_>) -> Option<Rejection<'a>> {
    let parsed = crate::rules::rule_payload(rule).ok()?;
    if !parsed.conditions.is_empty()
        || !matches!(parsed.effect, LocalExit::Return { value: Some(_) })
    {
        return None;
    }
    guarded(rule, owner, true, constant)
}

/// A guarded effect (an assignment or a call statement, #1998) as a row.
fn effect<'a>(rule: &'a Node, owner: &'a str, constant: Constants<'_>) -> Option<Rejection<'a>> {
    let parsed = crate::rules::rule_payload(rule).ok()?;
    if parsed.effect.exits() || writes_refusal(&parsed) {
        return None;
    }
    guarded(rule, owner, true, constant)
}

/// A Go call writing an error status before a bare return (#2009): a
/// rejection, not a change.
/// The call each condition of a Go rule tests the error of (`Store.Save`),
/// as the adapter recorded it, one per condition (#2009).
fn failed_calls(rule: &Node) -> Vec<Option<String>> {
    rule.props
        .get("failed_calls")
        .and_then(|v| v.as_array())
        .map(|calls| {
            calls
                .iter()
                .map(|c| c.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn writes_refusal(evidence: &GuardedExitEvidence) -> bool {
    super::statements::is_go(evidence)
        && matches!(&evidence.effect, LocalExit::Call { value }
            if super::statements::go_response(value.display.as_str())
                .is_some_and(|(status, _)| status >= 400))
}

/// A guarded exit as a row; with `any`, every exit, not only rejections.
fn guarded<'a>(
    rule: &'a Node,
    owner: &'a str,
    any: bool,
    constant: Constants<'_>,
) -> Option<Rejection<'a>> {
    let parsed = crate::rules::rule_payload(rule).ok()?;
    let outcome = match &parsed.effect {
        LocalExit::Throw { value } => format!("throws {}", one_line(value.display.as_str())),
        LocalExit::Return { value: Some(value) }
            if any
                || constructs_error(value)
                || returns_error_status(value)
                || (super::statements::is_go(&parsed)
                    && super::statements::go_error(value.display.as_str()).is_some()) =>
        {
            format!("returns {}", one_line(value.display.as_str()))
        }
        LocalExit::Return { value: None } if any => "returns nothing".to_owned(),
        LocalExit::Return { .. } => return None,
        LocalExit::Assign { target, value } if any => format!(
            "sets {} to {}",
            one_line(target.display.as_str()),
            one_line(value.display.as_str())
        ),
        LocalExit::Call { value } if writes_refusal(&parsed) => {
            format!("responds {}", one_line(value.display.as_str()))
        }
        LocalExit::Call { value } if any => format!("calls {}", one_line(value.display.as_str())),
        LocalExit::Assign { .. } | LocalExit::Call { .. } => return None,
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
    let statement = super::statements::statement(&parsed, owner, constant, &failed_calls(rule));
    Some(Rejection {
        rule,
        owner,
        when,
        outcome,
        statement,
        place: None,
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
    /// Configuration reads with a recorded literal default, by the module
    /// constant bound to them (#1998, AC-0904).
    constants: BTreeMap<&'a str, Vec<(&'a Node, &'a serde_json::Value)>>,
    /// Module string constants by name, with their file and module stem
    /// (#2027).
    strings: BTreeMap<&'a str, Vec<(&'a Node, &'a str, &'a str)>>,
    /// What each process start or scheduled job enters: a callable, or a
    /// file standing for the callables defined in it (#2027).
    entries: BTreeMap<&'a str, Vec<&'a str>>,
    /// Callables by the file that defines them.
    defined: BTreeMap<&'a str, Vec<&'a str>>,
    /// The Endpoints each Symbol handles.
    handled: BTreeMap<&'a str, Vec<&'a Node>>,
    /// Declarative state machines (#2067), in id order.
    machines: Vec<&'a Node>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut calls: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut governs: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
        let mut entries: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut defined: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut handled: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
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
                "ENTRY" => entries.entry(&src.id).or_default().push(&dst.id),
                "HANDLES" if src.label == "Endpoint" && dst.label == "Symbol" => {
                    handled.entry(&dst.id).or_default().push(src)
                }
                "DEFINED_IN" if src.label == "Symbol" && dst.label == "File" => {
                    defined.entry(&dst.id).or_default().push(&src.id)
                }
                _ => {}
            }
        }
        let mut constants: BTreeMap<&str, Vec<(&Node, &serde_json::Value)>> = BTreeMap::new();
        for node in nodes.iter().filter(|n| n.label == "ConfigRead") {
            for default in node.props["defaults"].as_array().into_iter().flatten() {
                if let Some(name) = default["constant"].as_str() {
                    constants
                        .entry(name)
                        .or_default()
                        .push((node, &default["value"]));
                }
            }
        }
        let mut strings: BTreeMap<&str, Vec<(&Node, &str, &str)>> = BTreeMap::new();
        for node in nodes.iter().filter(|n| n.label == "File") {
            let stem = node.props["path"]
                .as_str()
                .and_then(|p| p.rsplit('/').next())
                .and_then(|f| f.strip_suffix(".py"))
                .unwrap_or_default();
            for (name, value) in node.props["string_constants"]
                .as_object()
                .into_iter()
                .flatten()
            {
                if let Some(value) = value.as_str() {
                    strings.entry(name).or_default().push((node, value, stem));
                }
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
            constants,
            strings,
            entries,
            defined,
            handled,
            machines: nodes
                .iter()
                .copied()
                .filter(|n| n.label == "StateMachine")
                .collect(),
        }
    }

    /// The literal of the module string constant `name`, when exactly one
    /// module binds that name (#2027, matched by name, inferred).
    fn constant(&self, name: &str) -> Option<String> {
        match self.strings.get(name).map(Vec::as_slice) {
            Some([(_, value, _)]) => Some((*value).to_owned()),
            _ => None,
        }
    }

    /// A `↳` row for each configuration constant `row` names (by an
    /// identifier token equal to the constant), once per `explained` set:
    /// its default and environment variable when exactly one recorded read
    /// binds that name, else why it is not stated (#1998, AC-0904).
    fn explain_constants(
        &self,
        row: &Rejection<'_>,
        explained: &mut BTreeSet<String>,
        assertions: &mut Assertions,
        asserted: &mut BTreeSet<String>,
        out: &mut Text,
    ) -> Result<(), RenderLimit> {
        let text = format!("{} {}", row.when, row.outcome);
        let tokens = text
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|t| !t.is_empty());
        for token in tokens {
            let Some(found) = self.constants.get(token) else {
                continue;
            };
            if !explained.insert(token.to_owned()) {
                continue;
            }
            let [(read, value)] = found[..] else {
                writeln!(
                    out,
                    "| ↳ | The configured {} is not stated: {} recorded reads bind this name | \
                     {} | — |",
                    Literal(super::statements::words(token)),
                    found.len(),
                    Literal(token),
                )?;
                continue;
            };
            let key = read.props["key"].as_str().unwrap_or(token);
            let id = citation_id(&FactKey::Node {
                id: read.id.clone(),
            });
            writeln!(
                out,
                "| ↳ | The configured {} is {} unless the environment variable {} is set \
                 (matched by name, inferred). | {} | {id} |",
                Literal(super::statements::words(token)),
                Literal(value.to_string()),
                Literal(key),
                Literal(token),
            )?;
            if asserted.insert(id) {
                assertions.node(read, None)?;
            }
        }
        Ok(())
    }

    /// The governed callables a capability's handlers reach, nearest first.
    /// Handlers are the destinations of the `HANDLES` edges it cites, and
    /// what a cited process start or scheduled job enters: a callable, or
    /// each callable defined in an entered file (#2027).
    fn reached(&self, story: &Story<'a>) -> Vec<&'a str> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut order: Vec<&str> = Vec::new();
        let mut front: Vec<&str> = story
            .support
            .iter()
            .flat_map(|(_, fact)| -> Vec<&'a str> {
                match fact {
                    Support::Edge(e) if e.label == "HANDLES" => vec![e.dst.as_str()],
                    Support::Node(n) if matches!(n.label.as_str(), "EntryPoint" | "Schedule") => {
                        self.entries
                            .get(n.id.as_str())
                            .into_iter()
                            .flatten()
                            .flat_map(|dst| match self.defined.get(dst) {
                                Some(inside) => inside.clone(),
                                None => vec![*dst],
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                }
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

    /// Where an inline function's rule is, for a reader (#2009): the route
    /// it handles (`handler of GET /devices`), or the function it is
    /// written in. `None` for a named callable, whose name says it.
    fn place(&self, owner: &str, name: &str) -> Option<String> {
        let (enclosing, _) = name.split_once("/func@")?;
        let mut routes: Vec<String> = self
            .handled
            .get(owner)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                Some(format!(
                    "{} {}",
                    e.props["method"].as_str()?,
                    e.props["path"].as_str()?
                ))
            })
            .collect();
        routes.sort();
        routes.dedup();
        Some(match routes.as_slice() {
            [] => format!("inline function in {enclosing}"),
            [one] => format!("handler of {one}"),
            [first, rest @ ..] => format!("handler of {first} and {} more", rest.len()),
        })
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
                if let Some(mut found) = rejection(rule, name, Some(&|k| self.constant(k)))
                    .or_else(|| self.mail_reply(rule, owner, name))
                {
                    found.place = self.place(owner, name);
                    n += 1;
                    row(found)?;
                }
            }
        }
        Ok(n)
    }

    /// A `return` naming a module string constant that holds an SMTP reply
    /// (`status.E502` = “550 …”, matched by name and module stem,
    /// inferred) as the mail's reply (#2027, AC-0916).
    fn mail_reply(&self, rule: &'a Node, owner_id: &str, owner: &'a str) -> Option<Rejection<'a>> {
        let parsed = crate::rules::rule_payload(rule).ok()?;
        let LocalExit::Return { value: Some(value) } = &parsed.effect else {
            return None;
        };
        let display = value.display.as_str();
        let reply = |module: Option<&str>, name: &str| -> Option<(u16, &'a str)> {
            let found = self.strings.get(name)?;
            let picked: Vec<&(&Node, &str, &str)> = found
                .iter()
                .filter(|(_, _, stem)| module.is_none_or(|m| m == *stem))
                .collect();
            let [(_, literal, _)] = picked[..] else {
                return None;
            };
            let code: u16 = literal.get(..3)?.parse().ok()?;
            // 2xx, 4xx and 5xx end the mail's handling; a 3xx reply is an
            // intermediate step, not a decision.
            (matches!(code, 200..=299 | 400..=599) && literal.as_bytes().get(3) == Some(&b' '))
                .then_some((code, *literal))
        };
        let mut replies: Vec<(u16, &str)> = smtp_constants(display)
            .filter_map(|(module, name)| reply(module, name))
            .collect();
        // A returned local bound to reply constants in the function
        // (`res_status = status.E502`): each reply it may hold.
        let mut bound = false;
        if replies.is_empty() {
            let locals = self
                .by_id
                .get(owner_id)
                .map(|n| &n.props["constant_locals"]);
            for token in display.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                // Only a binding before this return can be what it answers.
                let before = parsed.exit_source.byte_start;
                for value in locals
                    .and_then(|l| l[token].as_array())
                    .into_iter()
                    .flatten()
                    .filter(|v| v["at"].as_u64().is_some_and(|at| at < before))
                    .filter_map(|v| v["value"].as_str())
                {
                    let (module, name) = match value.rsplit_once('.') {
                        Some((m, n)) => (Some(m.rsplit('.').next().unwrap_or(m)), n),
                        None => (None, value),
                    };
                    if let Some(found) = reply(module, name)
                        && !replies.contains(&found)
                    {
                        replies.push(found);
                        bound = true;
                    }
                }
            }
        }
        let (&(code, _), _) = replies.split_first()?;
        if !bound && replies.len() > 1 {
            return None;
        }
        let literal = replies
            .iter()
            .map(|(_, l)| format!("“{l}”"))
            .collect::<Vec<_>>()
            .join(" or ");
        let clause = if bound && replies.len() > 1 {
            format!("the mail is answered {literal}, as the function sets it")
        } else {
            match code {
                200..=299 => format!("the mail is accepted with the reply {literal}"),
                400..=499 => format!("the mail is deferred with the reply {literal}"),
                _ => format!("the mail is refused with the reply {literal}"),
            }
        };
        let literal = literal.replace(['“', '”'], "");
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
            outcome: format!("returns {} ({literal})", one_line(display)),
            statement: super::statements::with_outcome(
                &parsed,
                owner,
                &clause,
                Some(&|k| self.constant(k)),
                &failed_calls(rule),
            ),
            place: None,
        })
    }

    /// Each rule of `owners` that `pick` makes a row of, in order.
    fn each_picked(
        &self,
        owners: impl Iterator<Item = &'a str>,
        pick: fn(&'a Node, &'a str, Constants<'_>) -> Option<Rejection<'a>>,
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
                if let Some(mut found) = pick(rule, name, Some(&|k| self.constant(k))) {
                    found.place = self.place(owner, name);
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
    const SKIP: [&str; 17] = [
        // Go request reads and standard calls (#2009).
        "FormValue",
        "PostFormValue",
        "Get",
        "Query",
        "Error",
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

/// The dotted receiver of the first `receiver.name(` call in `when`:
/// `g.user` in `not g.user.is_active()`.
fn receiver<'w>(when: &'w str, name: &str) -> Option<&'w str> {
    let at = when.find(&format!(".{name}("))?;
    let before = &when[..at];
    let start = before
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .map_or(0, |p| p + 1);
    Some(&before[start..]).filter(|r| !r.is_empty())
}

/// The class a receiver names (#2029): `self`/`cls` the owner's class,
/// `g.user` and `current_user` the `User` login model, otherwise the
/// receiver's last segment in CamelCase (`api_key` → `ApiKey`).
fn receiver_class(receiver: &str, owner: &str) -> Option<String> {
    let last = receiver.rsplit('.').next()?;
    match (receiver, last) {
        (_, "self" | "cls") => {
            let (class, _) = owner.rsplit_once('.')?;
            Some(class.rsplit('.').next()?.to_owned())
        }
        ("g.user" | "current_user", _) => Some("User".to_owned()),
        _ => {
            let class: String = last
                .split('_')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let mut c = p.chars();
                    c.next()
                        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                        .unwrap_or_default()
                })
                .collect();
            Some(class).filter(|c| !c.is_empty())
        }
    }
}

/// `module.NAME` and bare `NAME` tokens of upper-case constants in a
/// display: `[(True, status.E502)]` gives `(Some("status"), "E502")`.
fn smtp_constants(display: &str) -> impl Iterator<Item = (Option<&str>, &str)> {
    display
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter_map(|token| {
            let (module, name) = match token.rsplit_once('.') {
                Some((module, name)) => (Some(module.rsplit('.').next()?), name),
                None => (None, token),
            };
            (name.len() > 1
                && name.starts_with(|c: char| c.is_ascii_uppercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .then_some((module, name))
        })
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
        (followed, explained): &mut (&mut BTreeSet<String>, &mut BTreeSet<String>),
        assertions: &mut Assertions,
        asserted: &mut BTreeSet<String>,
        out: &mut Text,
    ) -> Result<(), RenderLimit> {
        // A condition's local assigned a call of a plain function name is
        // followed into that function too (AC-1036): `alias_suffix =
        // check_suffix_signature(signed_suffix)` under `not alias_suffix`.
        let mut scan = row.when.clone();
        if let Ok(parsed) = crate::rules::rule_payload(row.rule) {
            // Only a definition the condition itself reads (AC-1040): the
            // locals its value reads in turn are stated, not followed.
            let tested = |definition: &&core_graph::rules::LocalDefinition| {
                definition.uses.iter().any(|u| {
                    parsed.conditions.iter().any(|c| {
                        c.expression.source.byte_start <= u.byte_start
                            && u.byte_end <= c.expression.source.byte_end
                    })
                })
            };
            for definition in parsed.local_definitions.iter().flatten().filter(tested) {
                // Only a recorded call of a plain name: an unsupported
                // initializer (`a() if f else b()`) is not one call's value.
                let nodes = &definition.expression.nodes;
                let plain_call = match nodes.get(definition.expression.root as usize) {
                    Some(DefinitionExpressionNode {
                        kind: DefinitionExpressionKind::Call { callee, .. },
                        ..
                    }) => nodes.get(*callee as usize).is_some_and(|c| {
                        matches!(c.kind, DefinitionExpressionKind::Identifier { .. })
                    }),
                    _ => false,
                };
                if plain_call {
                    scan.push_str(" and ");
                    scan.push_str(&one_line(definition.initializer.display.as_str()));
                }
            }
        }
        for name in called_names(&scan) {
            let named = self
                .by_name
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            // Several functions of this name: the one on the receiver's
            // class, when the receiver names one (#2029).
            let narrowed: Vec<&str>;
            let mut matched = "matched by name, inferred".to_owned();
            let targets = if named.len() > 1
                && let Some(class) =
                    receiver(&row.when, name).and_then(|r| receiver_class(r, row.owner))
            {
                narrowed = named
                    .iter()
                    .copied()
                    .filter(|t| {
                        self.by_id
                            .get(t)
                            .and_then(|n| n.props["name"].as_str())
                            .is_some_and(|q| {
                                q.strip_suffix(name)
                                    .and_then(|c| c.strip_suffix('.'))
                                    .is_some_and(|c| {
                                        c == class || c.ends_with(&format!(".{class}"))
                                    })
                            })
                    })
                    .collect();
                if narrowed.len() == 1 {
                    matched = format!("matched by name on the receiver's class {class}, inferred");
                    &narrowed[..]
                } else {
                    named
                }
            } else {
                named
            };
            // Followed once per table, per function followed.
            let key = match targets {
                [one] => (*one).to_owned(),
                _ => name.to_owned(),
            };
            if !followed.insert(key) {
                continue;
            }
            let [target] = targets else {
                let why = if targets.is_empty() {
                    "no repository function of this name was recovered".to_owned()
                } else {
                    format!("{} repository functions have this name", targets.len())
                };
                writeln!(
                    out,
                    "| ↳ | The check “{}” is not followed: {why}. | {}() | — |",
                    Literal(super::statements::words(name)),
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
                "| ↳ | The check “{}” decides as follows ({matched}, \
                 {PREDICATE_DEPTH} call hops): | {}() in {} | — |",
                Literal(super::statements::words(name)),
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
                    let Some(exit) = guarded(rule, owner, true, Some(&|k| self.constant(k))) else {
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
                    self.explain_constants(&indented, explained, assertions, asserted, out)?;
                }
            }
            if printed == 0 {
                writeln!(
                    out,
                    "| ↳ | No guarded exit is recovered in the check “{}” or what it calls. | \
                     {}() | — |",
                    Literal(super::statements::words(name)),
                    Literal(name)
                )?;
            }
            if omitted > 0 {
                writeln!(
                    out,
                    "| ↳ | {omitted} more guarded exits of the check “{}” are not listed; \
                     `rule-evidence.md` has them. | {}() | — |",
                    Literal(super::statements::words(name)),
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
    // The statement is the rule; the stored condition and outcome are its
    // source, kept beside it as evidence (#1999).
    let rule = row.statement.as_deref().unwrap_or(NOT_STATED);
    let when = if row.when == "always" {
        "always".to_owned()
    } else {
        format!("when {}", row.when)
    };
    writeln!(
        out,
        "| {prefix}{} | {} | {}: {} | {id} |",
        Literal(row.place.as_deref().unwrap_or(row.owner)),
        Literal(rule),
        Literal(&when),
        Literal(&row.outcome),
    )?;
    if asserted.insert(id) {
        assertions.node(row.rule, None)?;
    }
    Ok(())
}

const HEADER: &str = "| Where | Rule | Source | Citation |\n|---|---|---|---|\n";

/// How the Rule and Source columns read, for each table's lead (#1999).
const RULE_COLUMNS: &str = "The Rule column restates each exit in domain terms when its \
    condition and outcome are simple enough to render by rule (a presence or flag test, a \
    comparison with a literal or configured value, a test for no value, membership of a literal \
    set, an ownership test, and `and`/`or`/`not` of these), keeping the fact's tier; otherwise \
    it states a Gap. The Source column is the stored condition and outcome, kept as evidence, \
    not as the requirement.";

/// The Rule cell of an exit whose condition or outcome is outside the
/// rendering rules: a Gap, never the source expression (#1999).
pub(super) const NOT_STATED: &str =
    "Not stated in domain terms (Gap): the condition or outcome is outside the rendering rules";

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
    if index.governs.is_empty() && index.machines.is_empty() {
        return Ok(0);
    }
    let listed = knowledge.capabilities();
    let capabilities = || listed.iter().map(|(_, story)| *story);
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
    // Counted first so the lead states the totals before any row.
    let mut total = index.each(shared(), |_| Ok(()))?;
    let mut changes = index.each_picked(shared(), effect, |_| Ok(()))?;
    for story in capabilities() {
        total += index.each(index.reached(story).into_iter().filter(own), |_| Ok(()))?;
        changes +=
            index.each_picked(index.reached(story).into_iter().filter(own), effect, |_| {
                Ok(())
            })?;
    }
    // Each area's own callables no capability of it reaches (#2066).
    let own_code = index.area_own_code(knowledge, &listed, &reach_count);
    for (_, owners) in &own_code {
        total += index.each(owners.iter().copied(), |_| Ok(()))?;
        changes += index.each_picked(owners.iter().copied(), effect, |_| Ok(()))?;
    }
    let (machines, loose_machines) = index.area_machines(knowledge);
    if total + changes + index.machines.len() == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "\nRejection exits per capability: {total}. Each is a stored `throw` or `raise`, or a \
         `return` of a constructed error, an error-status response or a Go error value, or a Go \
         error-status write before a bare `return`, in a callable the capability's handlers reach within {CALL_DEPTH} call hops (Deterministic \
         observations). The branch conditions are the ones around the exit in its own \
         callable; the complete execution predicate and what the caller does with the outcome \
         are not established. A callable more than {SHARED_AFTER} capabilities reach is listed \
         once under shared rejections. A function a condition calls is followed by name \
         (`↳` rows, inferred): the guarded exits of the one repository function of that name \
         and of what it calls within {PREDICATE_DEPTH} hops, or why it was not followed. A \
         configuration constant a row names is stated with its recorded default and \
         environment variable (`↳` rows, matched by name, inferred). {RULE_COLUMNS}\n\n\
         State changes and calls per capability: {changes} (#1998). Under each capability, \
         “Changes” lists each stored assignment through an attribute (`x.field = value`) and \
         each call statement whose result is unused, in the same reached callables, with the \
         branch conditions around it (`always` when there are none). Logging calls, builtins \
         and constructors are not listed. Whether a statement runs on a given request, and \
         what a called function then does, are not established. At most {MAX_EFFECT_ROWS} per \
         capability, nearest callable first.\n"
    )?;
    if !index.machines.is_empty() {
        writeln!(
            out,
            "State machines (#2067): {}. Each is a `transitions` table declared as data: every \
             state with the states it may move to, as written (Deterministic), stated first \
             in its area. A state that may move nowhere is final. The guarded exits of a hook \
             beside the table that the \
             framework runs before a move (`onTransitionStart`) are listed under it. Whether the \
             framework applies further moves is not established.\n",
            index.machines.len()
        )?;
    }
    if !own_code.is_empty() {
        writeln!(
            out,
            "Rules in an area's own code (#2066): after an area's capabilities, the \
             rejections and changes of the callables defined in the area's member files that \
             none of its capabilities reach, and that are not shared, at most \
             {MAX_AREA_OWN_ROWS} rejections per area. They may be reached from another \
             area's capability, a command or a job, through injected configuration, or not at \
             all; who calls them is not established here.\n"
        )?;
    }
    let mut printed = 0;
    let mut headings = crate::workbench::document::AreaHeadings::new(knowledge);
    // Areas in name order, each one's state machines first (its lifecycle,
    // which a briefing cut from the front keeps, #2069), then its
    // capabilities, then its own code; the capabilities with no area last,
    // as `capabilities()` lists them.
    let mut named: Vec<&Node> = listed
        .iter()
        .filter_map(|(area, _)| *area)
        .chain(own_code.iter().map(|(area, _)| *area))
        .chain(machines.iter().map(|(area, _)| *area))
        .collect();
    named.sort_by(|a, b| area_order(a).cmp(&area_order(b)));
    named.dedup_by(|a, b| a.id == b.id);
    let mut units: Vec<Unit<'_, '_>> = Vec::new();
    for area in named {
        if machines.iter().any(|(a, _)| a.id == area.id) {
            units.push(Unit::Machines(area));
        }
        units.extend(
            listed
                .iter()
                .filter(|(a, _)| a.is_some_and(|a| a.id == area.id))
                .map(|(a, story)| Unit::Story(*a, story)),
        );
        if own_code.iter().any(|(a, _)| a.id == area.id) {
            units.push(Unit::Own(area));
        }
    }
    units.extend(
        listed
            .iter()
            .filter(|(a, _)| a.is_none())
            .map(|(a, story)| Unit::Story(*a, story)),
    );
    for unit in units {
        let (area, story) = match unit {
            Unit::Machines(area) => {
                for machine in machines
                    .iter()
                    .filter(|(a, _)| a.id == area.id)
                    .flat_map(|(_, m)| m)
                {
                    let marks = headings.before(Some(area), out)?;
                    printed += state_machine(&index, machine, marks, assertions, asserted, out)?;
                }
                continue;
            }
            Unit::Own(area) => {
                if let Some((_, owners)) = own_code.iter().find(|(a, _)| a.id == area.id) {
                    printed +=
                        index.own_code(owners, area, &mut headings, assertions, asserted, out)?;
                }
                continue;
            }
            Unit::Story(area, story) => (area, story),
        };
        let reached: Vec<&str> = index.reached(story).into_iter().filter(own).collect();
        let rejections = index.each(reached.iter().copied(), |_| Ok(()))?;
        let changed = index.each_picked(reached.iter().copied(), effect, |_| Ok(()))?;
        if rejections + changed == 0 {
            continue;
        }
        let marks = headings.before(area, out)?;
        writeln!(
            out,
            "{marks} {}\n",
            Literal(crate::workbench::render::NodeName(story.node))
        )?;
        let mut followed = BTreeSet::new();
        let mut explained = BTreeSet::new();
        if rejections > 0 {
            out.push_str(HEADER)?;
            printed += index.each(reached.iter().copied(), |row| {
                write_row(&row, assertions, asserted, out)?;
                index.explain_constants(&row, &mut explained, assertions, asserted, out)?;
                index.follow(
                    &row,
                    &mut (&mut followed, &mut explained),
                    assertions,
                    asserted,
                    out,
                )
            })?;
            out.push_str("\n")?;
        }
        printed += index.changes(&reached, &mut explained, assertions, asserted, out)?;
    }
    if !loose_machines.is_empty() {
        out.push_str("### State machines\n\n")?;
        for machine in loose_machines {
            printed += state_machine(&index, machine, "####", assertions, asserted, out)?;
        }
    }
    let shared: Vec<&str> = shared().collect();
    if index.each(shared.iter().copied(), |_| Ok(()))? > 0 {
        out.push_str("### Shared rejections\n\n")?;
        out.push_str(HEADER)?;
        let mut followed = BTreeSet::new();
        let mut explained = BTreeSet::new();
        printed += index.each(shared.iter().copied(), |row| {
            write_row(&row, assertions, asserted, out)?;
            index.explain_constants(&row, &mut explained, assertions, asserted, out)?;
            index.follow(
                &row,
                &mut (&mut followed, &mut explained),
                assertions,
                asserted,
                out,
            )
        })?;
        out.push_str("\n")?;
    }
    if index.each_picked(shared.iter().copied(), effect, |_| Ok(()))? > 0 {
        out.push_str("### Shared changes\n\n")?;
        printed += index.changes(&shared, &mut BTreeSet::new(), assertions, asserted, out)?;
    }
    Ok(printed)
}

/// One part of §1.5, in print order.
enum Unit<'s, 'a> {
    /// An area's state machines (#2067), first in the area (#2069).
    Machines(&'a Node),
    /// A capability's rules, under its area if it has one.
    Story(Option<&'a Node>, &'s Story<'a>),
    /// The rules in an area's own code (#2066), last in the area.
    Own(&'a Node),
}

/// Change rows printed per capability (#1998).
pub(super) const MAX_EFFECT_ROWS: usize = 16;

/// Rejection rows printed for one area's own code (#2066).
pub(super) const MAX_AREA_OWN_ROWS: usize = 32;

/// One state machine's table (#2067): its states, then each state with the
/// states it may move to, citing the machine. Returns the rows printed.
fn state_machine<'a>(
    index: &Index<'a>,
    machine: &'a Node,
    marks: &str,
    assertions: &mut Assertions,
    asserted: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let id = citation_id(&FactKey::Node {
        id: machine.id.clone(),
    });
    let name = machine.props["name"].as_str().unwrap_or("transitions");
    fn names(value: &serde_json::Value) -> Vec<&str> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect()
    }
    let states = names(&machine.props["states"]);
    writeln!(out, "{marks} State machine: {}\n", Literal(name))?;
    let declared = machine.props["declared_type"]
        .as_str()
        .map(|t| format!(" (`{}`)", Literal(t)))
        .unwrap_or_default();
    writeln!(
        out,
        "{}{declared}, in `{}`, has {} states: {}.\n",
        Literal(name),
        Literal(machine.props["file"].as_str().unwrap_or_default()),
        states.len(),
        Literal(&states.join(", "))
    )?;
    out.push_str("| State | May move to | Citation |\n|---|---|---|\n")?;
    let mut rows = 0;
    for transition in machine.props["transitions"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let to = names(&transition["to"]);
        let to = if to.is_empty() {
            "nothing (final)".to_owned()
        } else {
            to.join(", ")
        };
        writeln!(
            out,
            "| {} | {} | {id} |",
            Literal(transition["from"].as_str().unwrap_or_default()),
            Literal(&to)
        )?;
        rows += 1;
    }
    out.push_str("\n")?;
    if asserted.insert(id) {
        assertions.node(machine, None)?;
    }
    // The guarded exits of the hooks beside the table (`onTransitionStart`),
    // every exit: a hook refuses a move by what it returns or throws.
    let file = machine.props["file"].as_str().unwrap_or_default();
    let repo = machine.props["prov"]["evidence"][0]["repo"]
        .as_str()
        .unwrap_or_default();
    let prefix = format!("sym:{repo}@{file}#");
    for hook in machine.props["hooks"].as_array().into_iter().flatten() {
        let (Some(hook_name), Some(start)) = (hook["name"].as_str(), hook["byte_start"].as_u64())
        else {
            continue;
        };
        let suffix = format!(".{hook_name}@{start}");
        let owners: Vec<&str> = index
            .governs
            .keys()
            .copied()
            .filter(|id| id.starts_with(&prefix) && id.ends_with(&suffix))
            .collect();
        let mut guards = Vec::new();
        for owner in owners {
            for rule in index.governs.get(owner).into_iter().flatten() {
                if let Some(row) = guarded(rule, hook_name, true, Some(&|k| index.constant(k))) {
                    guards.push(row);
                }
            }
        }
        if guards.is_empty() {
            continue;
        }
        // Only `onTransitionStart` runs before a move and can refuse it;
        // `onTransitionEnd` runs after it (AC-1032).
        if hook_name == "onTransitionEnd" {
            writeln!(
                out,
                "Effects in `{}`, which the framework runs after a move (`fromState`, `toState`):\n",
                Literal(hook_name)
            )?;
        } else {
            writeln!(
                out,
                "Guards in `{}`, which the framework runs before a move (`fromState`, `toState`); \
                 a returned message or a throw refuses the move:\n",
                Literal(hook_name)
            )?;
        }
        out.push_str(HEADER)?;
        for row in &guards {
            write_row(row, assertions, asserted, out)?;
            rows += 1;
        }
        out.push_str("\n")?;
    }
    Ok(rows)
}

/// The order areas are listed in: by name, then id.
fn area_order(area: &Node) -> (&str, &str) {
    (area_name(area), area.id.as_str())
}

impl<'a> Index<'a> {
    /// Each area, in [`area_order`], with the callables defined in its
    /// member files that hold a rule row, that none of its capabilities
    /// reach and that are not shared (more than [`SHARED_AFTER`]
    /// capabilities reach them), in file then definition order (#2066).
    /// An area with none is left out.
    fn area_own_code(
        &self,
        knowledge: &Stories<'a>,
        listed: &[(Option<&'a Node>, &Story<'a>)],
        reach_count: &BTreeMap<&'a str, usize>,
    ) -> Vec<(&'a Node, Vec<&'a str>)> {
        if knowledge.file_areas.is_empty() {
            return Vec::new();
        }
        // Member files by area, then one area at a time: the callables its
        // capabilities reach are held for that area only, so what is
        // retained stays within the graph's rows (never area × callable).
        let mut by_area: BTreeMap<(&str, &str), (&'a Node, Vec<&'a str>)> = BTreeMap::new();
        for (file, area) in &knowledge.file_areas {
            by_area
                .entry(area_order(area))
                .or_insert_with(|| (*area, Vec::new()))
                .1
                .push(file);
        }
        for (area, members) in by_area.values_mut() {
            let mut mine: BTreeSet<&str> = BTreeSet::new();
            for (_, story) in listed
                .iter()
                .filter(|(a, _)| a.is_some_and(|a| a.id == area.id))
            {
                mine.extend(self.reached(story));
            }
            let files = std::mem::take(members);
            for file in files {
                for callable in self.defined.get(file).into_iter().flatten().copied() {
                    if reach_count.get(callable).is_some_and(|n| *n > SHARED_AFTER)
                        || mine.contains(callable)
                        || !self.governs.contains_key(callable)
                    {
                        continue;
                    }
                    members.push(callable);
                }
            }
        }
        by_area
            .into_values()
            .filter(|(_, owners)| {
                let rows = self.each(owners.iter().copied(), |_| Ok(())).unwrap_or(0);
                let changes = self
                    .each_picked(owners.iter().copied(), effect, |_| Ok(()))
                    .unwrap_or(0);
                rows + changes > 0
            })
            .collect()
    }

    /// The state machines by the area of the file that declares them, keyed
    /// in [`area_order`], and those in no area (#2067).
    fn area_machines(
        &self,
        knowledge: &Stories<'a>,
    ) -> (Vec<(&'a Node, Vec<&'a Node>)>, Vec<&'a Node>) {
        let mut by_area: BTreeMap<(&str, &str), (&'a Node, Vec<&'a Node>)> = BTreeMap::new();
        let mut loose = Vec::new();
        for machine in &self.machines {
            let evidence = &machine.props["prov"]["evidence"][0];
            let file = match (evidence["repo"].as_str(), evidence["path"].as_str()) {
                (Some(repo), Some(path)) => format!("file:{repo}@{path}"),
                _ => String::new(),
            };
            match knowledge.file_areas.get(file.as_str()) {
                Some(area) => by_area
                    .entry(area_order(area))
                    .or_insert_with(|| (*area, Vec::new()))
                    .1
                    .push(machine),
                None => loose.push(*machine),
            }
        }
        (by_area.into_values().collect(), loose)
    }

    /// The rules of one area's own code (#2066), under the area's heading:
    /// its rejections, at most [`MAX_AREA_OWN_ROWS`], then its changes.
    #[allow(clippy::too_many_arguments)]
    fn own_code(
        &self,
        owners: &[&'a str],
        area: &'a Node,
        headings: &mut crate::workbench::document::AreaHeadings<'a>,
        assertions: &mut Assertions,
        asserted: &mut BTreeSet<String>,
        out: &mut Text,
    ) -> Result<usize, RenderLimit> {
        let marks = headings.before(Some(area), out)?;
        writeln!(
            out,
            "{marks} Rules in the {} area's own code\n",
            Literal(area_name(area))
        )?;
        let mut followed = BTreeSet::new();
        let mut explained = BTreeSet::new();
        let mut shown = 0usize;
        if self.each(owners.iter().copied(), |_| Ok(()))? > 0 {
            out.push_str(HEADER)?;
            let mut omitted = 0usize;
            self.each(owners.iter().copied(), |row| {
                if shown == MAX_AREA_OWN_ROWS {
                    omitted += 1;
                    return Ok(());
                }
                shown += 1;
                write_row(&row, assertions, asserted, out)?;
                self.explain_constants(&row, &mut explained, assertions, asserted, out)?;
                self.follow(
                    &row,
                    &mut (&mut followed, &mut explained),
                    assertions,
                    asserted,
                    out,
                )
            })?;
            if omitted > 0 {
                writeln!(
                    out,
                    "| — | {omitted} more rejections in this area's code are not listed; \
                     `rule-evidence.md` has them. | — | — |"
                )?;
            }
            out.push_str("\n")?;
        }
        Ok(shown + self.changes(owners, &mut explained, assertions, asserted, out)?)
    }
}

impl<'a> Index<'a> {
    /// The “Changes” table of `owners`: their guarded effects (#1998,
    /// AC-0910), at most [`MAX_EFFECT_ROWS`], with a configuration constant
    /// a row names stated once per `explained` set. Prints nothing when
    /// there is none; returns the rows printed.
    fn changes(
        &self,
        owners: &[&'a str],
        explained: &mut BTreeSet<String>,
        assertions: &mut Assertions,
        asserted: &mut BTreeSet<String>,
        out: &mut Text,
    ) -> Result<usize, RenderLimit> {
        if self.each_picked(owners.iter().copied(), effect, |_| Ok(()))? == 0 {
            return Ok(0);
        }
        out.push_str("Changes:\n\n")?;
        out.push_str(HEADER)?;
        let mut shown = 0usize;
        let mut omitted = 0usize;
        self.each_picked(owners.iter().copied(), effect, |row| {
            if shown == MAX_EFFECT_ROWS {
                omitted += 1;
                return Ok(());
            }
            shown += 1;
            write_row(&row, assertions, asserted, out)?;
            self.explain_constants(&row, explained, assertions, asserted, out)
        })?;
        if omitted > 0 {
            writeln!(
                out,
                "| — | {omitted} more state changes and calls are not listed; \
                 `rule-evidence.md` has them. | — | — |"
            )?;
        }
        out.push_str("\n")?;
        Ok(shown)
    }
}

/// Call hops followed from a guard and the functions nested in it.
pub(super) const GUARD_DEPTH: usize = 2;
/// Rows printed for one guard.
pub(super) const MAX_GUARD_ROWS: usize = 12;

impl<'a> Index<'a> {
    /// The governed callables of `guard`: the guard, the functions nested
    /// in it (`guard.decorated`) and what they call within [`GUARD_DEPTH`]
    /// hops, nearest first.
    fn guard_owners(&self, guard: &'a str) -> Vec<&'a str> {
        let nested = format!("{guard}.");
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut front: Vec<&str> = std::iter::once(guard)
            .chain(
                self.by_id
                    .range::<str, _>((std::ops::Bound::Excluded(guard), std::ops::Bound::Unbounded))
                    .map(|(id, _)| *id)
                    .take_while(|id| id.starts_with(guard))
                    .filter(|id| id.starts_with(nested.as_str())),
            )
            .filter(|id| self.by_id.get(id).is_some_and(|n| n.label == "Symbol"))
            .filter(|id| seen.insert(id))
            .collect();
        let mut order = front.clone();
        for _ in 0..GUARD_DEPTH {
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

    /// Each guarded exit of `owners`, rejection or not, one at a time.
    fn each_exit(
        &self,
        owners: &[&'a str],
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
                if let Some(found) = guarded(rule, name, true, Some(&|k| self.constant(k))) {
                    n += 1;
                    row(found)?;
                }
            }
        }
        Ok(n)
    }
}

/// D1 §1.2: what each repository guard of the callers frame checks (#1998,
/// AC-0901). For every guard Symbol in `guards`, the guarded exits of the
/// guard, the functions nested in it and what they call within
/// [`GUARD_DEPTH`] hops are listed under the guard's name, at most
/// [`MAX_GUARD_ROWS`] per guard, and a predicate a condition calls is
/// followed by name as in §1.5. A guard with no guarded exit is not listed.
/// Returns the number of exit rows printed.
pub(super) fn guard_checks(
    (nodes, edges): (&[&Node], &[&Edge]),
    guards: &BTreeSet<String>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    if guards.is_empty() {
        return Ok(0);
    }
    let index = Index::new(nodes, edges);
    let listed: Vec<(&str, Vec<&str>)> = guards
        .iter()
        .filter_map(|g| index.by_id.get_key_value(g.as_str()))
        .filter(|(_, n)| n.label == "Symbol")
        .map(|(id, _)| (*id, index.guard_owners(id)))
        .filter(|(_, owners)| !owners.is_empty())
        .collect();
    let mut total = 0;
    for (_, owners) in &listed {
        total += index.each_exit(owners, |_| Ok(()))?;
    }
    if total == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "\n### What each guard checks\n\nGuarded exits of the repository guards above: \
         {total}. Each is a stored `return` or `raise` under a condition, or an assignment \
         through an attribute or a call statement with the conditions around it (`always` \
         when there are none; an assignment to `g.user` states whom the request acts as, \
         #1998), in the guard, a function nested in it, or what they call within \
         {GUARD_DEPTH} call hops (Deterministic observations). An exit returning an error \
         response or raising refuses the caller; the conditions are the ones around the exit in its own callable, and the \
         complete execution predicate is not established. A function a condition calls is \
         followed by name, and a configuration constant a row names is stated with its \
         recorded default (`↳` rows, inferred). At most {MAX_GUARD_ROWS} exits are listed per \
         guard; `rule-evidence.md` has every observation. {RULE_COLUMNS}\n"
    )?;
    let mut printed = 0;
    for (guard, owners) in &listed {
        if index.each_exit(owners, |_| Ok(()))? == 0 {
            continue;
        }
        let name = index
            .by_id
            .get(guard)
            .and_then(|n| n.props["name"].as_str())
            .unwrap_or(guard);
        writeln!(out, "#### {}\n", Literal(name))?;
        out.push_str(HEADER)?;
        // A function the guard's own rows already list is not followed again.
        let mut followed: BTreeSet<String> = owners
            .iter()
            .filter_map(|id| index.by_id.get(id)?.props["name"].as_str())
            .map(|name| name.rsplit('.').next().unwrap_or(name).to_owned())
            .collect();
        let mut explained = BTreeSet::new();
        let mut shown = 0usize;
        let mut omitted = 0usize;
        index.each_exit(owners, |row| {
            if shown == MAX_GUARD_ROWS {
                omitted += 1;
                return Ok(());
            }
            shown += 1;
            write_row(&row, assertions, asserted, out)?;
            index.explain_constants(&row, &mut explained, assertions, asserted, out)?;
            index.follow(
                &row,
                &mut (&mut followed, &mut explained),
                assertions,
                asserted,
                out,
            )
        })?;
        if omitted > 0 {
            writeln!(
                out,
                "| {} | {omitted} more guarded exits not listed; `rule-evidence.md` has them \
                 | — | — |",
                Literal(name)
            )?;
        }
        printed += shown;
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
        // Configuration constants by name and one table's explained set.
        map_slots::<&str, Vec<(&Node, &serde_json::Value)>>(rows)?,
        set_slots::<String>(rows)?,
        slots::<&str>(rows)?,
        set_slots::<&str>(rows)?,
        slots::<&str>(rows)?.checked_mul(3)?,
        set_slots::<String>(rows)?,
        // The endpoints each handler handles, by handler id (#2009).
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
    // plus a four-byte `not ` or ` and ` per value), then its citation id
    // and the place it names (#2009).
    let text = graph.bytes.checked_add(graph.values.checked_mul(5)?)?;
    let row = sum(&[
        json(graph.bytes, graph.values)?.checked_mul(2)?,
        slots::<&str>(graph.bytes)?,
        slots::<String>(graph.values)?,
        text.checked_mul(2)?,
        graph.bytes,
        // `place`: the handled routes' names, sorted, and the place itself.
        slots::<String>(rows)?,
        graph.bytes.checked_mul(2)?.checked_add(64)?,
    ])?;
    // What each guard checks (#1998): the guard ids, each guard's owners
    // and one guard's traversal and nested-name prefix, held with the index.
    let guards = sum(&[
        set_slots::<String>(rows)?,
        graph.bytes,
        slots::<(&str, Vec<&str>)>(rows)?,
        slots::<&str>(rows)?.checked_mul(3)?,
        set_slots::<&str>(rows)?,
        graph.bytes,
    ])?;
    // Each area's own code (#2066): the callables its capabilities reach,
    // by area, the area-ordered owners, and the arranged units.
    let own_code = sum(&[
        // The member files by area (then their callables), one area's
        // reached set at a time.
        map_slots::<(&str, &str), (&Node, Vec<&str>)>(rows)?,
        slots::<&str>(rows)?,
        set_slots::<&str>(rows)?,
        slots::<&str>(rows)?,
        slots::<(Option<&Node>, Option<&Story<'_>>)>(rows)?.checked_mul(2)?,
        // State machines (#2067): the index's list, by area and loose.
        slots::<&Node>(rows)?.checked_mul(3)?,
        map_slots::<(&str, &str), (&Node, Vec<&Node>)>(rows)?,
    ])?;
    sum(&[index, reach_count, traversal, row, guards, own_code])
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
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
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
            text.contains("when (qty &lt; 0): throws new NegativeQuantityError() |"),
            "{text}"
        );
        assert!(
            text.contains("when (qty &gt; 10): returns new OrderLimitError({ max: 10 }) |"),
            "{text}"
        );
        // A parenthesized construction is one; a construction or-ed with
        // another value is not.
        assert!(
            text.contains("when (qty &gt; 20): returns (new OrderLimitError({ max: 20 })) |"),
            "{text}"
        );
        assert!(!text.contains("|| qty"), "{text}");
        // A delimiter inside a string literal does not end the arguments.
        assert!(
            text.contains("when (qty &gt; 40): returns (new OrderLimitError("),
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
        rows(&facts.nodes, &facts.edges, handler_name)
    }

    /// The §1.5 rows of one capability whose endpoint `handler_name`
    /// handles, over adapter facts.
    fn rows(nodes: &[Node], edges: &[Edge], handler_name: &str) -> (String, usize) {
        // Rendered from a compacted base, as an export renders (#2215).
        let compacted = crate::rules::compacted_for_tests(nodes);
        let nodes: Vec<&Node> = compacted.iter().collect();
        let edges: Vec<&Edge> = edges.iter().collect();
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
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
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
                "when not alias or alias.user\\_id \\!= user.id: returns jsonify(error=\"Forbidden, really\"), 403 |"
            ),
            "{text}"
        );
        assert!(
            text.contains("when alias.disabled: throws AliasDisabled(\"disabled\") |"),
            "{text}"
        );
        assert!(text.contains("returns (\"pending\", 409,)"), "{text}");
        assert!(!text.contains("200"), "{text}");
        assert!(!text.contains("returns None"), "{text}");
    }

    // AC-0923 (#2009): a rule in an inline route handler says where it is
    // by the route the handler serves, not by the literal's offset.
    #[test]
    fn an_inline_handlers_rules_are_placed_by_its_route() {
        let source = r#"package device

import "net/http"

func routes(mux *http.ServeMux) {
	mux.HandleFunc("POST /sign-in", func(w http.ResponseWriter, r *http.Request) {
		if r.FormValue("email") == "" {
			http.Error(w, "email required", http.StatusBadRequest)
			return
		}
	})
	go func() {
		if ready() {
			http.Error(nil, "late", http.StatusConflict)
			return
		}
	}()
}

func ready() bool { return true }
"#;
        let facts = adapters_lang_go::extract_source(
            source.as_bytes(),
            "pkg/device/routes.go",
            &adapters_lang_go::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let literals: Vec<&str> = facts
            .nodes
            .iter()
            .filter_map(|n| n.props["name"].as_str())
            .filter(|n| n.starts_with("routes/func@"))
            .collect();
        assert_eq!(literals.len(), 1, "{literals:?}");
        let (text, printed) = rows(&facts.nodes, &facts.edges, literals[0]);
        assert_eq!(printed, 1, "{text}");
        assert!(text.contains("| handler of POST /sign-in | "), "{text}");
        assert!(!text.contains("func@"), "{text}");
    }

    // AC-0923 (#2009): the Go response and error shapes are read in Go
    // source only; a Python call taking 500 or a returned `ErrNotFound`
    // is not a refusal.
    #[test]
    fn go_shapes_are_not_read_in_other_languages() {
        let source = r#"
def update_alias(alias):
    if alias.busy:
        backoff(500)
    if alias.missing:
        return ErrNotFound
    return alias
"#;
        let (text, printed) = python_rows(source, "update_alias");
        // The call stays a change, and neither is a rejection.
        assert_eq!(printed, 1, "{text}");
        assert!(text.contains("calls backoff(500)"), "{text}");
        assert!(!text.contains("refused"), "{text}");
        assert!(!text.contains("ErrNotFound"), "{text}");
    }

    // AC-0923 (#2009): a Go handler's refusal (`http.Error` before a bare
    // return) and a guarded Go error return are its rejections, stated in
    // domain terms; they are not listed as changes.
    #[test]
    fn go_refusals_and_error_returns_are_rejections() {
        let source = r#"package device

import (
	"errors"
	"net/http"
)

var ErrNotFound = errors.New("not found")

func signIn(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	if r.FormValue("email") == "" {
		w.WriteHeader(http.StatusBadRequest)
		return
	}
	if err := lookup(r.FormValue("email")); err != nil {
		return
	}
	w.WriteHeader(http.StatusOK)
}

func lookup(id string) error {
	if id == "" {
		return ErrNotFound
	}
	if id == "admin" {
		return errors.New("reserved")
	}
	return nil
}
"#;
        let facts = adapters_lang_go::extract_source(
            source.as_bytes(),
            "pkg/device/web.go",
            &adapters_lang_go::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let (text, printed) = rows(&facts.nodes, &facts.edges, "signIn");
        assert_eq!(printed, 4, "{text}");
        for expected in [
            "the request is refused with status 405 (“method not allowed”)",
            "When the email is empty, the request is refused with status 400.",
            "When the request's method is not “POST”, the request is refused",
            "it fails with the error “not found”",
            "it fails with the error “reserved”",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains("calls http.Error"), "{text}");
        assert!(!text.contains("200"), "{text}");
        assert!(!text.contains("“form value”"), "{text}");
    }

    // AC-0911 (T-0911): the guarded effects of the callables a capability
    // reaches are its "Changes" rows, after its rejections, each restated in
    // domain terms with the stored statement as Source; a capability with
    // changes and no rejection is still listed.
    #[test]
    fn what_a_capability_changes_is_listed_under_it() {
        let source = r#"
from flask import jsonify

def delete_alias_view(alias_id, user):
    alias = Alias.get(alias_id)
    if not alias:
        return jsonify(error="Forbidden"), 403
    delete_alias(alias, user)
    return jsonify(deleted=True), 200

def delete_alias(alias, user):
    if alias.delete_on is not None:
        perform_alias_deletion(alias, user)
    else:
        move_alias_to_trash(alias, user)

def move_alias_to_trash(alias, user):
    alias.delete_on = arrow.now().shift(days=+ALIAS_TRASH_DAYS)
    alias.enabled = False

def perform_alias_deletion(alias, user):
    alias.note = compute(alias) + 1
"#;
        let (text, printed) = python_rows(source, "delete_alias_view");
        assert!(
            text.contains("State changes and calls per capability: 6"),
            "{text}"
        );
        let rejections = text
            .find("returns jsonify(error=\"Forbidden\"), 403")
            .unwrap();
        let changes = text.find("Changes:").unwrap();
        assert!(rejections < changes, "{text}");
        for line in [
            "| delete\\_alias | When the alias's delete on is set, it runs “perform alias deletion”. | \
             when alias.delete\\_on is not None: calls perform\\_alias\\_deletion(alias, user) |",
            "| move\\_alias\\_to\\_trash | The alias is no longer enabled. | always: sets alias.enabled to False |",
            "| move\\_alias\\_to\\_trash | The alias's delete on is set to the configured alias trash days from now. |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        // A value outside the rendering rules is a Gap, with its source kept.
        assert!(
            text.contains(&format!(
                "{NOT_STATED} | always: sets alias.note to compute(alias) + 1 |"
            )),
            "{text}"
        );
        assert_eq!(printed, 1 + 6, "{text}");
    }

    // AC-0916: a mail handler's guarded returns of module reply constants
    // are its decisions, stated with the reply text, under the capability
    // whose entry point enters the handler's file; a returned local bound to
    // reply constants names each reply it may hold.
    #[test]
    fn a_mail_handlers_replies_are_its_decisions() {
        let id = adapters_lang_python::SourceId {
            repo: "repo",
            commit: "2cc309deb5394af176314552ac7320a7b26a0104",
        };
        let status = adapters_lang_python::extract_source(
            br#"
E200 = "250 Message accepted for delivery"
E502 = "550 SL E502 Email not exist"
E407 = "421 SL E407 Retry later"
E354 = "354 Start mail input"
"#,
            "app/email/status.py",
            &id,
        )
        .unwrap();
        let handler = adapters_lang_python::extract_source(
            br#"
from app.email import status

def handle_forward(alias, contact, user):
    if not alias.enabled or contact.block_forward:
        res_status = status.E200
        if user.reject_blocked:
            res_status = status.E502
        return [(True, res_status)]
    if alias.over_quota:
        return [(False, status.E407)]
    if not user.active:
        return [(False, status.E502)]
    if alias.streaming:
        return [(True, status.E354)]
    return [(True, status.E200)]

def handle_reply(contact):
    reply = status.E502
    if contact.blocked:
        return [(False, reply)]
    reply = status.E200
    return [(True, reply)]
"#,
            "email_handler.py",
            &id,
        )
        .unwrap();
        let entry = Node {
            id: "entry:repo@email_handler.py#main".into(),
            label: "EntryPoint".into(),
            props: serde_json::json!({ "kind": "smtp-listener" }),
        };
        let enters = Edge {
            src: entry.id.clone(),
            dst: "file:repo@email_handler.py".into(),
            label: "ENTRY".into(),
            props: serde_json::json!({}),
        };
        let nodes: Vec<&Node> = status
            .nodes
            .iter()
            .chain(&handler.nodes)
            .chain([&entry])
            .collect();
        let edges: Vec<&Edge> = status
            .edges
            .iter()
            .chain(&handler.edges)
            .chain([&enters])
            .collect();
        let cite = BaseRef::Node(entry.id.clone());
        let cap = Node {
            id: "capability:test:mail".into(),
            label: "Capability".into(),
            props: serde_json::json!({ "title": "Receive mail over SMTP" }),
        };
        let knowledge = Stories {
            included: vec![crate::workbench::capabilities::Story {
                node: &cap,
                support: vec![(&cite, Support::Node(&entry))],
            }],
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
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
        let text = out.finish();
        for line in [
            "| handle\\_forward | When the alias is not enabled or the contact's block forward is set, \
             the mail is answered “250 Message accepted for delivery” or “550 SL E502 Email not exist”, \
             as the function sets it. |",
            "| handle\\_forward | When the alias's over quota is set, the mail is deferred with the reply \
             “421 SL E407 Retry later”. |",
            "| handle\\_forward | When the user is not active, the mail is refused with the reply \
             “550 SL E502 Email not exist”. |",
            // Only the binding before the return is what it answers.
            "| handle\\_reply | When the contact is blocked, the mail is refused with the reply \
             “550 SL E502 Email not exist”. |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        // The unguarded success answer and a 3xx step are not decisions.
        assert!(!text.contains("354"), "{text}");
        assert_eq!(printed, 4, "{text}");
    }

    // AC-1030 (T-1030): a check the receiver runs, a sum compared with a
    // method's value, a flag bit, membership in a method's value and the
    // last answer of a boolean method without a predicate prefix are stated
    // in domain terms, not as Gap rows.
    #[test]
    fn receiver_checks_sums_flags_and_last_answers_are_stated() {
        let source = r#"
from flask import jsonify

class User:
    def lifetime_or_active_subscription(self):
        if self.lifetime:
            return True
        return self.get_active_subscription() is not None

    def max_alias_for_free_account(self):
        if self.FLAG_FREE_OLD_ALIAS_LIMIT == self.flags & self.FLAG_FREE_OLD_ALIAS_LIMIT:
            return config.MAX_NB_EMAIL_OLD_FREE_PLAN
        else:
            return config.MAX_NB_EMAIL_FREE_PLAN

    def can_create_num_aliases(self, num_aliases):
        if self.disabled:
            return False
        if self.lifetime_or_active_subscription():
            return True
        else:
            active_alias_count = 3
            return (active_alias_count + num_aliases) <= self.max_alias_for_free_account()

    def can_create_new_alias(self):
        return self.can_create_num_aliases(1)

def verify_prefix_suffix(user, alias_prefix, alias_suffix):
    if alias_domain not in user.available_alias_domains(alias_options=alias_options):
        return False
    return True

def found_alias(missing, result):
    if missing:
        return False
    return result

def update_alias(alias_id, user):
    if not user.can_create_new_alias():
        return jsonify(error="quota"), 400
    if not user.can_create_num_aliases(2):
        return jsonify(error="quota"), 400
    if not found_alias(alias_id, user):
        return jsonify(error="no"), 400
    if not verify_prefix_suffix(user, 1, 2):
        return jsonify(error="no"), 400
    return jsonify(ok=True)
"#;
        let (text, _) = python_rows(source, "update_alias");
        assert!(!text.contains(NOT_STATED), "{text}");
        for line in [
            "| ↳ User.can\\_create\\_new\\_alias | A user can create new alias when the user can create num aliases (1). |",
            "| ↳ User.can\\_create\\_num\\_aliases | A user can create num aliases when the check “lifetime or active subscription” holds for the user. |",
            "| ↳ User.can\\_create\\_num\\_aliases | A user can create num aliases when the check “lifetime or active subscription” does not hold for the user, and the active alias count plus the num aliases is at most the user's max alias for free account. |",
            "| ↳ User.lifetime\\_or\\_active\\_subscription | Otherwise, the user's lifetime or active subscription is true when the user's active subscription is set. |",
            "| ↳ User.max\\_alias\\_for\\_free\\_account | When the user's flags include the “free old alias limit” flag, the user's max alias for free account is the configured max nb email old free plan. |",
            "| ↳ User.max\\_alias\\_for\\_free\\_account | When the user's flags do not include the “free old alias limit” flag, the user's max alias for free account is the configured max nb email free plan. |",
            "| ↳ verify\\_prefix\\_suffix | When the alias domain is not one of the user's available alias domains for the alias options, the verify prefix suffix is false. |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        // A named receiver's predicate keeps its number.
        assert!(
            text.contains("the user cannot create num aliases (2)"),
            "{text}"
        );
        // A free function's last answer after a guard is no unconditional
        // rule.
        assert!(!text.contains("returns the result"), "{text}");
        // A constant last answer with no exit before it is still no rule.
        assert!(
            !text.contains("verify\\_prefix\\_suffix | Otherwise"),
            "{text}"
        );
    }

    // AC-0886: a function a rejection's condition calls is followed by name
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
                "| ↳ | The check “can create new alias” decides as follows (matched by name, inferred, 2 call hops): | can\\_create\\_new\\_alias() in User.can\\_create\\_new\\_alias | — |"
            ),
            "{text}"
        );
        assert!(
            text.contains("when self.disabled: returns False |"),
            "{text}"
        );
        assert!(text.contains("when self.premium: returns True |"), "{text}");
        assert!(
            text.contains(
                "| ↳ | The check “outside check” is not followed: no repository function of this name was recovered. | outside\\_check() | — |"
            ),
            "{text}"
        );
    }

    // AC-1034 (T-1034), AC-1036 (T-1036): a condition's local is stated
    // with what it was assigned, a header read by its name and a lookup by
    // its keys; a local assigned a function's result is followed into that
    // function, whose `try` / `except` returns read as raised / not raised
    // errors, keeping the call's keyword inputs.
    #[test]
    fn a_condition_local_is_stated_with_its_assignment_and_followed() {
        let source = r#"
import itsdangerous
from flask import jsonify, request, g

def check_suffix_signature(signed_suffix):
    try:
        return signer.unsign(signed_suffix, max_age=600).decode()
    except itsdangerous.BadSignature:
        return None

def authorize_request():
    api_code = request.headers.get("Authentication")
    api_key = ApiKey.get_by(code=api_code)
    if not api_key:
        return jsonify(error="Wrong api key"), 401
    g.user = api_key.user

def get_alias(alias_id):
    alias = Alias.get(alias_id)
    if not alias:
        return jsonify(error="Unknown error"), 400
    entry = cache.get(alias_id)
    if not entry:
        return jsonify(error="Gone"), 410

def pick(signed_suffix):
    picked = check_suffix_signature(signed_suffix) if signed_suffix else None
    if not picked:
        return jsonify(error="No suffix"), 409

def new_custom_alias(signed_suffix):
    try:
        alias_suffix = check_suffix_signature(signed_suffix)
        if not alias_suffix:
            return jsonify(error="Alias creation time is expired, please retry"), 412
    except Exception:
        return jsonify(error="Tampered suffix"), 400
"#;
        let (text, _) = python_rows(source, "authorize_request");
        assert!(
            text.contains(
                "When no API key is given, the request is refused with status 401 (“Wrong api key”). Here the API key is the API key whose code is the “Authentication” header."
            ),
            "{text}"
        );
        let (text, _) = python_rows(source, "get_alias");
        assert!(
            text.contains(
                "When no alias is given, the request is refused with status 400 (“Unknown error”). Here the alias is the alias found by the alias ID. |"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "When no entry is given, the request is refused with status 410 (“Gone”). |"
            ),
            "{text}"
        );
        // An initializer that is not one call is not followed.
        let (text, _) = python_rows(source, "pick");
        assert!(text.contains("409"), "{text}");
        assert!(!text.contains("check suffix signature"), "{text}");
        let (text, _) = python_rows(source, "new_custom_alias");
        assert!(
            text.contains(
                "| ↳ | The check “check suffix signature” decides as follows (matched by name, inferred, 2 call hops): |"
            ),
            "{text}"
        );
        assert!(
            text.contains("when raised itsdangerous.BadSignature: returns None |"),
            "{text}"
        );
        assert!(
            text.contains("When a bad signature error is raised"),
            "{text}"
        );
        assert!(
            text.contains(
                "When no bad signature error is raised, the check suffix signature is the signer's unsign for the signed suffix and max age 600."
            ),
            "{text}"
        );
        assert!(text.contains("When an error is raised, the request is refused with status 400 (“Tampered suffix”)."), "{text}");
    }

    // AC-1040 (T-1040): a local assigned more than once is stated with the
    // assignment that reaches the condition, and each local that value
    // reads is stated in a sentence of its own: the website's suggested
    // alias is its domain name plus the first alias suffix.
    #[test]
    fn a_reassigned_local_is_stated_with_the_value_that_reaches_it() {
        let source = r#"
from flask import jsonify, request

def new_random_alias(user):
    alias = None
    hostname = request.args.get("hostname")
    if hostname:
        ext = tldextract.extract(hostname)
        prefix_suggestion = ext.domain
        prefix_suggestion = convert_to_id(prefix_suggestion)
        suffixes = get_alias_suffixes(user)
        suggested_alias = prefix_suggestion + suffixes[0].suffix
        alias = Alias.get_by(email=suggested_alias)
        if alias and not alias.user_id == user.id:
            return jsonify(error="Taken"), 409

def twice(user):
    plan = load_plan(user)
    if plan:
        plan = upgrade(plan)
        if plan:
            return jsonify(error="Upgraded"), 409
"#;
        // Form and JSON body fields are named as such, `len(x)` is a count,
        // and a plural subject agrees (AC-1040).
        let wording = r#"
from flask import jsonify, request

def redeem(coupon_form, to_addrs):
    code = coupon_form.code.data
    if not code:
        return jsonify(error="No code"), 400
    data = request.get_json()
    name = data.get("name")
    if not name:
        return jsonify(error="No name"), 400
    recipients = len(to_addrs)
    if recipients > 10:
        return jsonify(error="Too many"), 400
    parts = name.split("@")
    if not parts:
        return jsonify(error="No parts"), 400
    args = parser.parse_args()
    if not args:
        return jsonify(error="No args"), 400
"#;
        let (text, _) = python_rows(wording, "redeem");
        for said in [
            "Here the code is the “code” form field.",
            "Here the name is the “name” field of the JSON body.",
            "Here the recipients are the number of to addrs.",
            "Here the parts are the name split at “@”.",
        ] {
            assert!(text.contains(said), "{said}\n{text}");
        }
        // Another method's result is not said.
        assert!(!text.contains("Here the args"), "{text}");
        // Two values of `plan` reach one row's conditions: neither is said.
        let (text, _) = python_rows(source, "twice");
        assert!(text.contains("(“Upgraded”). |"), "{text}");
        let (text, _) = python_rows(source, "new_random_alias");
        assert!(
            text.contains(
                "Here the alias is the alias whose email is the suggested alias. Here the suggested alias is the prefix suggestion plus the suffix of the first of the suffixes. Here the prefix suggestion is the “convert to ID” result for the ext's domain. Here the suffixes are the “get alias suffixes” result for the user. |"
            ),
            "{text}"
        );
        // The functions those values call are stated, not followed.
        assert!(!text.contains("“convert to ID” is not followed"), "{text}");
    }

    // AC-0915 (T-0915): a check several functions share a name with is
    // followed to the one on the receiver's class (`g.user` the User
    // model, a variable by its CamelCase name); with no such class it is
    // still named as not followed.
    #[test]
    fn a_shared_check_name_is_followed_on_the_receivers_class() {
        let source = r#"
from flask import jsonify, g

class User:
    def is_active(self):
        return self.delete_on is None

class Mailbox:
    def is_active(self):
        if self.disabled:
            return False
        return True

class Job:
    def is_active(self):
        return True

def update_alias(alias_id, mailbox, thing):
    if not g.user.is_active():
        return jsonify(error="Account does not exist"), 401
    if not mailbox.is_active():
        return jsonify(error="mailbox"), 400
    if not thing.is_active():
        return jsonify(error="thing"), 400
    return jsonify(ok=True)
"#;
        let (text, _) = python_rows(source, "update_alias");
        assert!(
            text.contains(
                "| ↳ | The check “is active” decides as follows (matched by name on the receiver's class User, inferred, 2 call hops): | is\\_active() in User.is\\_active | — |"
            ),
            "{text}"
        );
        // A predicate's unguarded answer is stated as its rule.
        assert!(
            text.contains(
                "| ↳ User.is\\_active | A user is active when the user's delete on is not set. | always: returns self.delete\\_on is None |"
            ),
            "{text}"
        );
        // Another receiver's class is followed too; a receiver no class
        // names keeps the not-followed row.
        assert!(
            text.contains("is\\_active() in Mailbox.is\\_active"),
            "{text}"
        );
        assert!(
            text.contains(
                "The check “is active” is not followed: 3 repository functions have this name."
            ),
            "{text}"
        );
        assert_eq!(receiver_class("mailbox", "f"), Some("Mailbox".to_owned()));
        assert_eq!(receiver_class("api_key", "f"), Some("ApiKey".to_owned()));
        assert_eq!(
            receiver_class("self", "User.can_x"),
            Some("User".to_owned())
        );
        assert_eq!(
            receiver("not g.user.is_active()", "is_active"),
            Some("g.user")
        );
        assert_eq!(receiver("is_active()", "is_active"), None);
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
                "when not (not user or not user.check\\_password(password)) and not user.activated: throws"
            ),
            "{text}"
        );
    }

    // AC-0901 (T-0901): a repository guard's own exits, those of the
    // function nested in it and of what that calls are listed under the
    // guard, with a predicate followed by name; a guard id that is not a
    // recovered Symbol, or a guard with no guarded exit, is not listed.
    #[test]
    fn each_guard_lists_the_exits_it_and_its_wrapper_reach() {
        let source = r#"
from functools import wraps
from flask import jsonify, g, request

def authorize_request():
    api_key = request.headers.get("Authentication")
    if not api_key:
        return jsonify(error="Wrong api key"), 401
    if g.user.disabled:
        return jsonify(error="Disabled account"), 403
    if not g.user.is_active():
        return jsonify(error="Account does not exist"), 401
    return None

class User:
    def is_active(self):
        if self.delete_on is not None:
            return False
        return True

def require_api_auth(f):
    @wraps(f)
    def decorated(*args, **kwargs):
        error_return = authorize_request()
        if error_return:
            return error_return
        return f(*args, **kwargs)
    return decorated

def plain(f):
    return f
"#;
        let facts = adapters_lang_python::extract_source(
            source.as_bytes(),
            "app/api/base.py",
            &adapters_lang_python::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let id_of = |name: &str| {
            nodes
                .iter()
                .find(|n| n.label == "Symbol" && n.props["name"] == name)
                .unwrap()
                .id
                .clone()
        };
        let guards: BTreeSet<String> = [
            id_of("require_api_auth"),
            id_of("plain"),
            "sym:repo@app/api/base.py#missing".to_owned(),
        ]
        .into();
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        let printed = guard_checks(
            (&nodes, &edges),
            &guards,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let text = out.finish();
        assert_eq!(printed, 4, "{text}");
        assert!(text.contains("### What each guard checks"), "{text}");
        assert!(text.contains("#### require\\_api\\_auth\n"), "{text}");
        assert!(!text.contains("#### plain"), "{text}");
        assert!(!text.contains("missing"), "{text}");
        for row in [
            "when error\\_return: returns error\\_return |",
            "when not api\\_key: returns jsonify(error=\"Wrong api key\"), 401 |",
            "when g.user.disabled: returns jsonify(error=\"Disabled account\"), 403 |",
            "when not g.user.is\\_active(): returns jsonify(error=\"Account does not exist\"), 401 |",
            "| ↳ | The check “is active” decides as follows (matched by name, inferred, 2 call hops): | is\\_active() in User.is\\_active | — |",
            // AC-0905: the rules read as statements.
            "| authorize\\_request | When the user is disabled, the request is refused with status 403 (“Disabled account”). |",
            "| ↳ User.is\\_active | A user is not active when the user's delete on is set. |",
            "when self.delete\\_on is not None: returns False |",
        ] {
            assert!(text.contains(row), "missing {row}\n{text}");
        }
    }

    // AC-0904 (T-0904): a configuration constant a row or a followed row
    // names is stated once per table with its recorded default and
    // environment variable; a name two reads bind is not stated.
    #[test]
    fn a_configuration_constant_a_row_names_is_stated_with_its_default() {
        let source = r#"
from flask import jsonify
import config

class User:
    def max_alias(self):
        if self.old:
            return config.MAX_OLD
        return config.MAX_FREE

def create(user):
    if user.count() >= user.max_alias():
        return jsonify(error="quota"), 400
    if user.count() > config.MAX_FREE:
        return jsonify(error="over"), 400
    if user.trash > config.TWICE:
        return jsonify(error="x"), 400
"#;
        let mut facts = adapters_lang_python::extract_source(
            source.as_bytes(),
            "app/api/alias.py",
            &adapters_lang_python::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let read = |key: &str, path: &str, value: serde_json::Value| Node {
            id: format!("cfgread:repo@{path}:env:{key}"),
            label: "ConfigRead".into(),
            props: serde_json::json!({
                "key": key,
                "source": "env",
                "defaults": [{ "constant": key, "value": value, "byte_start": 0, "byte_end": 1 }],
            }),
        };
        facts
            .nodes
            .push(read("MAX_FREE", "app/config.py", 5.into()));
        facts
            .nodes
            .push(read("MAX_OLD", "app/config.py", 15.into()));
        facts.nodes.push(read("TWICE", "app/config.py", 1.into()));
        facts.nodes.push(read("TWICE", "app/other.py", 2.into()));
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let handler = nodes
            .iter()
            .find(|n| n.label == "Symbol" && n.props["name"] == "create")
            .unwrap();
        let handles = Edge {
            src: "ep:repo@POST:/api/alias".into(),
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
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        rules_per_capability(
            (&nodes, &edges),
            &knowledge,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let text = out.finish();
        for row in [
            "| ↳ | The configured max old is 15 unless the environment variable MAX\\_OLD is set (matched by name, inferred). | MAX\\_OLD | ",
            "| ↳ | The configured max free is 5 unless the environment variable MAX\\_FREE is set (matched by name, inferred). | MAX\\_FREE | ",
            // AC-0905: a value a method gives is stated.
            "| ↳ User.max\\_alias | When the user is old, the user's max alias is the configured max old. |",
            "| ↳ | The configured twice is not stated: 2 recorded reads bind this name | TWICE | — |",
        ] {
            assert!(text.contains(row), "missing {row}\n{text}");
        }
        assert_eq!(text.matches("| MAX\\_FREE |").count(), 1, "{text}");
    }

    // AC-0950: with areas, each area's own code that none of its
    // capabilities reach is stated after them: an area with no reaching
    // capability gets its rejections and changes; a rule a capability of
    // the area already lists is not repeated.
    #[test]
    fn an_areas_own_code_is_stated_after_its_capabilities() {
        let id = adapters_lang_python::SourceId {
            repo: "repo",
            commit: "2cc309deb5394af176314552ac7320a7b26a0104",
        };
        let alias = adapters_lang_python::extract_source(
            b"def update_alias(alias_id):\n    if not alias_id:\n        raise ValueError('no alias')\n    return 'ok'\n\n\
def rename_alias(alias, name):\n    if not name:\n        raise ValueError('no name')\n    alias.name = name\n",
            "app/api/alias.py",
            &id,
        )
        .unwrap();
        let abuser = adapters_lang_python::extract_source(
            b"def mark_user_as_abuser(user, note):\n    user.disabled = True\n\n\
def store_abuse_data(user):\n    if not user.email:\n        raise ValueError('no primary email')\n    return user\n",
            "app/abuser.py",
            &id,
        )
        .unwrap();
        let nodes: Vec<Node> = alias.nodes.iter().chain(&abuser.nodes).cloned().collect();
        let edges: Vec<Edge> = alias.edges.iter().chain(&abuser.edges).cloned().collect();
        let nodes: Vec<&Node> = nodes.iter().collect();
        let edges: Vec<&Edge> = edges.iter().collect();
        let file = |path: &str| {
            edges
                .iter()
                .find(|e| e.label == "DEFINED_IN" && e.dst.ends_with(path))
                .map(|e| e.dst.clone())
                .unwrap()
        };
        let (alias_file, abuser_file) = (file("app/api/alias.py"), file("app/abuser.py"));
        let handler = nodes
            .iter()
            .copied()
            .find(|n| n.label == "Symbol" && n.props["name"] == "update_alias")
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
        let area = |id: &str, name: &str| Node {
            id: id.into(),
            label: "Subsystem".into(),
            props: serde_json::json!({ "name": name }),
        };
        let (alias_area, abuser_area) =
            (area("subsystem:1", "Alias"), area("subsystem:2", "Abuser"));
        let knowledge = Stories {
            included: vec![crate::workbench::capabilities::Story {
                node: &cap,
                support: vec![(&cite, Support::Edge(&handles))],
            }],
            filtered: 0,
            areas: [(cap.id.as_str(), &alias_area)].into_iter().collect(),
            file_areas: [
                (alias_file.as_str(), &alias_area),
                (abuser_file.as_str(), &abuser_area),
            ]
            .into_iter()
            .collect(),
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
        let text = out.finish();
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}\n{text}"))
        };
        // Abuser sorts first; its own code is stated though no capability
        // reaches it, rejection and change alike.
        assert!(at("### Abuser\n") < at("#### Rules in the Abuser area's own code"));
        assert!(at("#### Rules in the Abuser area's own code") < at("| store\\_abuse\\_data |"));
        assert!(at("| store\\_abuse\\_data |") < at("### Alias\n"));
        assert!(
            at("sets user.disabled to True") < at("### Alias\n"),
            "{text}"
        );
        // Alias: the capability, then the function it does not reach.
        assert!(at("#### Manage alias (API)") < at("#### Rules in the Alias area's own code"));
        assert!(at("#### Rules in the Alias area's own code") < at("| rename\\_alias |"));
        assert_eq!(text.matches("| update\\_alias |").count(), 1, "{text}");
        assert!(text.contains("Rules in an area's own code (#2066)"));
        assert!(printed >= 4, "{printed}\n{text}");

        // Member areas with no Capability in one still head their own code,
        // so a briefing finds the area's block.
        let unplaced = Stories {
            included: vec![crate::workbench::capabilities::Story {
                node: &cap,
                support: vec![(&cite, Support::Edge(&handles))],
            }],
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: knowledge.file_areas.clone(),
        };
        let mut out = Text::new(&budget, "").unwrap();
        rules_per_capability(
            (&nodes, &edges),
            &unplaced,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let unplaced_text = out.finish();
        assert!(
            unplaced_text.contains("### Abuser\n\n#### Rules in the Abuser area's own code"),
            "{unplaced_text}"
        );

        // Without areas, nothing beyond the capabilities is stated.
        let plain = Stories {
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
            ..knowledge
        };
        let mut out = Text::new(&budget, "").unwrap();
        rules_per_capability(
            (&nodes, &edges),
            &plain,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        assert!(!out.finish().contains("own code"));
    }

    // AC-0951: a state machine is stated under the area of the file that
    // declares it, with each state and where it may move; the guard of its
    // `onTransitionStart` hook is a row of the area's own code; with no
    // areas the machine is listed under "State machines".
    #[test]
    fn a_state_machine_is_stated_under_its_area() {
        let source = "export function configureDefaultOrderProcess(options) {
    const orderProcess: OrderProcess<OrderState> = {
        transitions: {
            AddingItems: { to: ['ArrangingPayment', 'Cancelled'] },
            ArrangingPayment: { to: ['AddingItems', 'Cancelled'] },
            Cancelled: { to: [] },
        },
        async onTransitionStart(fromState, toState, { order }) {
            if (toState === 'ArrangingPayment' && order.lines.length === 0) {
                return 'message.cannot-transition-to-payment-when-order-is-empty';
            }
        },
    };
    return orderProcess;
}
";
        let facts = adapters_lang_ts::extract_source(
            source.as_bytes(),
            "src/config/order/default-order-process.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let order = Node {
            id: "subsystem:1".into(),
            label: "Subsystem".into(),
            props: serde_json::json!({ "name": "Order" }),
        };
        let file = "file:repo@src/config/order/default-order-process.ts";
        let render = |file_areas: BTreeMap<&str, &Node>| {
            let knowledge = Stories {
                included: Vec::new(),
                filtered: 0,
                areas: BTreeMap::new(),
                file_areas,
            };
            let budget = Budget::new(None).unwrap();
            let mut assertions = Assertions::new(&budget);
            let mut asserted = BTreeSet::new();
            let mut out = Text::new(&budget, "").unwrap();
            rules_per_capability(
                (&nodes, &edges),
                &knowledge,
                (&mut assertions, &mut asserted),
                &mut out,
            )
            .unwrap();
            (out.finish(), assertions.values.len())
        };
        let (text, asserted) = render([(file, &order)].into_iter().collect());
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}\n{text}"))
        };
        assert!(text.contains("State machines (#2067): 1."), "{text}");
        assert!(at("### Order\n") < at("#### State machine: configureDefaultOrderProcess"));
        assert!(
            at("#### State machine: configureDefaultOrderProcess")
                < at("Guards in `onTransitionStart`")
        );
        assert!(
            at("Guards in `onTransitionStart`")
                < at(
                    "| onTransitionStart | When the to state is “ArrangingPayment” and the lines's length is 0, the move is refused with a message the export withholds. |"
                )
        );
        assert!(text.contains(
            "when (toState === 'ArrangingPayment' &amp;&amp; order.lines.length === 0): returns"
        ));
        assert!(text.contains(
            "configureDefaultOrderProcess (`OrderProcess&lt;OrderState&gt;`), in `src/config/order/default-order-process.ts`, has 3 states: AddingItems, ArrangingPayment, Cancelled."
        ), "{text}");
        assert!(text.contains("| AddingItems | ArrangingPayment, Cancelled | F-"));
        assert!(text.contains("| Cancelled | nothing (final) | F-"));
        assert_eq!(asserted, 2, "the machine and its guard");

        let (plain, _) = render(BTreeMap::new());
        assert!(
            plain
                .contains("### State machines\n\n#### State machine: configureDefaultOrderProcess"),
            "{plain}"
        );
    }

    // AC-1032 (T-1032): a transition guard of a state machine reads as a
    // refused move, whatever message it returns (one the export withholds,
    // a plain one or `false`), over conditions with optional chaining and
    // camel-case checks.
    #[test]
    fn transition_guards_are_stated_as_refused_moves() {
        let source = "export function configureDefaultOrderProcess(options) {
    const orderProcess: OrderProcess<OrderState> = {
        transitions: {
            AddingItems: { to: ['ArrangingPayment', 'Cancelled'] },
            ArrangingPayment: { to: ['PaymentSettled', 'Cancelled'] },
            PaymentSettled: { to: ['Cancelled'] },
            Cancelled: { to: [] },
        },
        async onTransitionStart(fromState, toState, { order }) {
            if (toState === 'ArrangingPayment') {
                if (options.arrangingPaymentRequiresContents !== false && order.lines.length === 0) {
                    return 'message.cannot-transition-to-payment-when-order-is-empty';
                }
                if (options.arrangingPaymentRequiresShipping !== false && order.shippingLines?.length === 0) {
                    return 'no shipping';
                }
            }
            if (toState === 'PaymentSettled' && !orderTotalIsCovered(order, 'Settled')) {
                return false;
            }
            if (toState === 'Cancelled' && !hasAnAuthorizedPayment) {
                return ctx.translate('message.cannot-cancel');
            }
            if (toState === 'Shipped') {
                return true;
            }
            if (toState === 'Delivered') {
                throw new IllegalOperationError('message.cannot-deliver');
            }
        },
    };
    return orderProcess;
}
";
        let facts = adapters_lang_ts::extract_source(
            source.as_bytes(),
            "src/config/order/default-order-process.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let knowledge = Stories {
            included: Vec::new(),
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: BTreeMap::new(),
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        rules_per_capability(
            (&nodes, &edges),
            &knowledge,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let text = out.finish();
        for line in [
            "| onTransitionStart | When the to state is “ArrangingPayment”, and the options's arranging payment requires contents is not false and the lines's length is 0, the move is refused with a message the export withholds. |",
            "| onTransitionStart | When the to state is “ArrangingPayment”, and the options's arranging payment requires shipping is not false and the shipping lines's length is 0, the move is refused with the message “no shipping”. |",
            "| onTransitionStart | When the to state is “PaymentSettled” and the check “order total is covered” does not hold, the move is refused. |",
            "| onTransitionStart | When the to state is “Cancelled” and the check “has no authorized payment” holds, the move is refused. |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(!text.contains(NOT_STATED), "{text}");
        // A throw refuses the move too.
        assert!(
            text.contains(
                "| onTransitionStart | When the to state is “Delivered”, the move is refused with an error. |"
            ),
            "{text}"
        );
        // `true` lets the move happen.
        assert!(
            text.contains(
                "| onTransitionStart | When the to state is “Shipped”, the move is allowed. |"
            ),
            "{text}"
        );
    }

    // AC-0998: an area's state machines come first in its §1.5 block,
    // before its capabilities and its own code, so an area briefing cut from
    // the front keeps the lifecycle (#2069).
    #[test]
    fn an_areas_state_machines_come_before_its_own_code() {
        let source = "export const defaultPaymentProcess: PaymentProcess<PaymentState> = {
    transitions: {
        Authorized: { to: ['Settled', 'Declined'] },
        Settled: { to: [] },
        Declined: { to: [] },
    },
};
export function assertPaymentState(state) {
    if (!state) {
        throw new Error('no state');
    }
}
";
        let facts = adapters_lang_ts::extract_source(
            source.as_bytes(),
            "src/config/payment/default-payment-process.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "2cc309deb5394af176314552ac7320a7b26a0104",
            },
        )
        .unwrap();
        let nodes: Vec<&Node> = facts.nodes.iter().collect();
        let edges: Vec<&Edge> = facts.edges.iter().collect();
        let payment = Node {
            id: "subsystem:1".into(),
            label: "Subsystem".into(),
            props: serde_json::json!({ "name": "Payment" }),
        };
        let file = "file:repo@src/config/payment/default-payment-process.ts";
        let knowledge = Stories {
            included: Vec::new(),
            filtered: 0,
            areas: BTreeMap::new(),
            file_areas: [(file, &payment)].into_iter().collect(),
        };
        let budget = Budget::new(None).unwrap();
        let mut assertions = Assertions::new(&budget);
        let mut asserted = BTreeSet::new();
        let mut out = Text::new(&budget, "").unwrap();
        rules_per_capability(
            (&nodes, &edges),
            &knowledge,
            (&mut assertions, &mut asserted),
            &mut out,
        )
        .unwrap();
        let text = out.finish();
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}\n{text}"))
        };
        assert!(
            at("### Payment\n") < at("#### State machine: defaultPaymentProcess"),
            "{text}"
        );
        assert!(
            at("#### State machine: defaultPaymentProcess")
                < at("#### Rules in the Payment area's own code"),
            "{text}"
        );
        assert!(
            text.contains("| Authorized | Settled, Declined | F-"),
            "{text}"
        );
        assert!(text.contains("assertPaymentState"), "{text}");
    }
}
