//! flask-limiter rate limits on Flask route handlers (#1998, AC-0881):
//! `props.rate_limits` on the Endpoint, computed after every file is read.
//!
//! A decorator `@<limiter>.limit(<value>, …)` or `.shared_limit(…)` below the
//! handler's route decorators (so it wraps the registered view) is read when
//! `<limiter>` is a module-level name proven to be a flask-limiter `Limiter`
//! (the definitions `auth_guards` proves, #1157), and its first argument is:
//!
//! - a string literal in flask-limiter's notation (`"100/day;50/hour"`,
//!   `"10 per minute"`): `value: "literal"`, Confirmed;
//! - a module-level name, in the handler's module or imported by name from a
//!   repository module, bound exactly once to such a literal (Confirmed), or
//!   to `os.environ.get("K") or "<literal>"`, `os.environ.get("K", "<literal>")`
//!   or the same with `os.getenv`: `value: "environment_default"` with
//!   `environment_key`, InferredStrong, because the environment may replace
//!   the default.
//!
//! Anything else (a call, a computed string, a name that is rebound or not
//! found) is kept as an entry with `unresolved` and its source text, and the
//! whole fact is a Gap when no entry resolved. A `key_func` keyword is noted
//! as `keyed_by: "custom"`, a `deduct_when` or `exempt_when` keyword as
//! `conditional: true` (not every request counts); otherwise flask-limiter's own key applies, and
//! the entry says nothing about it. A receiver that is not a proven
//! `Limiter` records nothing here (`auth_guards` keeps it unclassified).
//!
//! Only strings that parse as flask-limiter limits are ever stored, so no
//! other module constant is copied into the graph.

use crate::app_scope::{self, AppRef};
use crate::auth_guards::{import_dotted, named, unparenthesize};
use crate::{Extraction, FileCx, ImportBinding, literal_string, module_binding, prov_from};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as TsNode;

/// The first argument of a limit decorator.
#[derive(Debug, Clone)]
pub(crate) enum LimitArg {
    Literal(String),
    /// A name, by its definition key.
    Name((String, String)),
    /// Anything else.
    Other,
}

/// One limit decorator on a route handler, before the limiter is proven.
#[derive(Debug, Clone)]
pub(crate) struct PendingLimit {
    pub handler: String,
    pub receiver: (String, String),
    pub arg: LimitArg,
    pub text: String,
    pub custom_key: bool,
    /// A `deduct_when` or `exempt_when` keyword: not every request counts.
    pub conditional: bool,
    pub shared: bool,
    pub evidence: EvidenceRef,
}

/// A module-level name bound once to a limit string.
#[derive(Debug, Clone)]
pub(crate) struct LimitConstant {
    pub key: (String, String),
    pub value: String,
    pub environment_key: Option<String>,
    pub evidence: EvidenceRef,
}

/// One parsed limit: `count` per `per` `unit`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Limit {
    pub count: u64,
    pub per: u64,
    pub unit: &'static str,
}

const UNITS: [&str; 6] = ["second", "minute", "hour", "day", "month", "year"];

/// flask-limiter's notation: items separated by `;`, `,` or `|`, each
/// `<count> (/|per) [<n>] <unit>[s]`. `None` when any item does not parse.
pub(crate) fn parse(text: &str) -> Option<Vec<Limit>> {
    let mut out = Vec::new();
    for item in text.split([';', ',', '|']) {
        let item = item.trim().to_ascii_lowercase();
        if item.is_empty() {
            continue;
        }
        let (count, rest) = match item.split_once('/') {
            Some((count, rest)) => (count.trim(), rest.trim()),
            None => {
                let (count, rest) = item.split_once(" per ")?;
                (count.trim(), rest.trim())
            }
        };
        let count: u64 = count.parse().ok()?;
        let (per, unit) = match rest.split_once(char::is_whitespace) {
            Some((n, unit)) => (n.parse().ok()?, unit.trim()),
            None => {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                if digits == 0 {
                    (1, rest)
                } else {
                    (rest[..digits].parse().ok()?, rest[digits..].trim())
                }
            }
        };
        let unit = unit.strip_suffix('s').unwrap_or(unit);
        let unit = UNITS.into_iter().find(|known| *known == unit)?;
        if count == 0 || per == 0 {
            return None;
        }
        out.push(Limit { count, per, unit });
    }
    (!out.is_empty()).then_some(out)
}

/// Collect the limit decorators of `functions` and the module's limit
/// constants.
pub(crate) fn collect(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    functions: &[(TsNode<'_>, String)],
    out: &mut Extraction,
) {
    for (function, handler) in functions {
        let Some(parent) = function
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
        else {
            continue;
        };
        let decorators: Vec<TsNode<'_>> = named(parent)
            .into_iter()
            .filter(|child| child.kind() == "decorator")
            .collect();
        let Some(last_route) = decorators
            .iter()
            .rposition(|decorator| crate::auth_guards::is_route_decorator(cx, *decorator))
        else {
            continue;
        };
        for decorator in &decorators[last_route + 1..] {
            if let Some(pending) = limit_decorator(cx, *decorator, handler, bindings) {
                out.pending_limits.push(pending);
            }
        }
    }
    for statement in named(root) {
        if statement.kind() != "expression_statement" {
            continue;
        }
        let Some(assignment) = statement
            .named_child(0)
            .filter(|n| n.kind() == "assignment")
        else {
            continue;
        };
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }
        let name = cx.text(&left);
        let writes = module_binding(cx, root, name).writes;
        if writes.len() != 1 || writes[0] != assignment {
            continue;
        }
        if let Some((value, environment_key)) = limit_value(cx, right, bindings) {
            out.limit_constants.push(LimitConstant {
                key: (cx.path.to_string(), name.to_string()),
                value,
                environment_key,
                evidence: cx.evidence(&assignment),
            });
        }
    }
}

fn limit_decorator(
    cx: &FileCx<'_>,
    decorator: TsNode<'_>,
    handler: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<PendingLimit> {
    let call = unparenthesize(decorator.named_child(0)?);
    if call.kind() != "call" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    if callee.kind() != "attribute" {
        return None;
    }
    let verb = cx.text(&callee.child_by_field_name("attribute")?);
    let shared = match verb {
        "limit" => false,
        "shared_limit" => true,
        _ => return None,
    };
    let AppRef::Def(receiver) =
        app_scope::app_ref(cx, callee.child_by_field_name("object")?, bindings)
    else {
        return None;
    };
    let arguments = named(call.child_by_field_name("arguments")?);
    let first = arguments
        .iter()
        .find(|argument| !matches!(argument.kind(), "comment" | "keyword_argument"))
        .or_else(|| {
            arguments.iter().find(|argument| {
                argument.kind() == "keyword_argument"
                    && argument
                        .child_by_field_name("name")
                        .is_some_and(|name| matches!(cx.text(&name), "limit_value"))
            })
        })
        .map(|argument| {
            argument
                .child_by_field_name("value")
                .filter(|_| argument.kind() == "keyword_argument")
                .unwrap_or(*argument)
        })?;
    let keyword = |wanted: &[&str]| {
        arguments.iter().any(|argument| {
            argument.kind() == "keyword_argument"
                && argument
                    .child_by_field_name("name")
                    .is_some_and(|name| wanted.contains(&cx.text(&name)))
        })
    };
    let custom_key = keyword(&["key_func"]);
    let conditional = keyword(&["deduct_when", "exempt_when"]);
    let arg = match first.kind() {
        "string" => match literal_string(cx, first).filter(|text| parse(text).is_some()) {
            Some(text) => LimitArg::Literal(text),
            None => LimitArg::Other,
        },
        "identifier" => match app_scope::app_ref(cx, first, bindings) {
            AppRef::Def(key) => LimitArg::Name(key),
            _ => LimitArg::Other,
        },
        _ => LimitArg::Other,
    };
    Some(PendingLimit {
        handler: handler.to_string(),
        receiver,
        arg,
        text: cx.text(&call).chars().take(200).collect(),
        custom_key,
        conditional,
        shared,
        evidence: cx.evidence(&decorator),
    })
}

/// A limit literal, or an environment read with a limit literal default.
fn limit_value(
    cx: &FileCx<'_>,
    value: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(String, Option<String>)> {
    let value = unparenthesize(value);
    match value.kind() {
        "string" => {
            let text = literal_string(cx, value)?;
            parse(&text)?;
            Some((text, None))
        }
        // `os.environ.get("K") or "<literal>"`
        "boolean_operator" => {
            let operator = value.child_by_field_name("operator")?;
            if cx.text(&operator) != "or" {
                return None;
            }
            let left = unparenthesize(value.child_by_field_name("left")?);
            let right = unparenthesize(value.child_by_field_name("right")?);
            let (key, default) = environment_read(cx, left, bindings)?;
            if default.is_some() {
                return None;
            }
            let text = literal_string(cx, right)?;
            parse(&text)?;
            Some((text, Some(key)))
        }
        // `os.environ.get("K", "<literal>")`
        "call" => {
            let (key, default) = environment_read(cx, value, bindings)?;
            let text = default?;
            parse(&text)?;
            Some((text, Some(key)))
        }
        _ => None,
    }
}

/// `os.environ.get("K"[, "<default>"])` or `os.getenv(…)`, with `os` the
/// standard module: the key and the literal default.
fn environment_read(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(String, Option<String>)> {
    if call.kind() != "call" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    let dotted = import_dotted(cx, callee, bindings)?;
    if !matches!(dotted.as_str(), "os.environ.get" | "os.getenv") {
        return None;
    }
    let arguments: Vec<TsNode<'_>> = named(call.child_by_field_name("arguments")?)
        .into_iter()
        .filter(|argument| argument.kind() != "comment")
        .collect();
    let key = literal_string(cx, *arguments.first()?)?;
    match arguments.as_slice() {
        [_] => Some((key, None)),
        [_, default] => Some((key, Some(literal_string(cx, *default)?))),
        _ => None,
    }
}

/// Set `rate_limits` on the Flask endpoints whose handler carries a limit
/// decorator on a proven limiter. Runs before `auth_guards::resolve`, which
/// consumes the limiter definitions.
pub(crate) fn resolve(out: &mut Extraction, limiters: &HashSet<(String, String)>) {
    let pending = std::mem::take(&mut out.pending_limits);
    if pending.is_empty() {
        out.limit_constants.clear();
        return;
    }
    let constants: HashMap<(String, String), LimitConstant> =
        std::mem::take(&mut out.limit_constants)
            .into_iter()
            .map(|constant| (constant.key.clone(), constant))
            .collect();
    let mut by_handler: HashMap<String, Vec<PendingLimit>> = HashMap::new();
    for limit in pending {
        if limiters.contains(&limit.receiver) {
            by_handler
                .entry(limit.handler.clone())
                .or_default()
                .push(limit);
        }
    }
    for node in &mut out.nodes {
        if node.label != "Endpoint" || node.props["framework"] != "flask" {
            continue;
        }
        let Some(limits) = node.props["handler_sym"]
            .as_str()
            .and_then(|handler| by_handler.get(handler))
        else {
            continue;
        };
        let handler = node.props["handler_sym"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        node.props["rate_limits"] = props(&handler, limits, &constants);
    }
}

fn props(
    handler: &str,
    limits: &[PendingLimit],
    constants: &HashMap<(String, String), LimitConstant>,
) -> serde_json::Value {
    let mut entries = Vec::new();
    let mut evidence = Vec::new();
    let mut resolved = 0usize;
    let mut environment = false;
    for limit in limits {
        evidence.push(limit.evidence.clone());
        let mut entry = serde_json::json!({ "text": limit.text });
        let value = match &limit.arg {
            LimitArg::Literal(text) => Some((text.clone(), None)),
            LimitArg::Name(key) => constants.get(key).map(|constant| {
                evidence.push(constant.evidence.clone());
                (constant.value.clone(), constant.environment_key.clone())
            }),
            LimitArg::Other => None,
        };
        match value.and_then(|(text, key)| Some((parse(&text)?, text, key))) {
            Some((parsed, text, key)) => {
                resolved += 1;
                entry["value"] = text.into();
                entry["limits"] = parsed
                    .iter()
                    .map(|l| serde_json::json!({"count": l.count, "per": l.per, "unit": l.unit}))
                    .collect();
                match key {
                    Some(key) => {
                        environment = true;
                        entry["source"] = "environment_default".into();
                        entry["environment_key"] = key.into();
                    }
                    None => entry["source"] = "literal".into(),
                }
            }
            None => {
                entry["unresolved"] = match &limit.arg {
                    LimitArg::Name(_) => "limit_value_not_a_proven_constant",
                    _ => "limit_value_not_literal",
                }
                .into();
            }
        }
        if limit.custom_key {
            entry["keyed_by"] = "custom".into();
        }
        if limit.shared {
            entry["shared"] = true.into();
        }
        if limit.conditional {
            entry["conditional"] = true.into();
        }
        entries.push(entry);
    }
    evidence.sort_by(|a, b| (&a.path, a.byte_start).cmp(&(&b.path, b.byte_start)));
    evidence.dedup();
    let confidence = if resolved == 0 {
        ConfidenceTier::Gap
    } else if environment {
        ConfidenceTier::InferredStrong
    } else {
        ConfidenceTier::Confirmed
    };
    // flask-limiter refuses a request past a limit with 429 Too Many
    // Requests (`RateLimitExceeded`), so a rule stating the refusal cites
    // the Endpoint for its status too (#2539).
    let mut out = serde_json::json!({ "entries": entries, "exceeded_status": 429 });
    if resolved < limits.len() {
        out["partial"] = serde_json::json!(["limit_value_unresolved"]);
    }
    out["prov"] = prov_from(evidence, confidence, &format!("Rate limits of {handler}"));
    out
}

/// [`resolve`] over the limiters `auth_guards` proves, without consuming them.
pub(crate) fn resolve_proven(out: &mut Extraction) {
    let patched = out.patched_guards.iter().cloned().collect();
    let limiters = crate::auth_guards::proven_limiters(out, &patched);
    resolve(out, &limiters);
}

/// Point pending evidence at `commit`.
pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for limit in &mut extraction.pending_limits {
        limit.evidence.commit_sha = commit.into();
    }
    for constant in &mut extraction.limit_constants {
        constant.evidence.commit_sha = commit.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0881 (T-0881): flask-limiter's notation parses; anything else does not.
    #[test]
    fn parses_flask_limiter_notation() {
        let parsed = parse("100/day;50/hour;5/minute").unwrap();
        assert_eq!(
            parsed,
            vec![
                Limit {
                    count: 100,
                    per: 1,
                    unit: "day"
                },
                Limit {
                    count: 50,
                    per: 1,
                    unit: "hour"
                },
                Limit {
                    count: 5,
                    per: 1,
                    unit: "minute"
                },
            ]
        );
        assert_eq!(
            parse("10 per minute, 2/5 seconds").unwrap(),
            vec![
                Limit {
                    count: 10,
                    per: 1,
                    unit: "minute"
                },
                Limit {
                    count: 2,
                    per: 5,
                    unit: "second"
                },
            ]
        );
        for bad in [
            "",
            "fast",
            "10/fortnight",
            "0/hour",
            "x/hour",
            "100/day;oops",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }
}
