//! Raw Node `http.createServer((req, res) => …)` routes (#1077).
//!
//! A `createServer` proven to come from `node:http`, `http`, `node:https` or
//! `https` (named, default or namespace import, or a `require`) whose handler
//! is an inline function or a same-file function declaration/const is read for
//! route predicates over the request URL. Each `if`/ternary/`&&` guard that
//! compares the pathname (`new URL(req.url, …).pathname` bound to a const, or
//! `req.url` itself) against a literal becomes an `Endpoint`
//! (`framework: "node-http"`) that the handler `HANDLES`:
//!
//! - `pathname === '/x'`, optionally ANDed with `req.method === 'M'`
//!   (no method guard is method `ALL`, as Express `all`);
//! - `pathname.startsWith('/x/')`, a prefix route (`/x/*`, `match: "prefix"`);
//! - `/^\/x\/(a|b)$/.exec(pathname)` / `.test(pathname)` with only literal
//!   alternatives, one path per alternative;
//! - `['GET /a', 'POST /b'].includes(`${req.method} ${pathname}`)`.
//!
//! Any other read of the pathname through a call, and any other regex, is a
//! cited Gap: the route is computed and never guessed.

use crate::callable::{self, Bindings};
use crate::{FileCx, literal_string};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as TsNode;

/// Framework label on every endpoint this module emits.
pub const FRAMEWORK: &str = "node-http";
/// Gap reason for a pathname read through a call this module cannot evaluate.
pub const COMPUTED_REASON: &str = "computed node:http route";
/// Gap reason for a regex that is not `^literal(alt|alt)$`.
pub const REGEX_REASON: &str = "node:http route regex is not a literal alternation";

const MODULES: &[&str] = &["node:http", "http", "node:https", "https"];
const MAX_EXPANSION: usize = 64;

/// File-level import proofs the extractor already computed.
pub(crate) struct Proofs<'a, 'tree> {
    pub bindings: &'a Bindings<'tree>,
    pub import_modules: &'a HashMap<String, String>,
    pub imported_names: &'a HashMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    Exact,
    Prefix,
}

struct Route<'t> {
    kind: Match,
    path: String,
    /// A method carried by the route itself (`includes` entries).
    method: Option<String>,
    node: TsNode<'t>,
}

struct GapSite<'t> {
    reason: &'static str,
    node: TsNode<'t>,
}

/// One way a condition can hold: the routes it names under one method guard.
#[derive(Default)]
struct Alt<'t> {
    routes: Vec<Route<'t>>,
    gaps: Vec<GapSite<'t>>,
    method: Option<(String, TsNode<'t>)>,
}

fn substitution(part: TsNode<'_>) -> Option<TsNode<'_>> {
    (part.kind() == "template_substitution")
        .then(|| part.named_child(0))
        .flatten()
}

fn strip(mut node: TsNode<'_>) -> TsNode<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
    ) {
        let Some(inner) = node.named_child(0) else {
            break;
        };
        node = inner;
    }
    node
}

fn args_of(call: TsNode<'_>) -> Vec<TsNode<'_>> {
    call.child_by_field_name("arguments")
        .map(|args| {
            let mut walk = args.walk();
            args.named_children(&mut walk).collect()
        })
        .unwrap_or_default()
}

fn const_declarator(node: TsNode<'_>) -> bool {
    node.parent()
        .and_then(|parent| parent.child(0))
        .is_some_and(|keyword| keyword.kind() == "const")
}

/// What a `const … = require('m')` declarator binds `name` to: the module and
/// the exported member for a destructured binding.
fn required(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, '_>,
    site: TsNode<'_>,
    name: &str,
) -> Option<(String, Option<String>)> {
    let declarator = proofs.bindings.stable_declaration(site, name)?;
    if declarator.kind() != "variable_declarator" || !const_declarator(declarator) {
        return None;
    }
    let value = strip(declarator.child_by_field_name("value")?);
    if value.kind() != "call_expression" {
        return None;
    }
    let callee = value.child_by_field_name("function")?;
    if callee.kind() != "identifier"
        || cx.text(&callee) != "require"
        || !proofs.bindings.global_unshadowed(value, "require")
    {
        return None;
    }
    let spec = literal_string(cx, *args_of(value).first()?)?;
    let pattern = declarator.child_by_field_name("name")?;
    match pattern.kind() {
        "identifier" => (cx.text(&pattern) == name).then_some((spec, None)),
        "object_pattern" => {
            let mut walk = pattern.walk();
            let children: Vec<_> = pattern.named_children(&mut walk).collect();
            children.into_iter().find_map(|child| match child.kind() {
                "shorthand_property_identifier_pattern" if cx.text(&child) == name => {
                    Some((spec.clone(), Some(name.to_string())))
                }
                "pair_pattern" => {
                    let key = child.child_by_field_name("key")?;
                    let value = child.child_by_field_name("value")?;
                    (cx.text(&value) == name)
                        .then(|| (spec.clone(), Some(cx.text(&key).to_string())))
                }
                _ => None,
            })
        }
        _ => None,
    }
}

/// True when `callee` is a `createServer` proven to come from a Node HTTP(S)
/// module.
fn is_create_server(cx: &FileCx<'_>, proofs: &Proofs<'_, '_>, callee: TsNode<'_>) -> bool {
    let callee = strip(callee);
    match callee.kind() {
        "identifier" => {
            let name = cx.text(&callee);
            if proofs.bindings.imported(callee, name) {
                return proofs
                    .import_modules
                    .get(name)
                    .is_some_and(|spec| MODULES.contains(&spec.as_str()))
                    && proofs.imported_names.get(name).map(String::as_str) == Some("createServer");
            }
            required(cx, proofs, callee, name).is_some_and(|(spec, member)| {
                MODULES.contains(&spec.as_str()) && member.as_deref() == Some("createServer")
            })
        }
        "member_expression" => {
            let (Some(object), Some(property)) = (
                callee.child_by_field_name("object"),
                callee.child_by_field_name("property"),
            ) else {
                return false;
            };
            if object.kind() != "identifier" || cx.text(&property) != "createServer" {
                return false;
            }
            let name = cx.text(&object);
            if proofs.bindings.imported(object, name) {
                return proofs
                    .import_modules
                    .get(name)
                    .is_some_and(|spec| MODULES.contains(&spec.as_str()))
                    && !proofs.imported_names.contains_key(name);
            }
            required(cx, proofs, object, name)
                .is_some_and(|(spec, member)| MODULES.contains(&spec.as_str()) && member.is_none())
        }
        _ => false,
    }
}

/// The handler function node: an inline function, or a same-file function
/// declaration / const-bound function the argument names.
fn handler_function<'t>(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, 't>,
    argument: TsNode<'t>,
) -> Option<TsNode<'t>> {
    let argument = strip(argument);
    match argument.kind() {
        "arrow_function" | "function_expression" => Some(argument),
        "identifier" => {
            let declaration = proofs
                .bindings
                .stable_declaration(argument, cx.text(&argument))?;
            match declaration.kind() {
                "function_declaration" => Some(declaration),
                "variable_declarator" if const_declarator(declaration) => declaration
                    .child_by_field_name("value")
                    .map(strip)
                    .filter(|value| {
                        matches!(value.kind(), "arrow_function" | "function_expression")
                    }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The request parameter's name and the node `Bindings` records as its
/// declaration: the arrow function for `req => …`, else the first parameter.
fn request_param<'t>(cx: &FileCx<'_>, function: TsNode<'t>) -> Option<(String, TsNode<'t>)> {
    if let Some(parameter) = function.child_by_field_name("parameter") {
        return (parameter.kind() == "identifier")
            .then(|| (cx.text(&parameter).to_string(), function));
    }
    let first = function.child_by_field_name("parameters")?.named_child(0)?;
    match first.kind() {
        "required_parameter" | "optional_parameter" => {
            let pattern = first.child_by_field_name("pattern")?;
            (pattern.kind() == "identifier").then(|| (cx.text(&pattern).to_string(), first))
        }
        _ => None,
    }
}

/// Reads of the request URL inside one handler.
struct Handler<'a, 't> {
    cx: &'a FileCx<'a>,
    proofs: &'a Proofs<'a, 't>,
    req: String,
    /// The request parameter's declaration; a `req` resolving elsewhere (a
    /// nested parameter, a shadowing `const`) is not the server request.
    req_declaration: TsNode<'t>,
    /// `const url = new URL(req.url, …)` declarators by name.
    urls: HashMap<String, TsNode<'t>>,
    /// `const pathname = new URL(req.url, …).pathname` declarators by name.
    paths: HashMap<String, TsNode<'t>>,
}

impl<'a, 't> Handler<'a, 't> {
    fn is_req_member(&self, node: TsNode<'_>, property: &str) -> bool {
        let node = strip(node);
        node.kind() == "member_expression"
            && node.child_by_field_name("object").is_some_and(|object| {
                object.kind() == "identifier"
                    && self.cx.text(&object) == self.req
                    && self
                        .proofs
                        .bindings
                        .stable_declaration(object, &self.req)
                        .is_some_and(|declaration| declaration.id() == self.req_declaration.id())
            })
            && node
                .child_by_field_name("property")
                .is_some_and(|p| self.cx.text(&p) == property)
    }

    fn is_url_ctor(&self, node: TsNode<'_>) -> bool {
        let node = strip(node);
        if node.kind() != "new_expression" {
            return false;
        }
        let Some(ctor) = node.child_by_field_name("constructor") else {
            return false;
        };
        if ctor.kind() != "identifier" || self.cx.text(&ctor) != "URL" {
            return false;
        }
        let proven = self.proofs.bindings.global_unshadowed(ctor, "URL")
            || (self.proofs.bindings.imported(ctor, "URL")
                && matches!(
                    self.proofs.import_modules.get("URL").map(String::as_str),
                    Some("node:url" | "url")
                ));
        proven
            && args_of(node)
                .first()
                .is_some_and(|first| self.is_req_member(*first, "url"))
    }

    fn bound_to(&self, map: &HashMap<String, TsNode<'t>>, node: TsNode<'_>) -> bool {
        let name = self.cx.text(&node);
        map.get(name).is_some_and(|declarator| {
            self.proofs
                .bindings
                .stable_declaration(node, name)
                .is_some_and(|found| found.id() == declarator.id())
        })
    }

    fn is_pathname(&self, node: TsNode<'_>) -> bool {
        let node = strip(node);
        match node.kind() {
            "identifier" => self.bound_to(&self.paths, node),
            "member_expression" => {
                if self.is_req_member(node, "url") {
                    return true;
                }
                let (Some(object), Some(property)) = (
                    node.child_by_field_name("object"),
                    node.child_by_field_name("property"),
                ) else {
                    return false;
                };
                if self.cx.text(&property) != "pathname" {
                    return false;
                }
                let object = strip(object);
                (object.kind() == "identifier" && self.bound_to(&self.urls, object))
                    || self.is_url_ctor(object)
            }
            _ => false,
        }
    }

    fn mentions_pathname(&self, node: TsNode<'_>) -> bool {
        callable::walk(node)
            .into_iter()
            .any(|inner| self.is_pathname(inner))
    }

    /// The outermost calls under `node` that read the pathname.
    fn computed_calls(&self, node: TsNode<'t>, gaps: &mut Vec<GapSite<'t>>) {
        if node.kind() == "call_expression" && self.mentions_pathname(node) {
            gaps.push(GapSite {
                reason: COMPUTED_REASON,
                node,
            });
            return;
        }
        let mut walk = node.walk();
        let children: Vec<_> = node.named_children(&mut walk).collect();
        for child in children {
            self.computed_calls(child, gaps);
        }
    }

    fn neutral_with_gaps(&self, node: TsNode<'t>) -> Vec<Alt<'t>> {
        let mut gaps = Vec::new();
        self.computed_calls(node, &mut gaps);
        vec![Alt {
            gaps,
            ..Alt::default()
        }]
    }

    fn analyze(&self, node: TsNode<'t>) -> Vec<Alt<'t>> {
        let node = strip(node);
        match node.kind() {
            "binary_expression" => self.binary(node),
            "unary_expression" => {
                // A negated predicate names no route, but its calls may still
                // read the pathname through code this module cannot evaluate.
                let mut alts = node
                    .child_by_field_name("argument")
                    .map(|argument| self.analyze(argument))
                    .unwrap_or_default();
                for alt in &mut alts {
                    alt.routes.clear();
                    alt.method = None;
                }
                alts
            }
            "call_expression" => self.call(node),
            _ => self.neutral_with_gaps(node),
        }
    }

    fn binary(&self, node: TsNode<'t>) -> Vec<Alt<'t>> {
        let (Some(left), Some(right), Some(operator)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
            node.child_by_field_name("operator"),
        ) else {
            return self.neutral_with_gaps(node);
        };
        match self.cx.text(&operator) {
            "||" => {
                let mut alts = self.analyze(left);
                alts.extend(self.analyze(right));
                alts
            }
            "&&" => self.and(node, self.analyze(left), self.analyze(right)),
            "===" | "==" => {
                for (a, b) in [(left, right), (right, left)] {
                    if self.is_req_member(a, "method")
                        && let Some(method) = literal_string(self.cx, strip(b))
                    {
                        return vec![Alt {
                            method: Some((method.to_ascii_uppercase(), node)),
                            ..Alt::default()
                        }];
                    }
                    if self.is_pathname(a)
                        && strip(b).kind() == "string"
                        && let Some(path) = literal_string(self.cx, strip(b))
                    {
                        return vec![Alt {
                            routes: vec![Route {
                                kind: Match::Exact,
                                path,
                                method: None,
                                node,
                            }],
                            ..Alt::default()
                        }];
                    }
                }
                self.neutral_with_gaps(node)
            }
            _ => self.neutral_with_gaps(node),
        }
    }

    fn and(&self, whole: TsNode<'t>, left: Vec<Alt<'t>>, right: Vec<Alt<'t>>) -> Vec<Alt<'t>> {
        let mut out = Vec::new();
        for l in &left {
            for r in &right {
                let method = match (&l.method, &r.method) {
                    (Some(a), Some(b)) if a.0 != b.0 => continue, // unreachable branch
                    (Some(a), _) | (None, Some(a)) => Some(a.clone()),
                    (None, None) => None,
                };
                let mut alt = Alt {
                    method,
                    ..Alt::default()
                };
                let copy = |route: &Route<'t>| Route {
                    kind: route.kind,
                    path: route.path.clone(),
                    method: route.method.clone(),
                    node: route.node,
                };
                let gap = |g: &GapSite<'t>| GapSite {
                    reason: g.reason,
                    node: g.node,
                };
                alt.gaps.extend(l.gaps.iter().map(gap));
                alt.gaps.extend(r.gaps.iter().map(gap));
                if !l.routes.is_empty() && !r.routes.is_empty() {
                    // Two path predicates in one conjunction: not a route we
                    // can state without guessing which one governs.
                    alt.gaps.push(GapSite {
                        reason: COMPUTED_REASON,
                        node: whole,
                    });
                } else {
                    alt.routes.extend(l.routes.iter().map(copy));
                    alt.routes.extend(r.routes.iter().map(copy));
                }
                out.push(alt);
            }
        }
        out
    }

    fn call(&self, call: TsNode<'t>) -> Vec<Alt<'t>> {
        let Some(callee) = call.child_by_field_name("function").map(strip) else {
            return self.neutral_with_gaps(call);
        };
        let args = args_of(call);
        if callee.kind() == "member_expression"
            && let (Some(object), Some(property)) = (
                callee.child_by_field_name("object").map(strip),
                callee.child_by_field_name("property"),
            )
        {
            match (self.cx.text(&property), args.first()) {
                ("startsWith", Some(arg)) if self.is_pathname(object) => {
                    if let Some(path) = literal_string(self.cx, strip(*arg)) {
                        return vec![Alt {
                            routes: vec![Route {
                                kind: Match::Prefix,
                                path,
                                method: None,
                                node: call,
                            }],
                            ..Alt::default()
                        }];
                    }
                }
                ("exec" | "test", Some(arg))
                    if object.kind() == "regex" && self.is_pathname(*arg) =>
                {
                    return vec![match regex_paths(self.cx, object) {
                        Some(paths) => Alt {
                            routes: paths
                                .into_iter()
                                .map(|path| Route {
                                    kind: Match::Exact,
                                    path,
                                    method: None,
                                    node: call,
                                })
                                .collect(),
                            ..Alt::default()
                        },
                        None => Alt {
                            gaps: vec![GapSite {
                                reason: REGEX_REASON,
                                node: call,
                            }],
                            ..Alt::default()
                        },
                    }];
                }
                ("includes", Some(arg))
                    if object.kind() == "array" && self.is_method_path_template(*arg) =>
                {
                    return vec![self.includes(object, call)];
                }
                _ => {}
            }
        }
        self.neutral_with_gaps(call)
    }

    /// The method an enclosing `if (req.method === 'M') { … }` imposes on
    /// `node`: `Some(None)` when none does. `None` declines the claim: two
    /// guards disagree (unreachable), or an enclosing condition constrains the
    /// method in a way one method cannot state (its `else` branch, a negated
    /// or mixed guard).
    fn enclosing_method(
        &self,
        node: TsNode<'t>,
        function: TsNode<'t>,
    ) -> Option<Option<(String, TsNode<'t>)>> {
        let mut method: Option<(String, TsNode<'t>)> = None;
        let mut current = node;
        while let Some(parent) = current.parent() {
            if parent.id() == function.id() {
                break;
            }
            if parent.kind() == "if_statement"
                && let Some(condition) = parent.child_by_field_name("condition")
                && callable::walk(condition)
                    .into_iter()
                    .any(|inner| self.is_req_member(inner, "method"))
            {
                let in_consequence = parent
                    .child_by_field_name("consequence")
                    .is_some_and(|block| block.id() == current.id());
                if !in_consequence {
                    return None;
                }
                let alts = self.analyze(condition);
                // Only a pure method guard: every way it holds names the
                // same method and no route.
                let guard = alts.first().and_then(|first| first.method.clone())?;
                if !alts.iter().all(|alt| {
                    alt.routes.is_empty() && alt.method.as_ref().is_some_and(|m| m.0 == guard.0)
                }) {
                    return None;
                }
                match &method {
                    Some(inner) if inner.0 != guard.0 => return None,
                    Some(_) => {}
                    None => method = Some(guard),
                }
            }
            current = parent;
        }
        Some(method)
    }

    /// `` `${req.method} ${pathname}` ``.
    fn is_method_path_template(&self, node: TsNode<'t>) -> bool {
        let node = strip(node);
        if node.kind() != "template_string" {
            return false;
        }
        let mut walk = node.walk();
        let parts: Vec<_> = node.named_children(&mut walk).collect();
        let [first, sep, last] = parts.as_slice() else {
            return false;
        };
        let inner = |part: &TsNode<'t>| substitution(*part);
        sep.kind() == "string_fragment"
            && self.cx.text(sep) == " "
            && inner(first).is_some_and(|method| self.is_req_member(method, "method"))
            && inner(last).is_some_and(|path| self.is_pathname(path))
    }

    fn includes(&self, list: TsNode<'t>, call: TsNode<'t>) -> Alt<'t> {
        let mut alt = Alt::default();
        let mut walk = list.walk();
        let elements: Vec<_> = list.named_children(&mut walk).collect();
        let mut invalid = false;
        for element in elements {
            let entry = (strip(element).kind() == "string")
                .then(|| literal_string(self.cx, strip(element)))
                .flatten()
                .and_then(|text| {
                    let (method, path) = text.split_once(' ')?;
                    (!method.is_empty()
                        && method.bytes().all(|b| b.is_ascii_uppercase())
                        && path.starts_with('/'))
                    .then(|| (method.to_string(), path.to_string()))
                });
            match entry {
                Some((method, path)) => alt.routes.push(Route {
                    kind: Match::Exact,
                    path,
                    method: Some(method),
                    node: call,
                }),
                None => invalid = true,
            }
        }
        if invalid {
            alt.gaps.push(GapSite {
                reason: COMPUTED_REASON,
                node: call,
            });
        }
        alt
    }
}

/// Expand `^literal(alt1|alt2)$` into one path per alternative.
fn regex_paths(cx: &FileCx<'_>, regex: TsNode<'_>) -> Option<Vec<String>> {
    let mut walk = regex.walk();
    let children: Vec<_> = regex.named_children(&mut walk).collect();
    if children.iter().any(|c| c.kind() == "regex_flags") {
        return None;
    }
    let pattern = children
        .iter()
        .find(|c| c.kind() == "regex_pattern")
        .map(|c| cx.text(c).to_string())?;
    expand_pattern(&pattern)
}

fn literal_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '~' | '%')
}

/// Parse a run of literal characters (with `\/`-style escapes of punctuation).
fn literal_run(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let escaped = chars.next()?;
            if matches!(escaped, '/' | '.' | '-' | '_' | '~' | '%') {
                out.push(escaped);
            } else {
                return None;
            }
        } else if literal_char(c) {
            out.push(c);
        } else {
            return None;
        }
    }
    Some(out)
}

fn expand_pattern(pattern: &str) -> Option<Vec<String>> {
    let body = pattern.strip_prefix('^')?.strip_suffix('$')?;
    let mut paths = vec![String::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let open = find_unescaped(rest, '(');
        let (literal, group) = match open {
            Some(at) => (&rest[..at], Some(&rest[at + 1..])),
            None => (rest, None),
        };
        let literal = literal_run(literal)?;
        for path in &mut paths {
            path.push_str(&literal);
        }
        let Some(group) = group else {
            break;
        };
        let close = find_unescaped(group, ')')?;
        let inner = &group[..close];
        let inner = inner.strip_prefix("?:").unwrap_or(inner);
        let alternatives = inner
            .split('|')
            .map(literal_run)
            .collect::<Option<Vec<_>>>()?;
        if alternatives.iter().any(String::is_empty)
            || paths.len() * alternatives.len() > MAX_EXPANSION
        {
            return None;
        }
        paths = paths
            .iter()
            .flat_map(|path| alternatives.iter().map(move |alt| format!("{path}{alt}")))
            .collect();
        rest = &group[close + 1..];
    }
    paths
        .iter()
        .all(|path| path.starts_with('/'))
        .then_some(paths)
}

fn find_unescaped(text: &str, target: char) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == target {
            return Some(at);
        }
    }
    None
}

fn cite<'t>(nodes: &[TsNode<'t>]) -> Vec<TsNode<'t>> {
    let mut nodes = nodes.to_vec();
    nodes.sort_by_key(|node| (node.start_byte(), node.end_byte()));
    nodes.dedup_by_key(|node| (node.start_byte(), node.end_byte()));
    nodes
}

/// Emit the `Endpoint`/`Gap` facts of every proven `createServer` in a file.
pub(crate) fn extract<'t>(
    cx: &FileCx<'_>,
    root: TsNode<'t>,
    proofs: &Proofs<'_, 't>,
    out: &mut crate::Extraction,
) {
    let mut seen_endpoints = HashSet::new();
    let mut seen_handles = HashSet::new();
    let mut seen_gaps = HashSet::new();
    let mut seen_handlers = HashSet::new();
    for call in callable::walk(root) {
        if call.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = call.child_by_field_name("function") else {
            continue;
        };
        if !is_create_server(cx, proofs, callee) {
            continue;
        }
        let Some(argument) = args_of(call).last().copied() else {
            continue;
        };
        let Some(function) = handler_function(cx, proofs, argument) else {
            continue;
        };
        if !seen_handlers.insert(function.id()) {
            continue;
        }
        let Some((req, req_declaration)) = request_param(cx, function) else {
            continue;
        };
        let mut handler = Handler {
            cx,
            proofs,
            req,
            req_declaration,
            urls: HashMap::new(),
            paths: HashMap::new(),
        };
        let nodes = callable::walk(function);
        for node in &nodes {
            if node.kind() != "variable_declarator" || !const_declarator(*node) {
                continue;
            }
            let (Some(name), Some(value)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("value").map(strip),
            ) else {
                continue;
            };
            if name.kind() != "identifier" {
                continue;
            }
            let name = cx.text(&name).to_string();
            if handler.is_url_ctor(value) {
                handler.urls.insert(name, *node);
            } else if value.kind() == "member_expression"
                && value
                    .child_by_field_name("property")
                    .is_some_and(|p| cx.text(&p) == "pathname")
                && value
                    .child_by_field_name("object")
                    .is_some_and(|object| handler.is_url_ctor(object))
            {
                handler.paths.insert(name, *node);
            }
        }
        let handler_id = callable::id(cx, function);
        let mut done: Vec<(usize, usize)> = Vec::new();
        for node in &nodes {
            if node.kind() == "variable_declarator"
                && let Some(value) = node.child_by_field_name("value")
            {
                // `const route = ownerRouteName(req.method, url.pathname)`:
                // the pathname is routed through code this module cannot read.
                let mut gaps = Vec::new();
                handler.computed_calls(value, &mut gaps);
                for gap in &gaps {
                    emit_gap(cx, out, &handler_id, gap, &mut seen_gaps);
                }
            }
            let condition = match node.kind() {
                "if_statement" | "ternary_expression" => node.child_by_field_name("condition"),
                "expression_statement" => node.named_child(0).map(strip).filter(|expr| {
                    expr.kind() == "binary_expression"
                        && expr
                            .child_by_field_name("operator")
                            .is_some_and(|op| cx.text(&op) == "&&")
                }),
                _ => None,
            };
            let Some(condition) = condition else {
                continue;
            };
            let span = (condition.start_byte(), condition.end_byte());
            if done.iter().any(|&(s, e)| s <= span.0 && span.1 <= e) {
                continue;
            }
            done.push(span);
            let Some(outer) = handler.enclosing_method(*node, function) else {
                continue;
            };
            for mut alt in handler.analyze(condition) {
                match (&alt.method, &outer) {
                    (Some(own), Some(guard)) if own.0 != guard.0 => continue,
                    (None, Some(guard)) => alt.method = Some(guard.clone()),
                    _ => {}
                }
                for gap in &alt.gaps {
                    emit_gap(cx, out, &handler_id, gap, &mut seen_gaps);
                }
                for route in &alt.routes {
                    let method = match (&route.method, &alt.method) {
                        (Some(own), Some((guard, _))) if own != guard => continue,
                        (Some(own), _) => own.clone(),
                        (None, Some((guard, _))) => guard.clone(),
                        (None, None) => "ALL".to_string(),
                    };
                    let mut evidence = vec![route.node];
                    if let Some((_, guard)) = &alt.method {
                        evidence.push(*guard);
                    }
                    let evidence = cite(&evidence);
                    let path = match route.kind {
                        Match::Exact => route.path.clone(),
                        Match::Prefix => format!("{}*", route.path),
                    };
                    let ep_id = format!("ep:{}@{method}:{path}", cx.id.repo);
                    // One endpoint node, but every handler that serves it.
                    if !seen_handles.insert((ep_id.clone(), handler_id.clone())) {
                        continue;
                    }
                    let new_endpoint = seen_endpoints.insert(ep_id.clone());
                    let mut props = serde_json::json!({
                        "method": method,
                        "path": path,
                        "framework": FRAMEWORK,
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Confirmed,
                            &format!("Endpoint {method} {path}"),
                        ),
                    });
                    if route.kind == Match::Prefix {
                        props["match"] = serde_json::json!("prefix");
                    }
                    if new_endpoint {
                        out.nodes.push(Node {
                            id: ep_id.clone(),
                            label: "Endpoint".into(),
                            props,
                        });
                    }
                    out.edges.push(Edge {
                        src: ep_id,
                        dst: handler_id.clone(),
                        label: "HANDLES".into(),
                        props: serde_json::json!({
                            "prov": cx.prov_spanning(
                                &evidence,
                                ConfidenceTier::Confirmed,
                                &format!("HANDLES {method} {path}"),
                            ),
                        }),
                    });
                }
            }
        }
    }
}

fn emit_gap(
    cx: &FileCx<'_>,
    out: &mut crate::Extraction,
    handler_id: &str,
    gap: &GapSite<'_>,
    seen: &mut HashSet<usize>,
) {
    if !seen.insert(gap.node.start_byte()) {
        return;
    }
    let id = format!(
        "gap:node-http:{}@{}@{}",
        cx.id.repo,
        cx.path,
        gap.node.start_byte()
    );
    out.nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "kind": "node-http-route-computed",
            "reason": gap.reason,
            "attempted_tiers": ["T0"],
            "prov": cx.prov_with_confidence(&gap.node, ConfidenceTier::Gap, &format!("Gap {id}")),
        }),
    });
    out.edges.push(Edge {
        src: handler_id.to_string(),
        dst: id.clone(),
        label: "DEPENDS_ON".into(),
        props: serde_json::json!({
            "reason": gap.reason,
            "prov": cx.prov_with_confidence(
                &gap.node,
                ConfidenceTier::Gap,
                &format!("DEPENDS_ON {id}"),
            ),
        }),
    });
}
