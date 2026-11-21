//! Ktor server routing DSL (#2021, AC-1023).
//!
//! Inside a `routing { }` block, or in the body of a `Route` (or `Routing`)
//! extension function, the verb builders `get`, `post`, `put`, `delete`,
//! `patch`, `head` and `options` declare an endpoint whose handler is the
//! trailing lambda, and `route("/prefix") { }` prefixes everything inside
//! it. Each name is proven by a named import from `io.ktor.server.routing`
//! (or Ktor 1's `io.ktor.routing`) or by a star import of that package,
//! unless the file declares a top-level function of that name. A star
//! proof also fails closed when this repository declares a same-package
//! function of that name, since Kotlin resolves the package before star
//! imports.
//!
//! A handler lambda becomes a `Symbol` (`…#<owner>/<verb>@<offset>`, kind
//! `route handler`), and the calls inside it are its own. The path is the
//! one positional argument, a pure string literal; any other argument
//! makes the route a Gap (R-INT-4). A zero-argument call of a bare name
//! inside routing mounts the `Route` extension of that name, so a group
//! declared in one file and mounted in another composes its prefix at the
//! directory join; a group no routing block mounts, one whose name two
//! files declare, or one reached only through a computed prefix is a Gap,
//! never a guessed path.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

use super::rest_template::named_children;
use super::{FileCx, Imports, extension_receiver, join_route, pure_string_literal, symbol_id};

const PACKAGES: &[&str] = &["io.ktor.server.routing", "io.ktor.routing"];
const FRAMEWORK: &str = "ktor";
const EXTRACTOR_ID: &str = super::EXTRACTOR_ID;

/// How a routing-DSL name is proven in one file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameProof {
    Named,
    Star,
}

/// One verb builder, before the directory join composes its mount prefix.
#[derive(Debug, Clone)]
pub(crate) struct Route {
    /// The file that declares it.
    path: String,
    package: Option<String>,
    /// DSL names this route's proof takes from a star import only.
    star: BTreeSet<String>,
    /// `None` under a `routing { }` block; the extension function's name
    /// when the route is declared in a `Route` extension.
    group: Option<String>,
    /// The path within the routing block or group, `None` when computed.
    route: Option<String>,
    verb: &'static str,
    handler: String,
    evidence: EvidenceRef,
}

/// A zero-argument call of a bare name inside routing: a candidate mount
/// of the `Route` extension of that name.
#[derive(Debug, Clone)]
pub(crate) struct Mount {
    name: String,
    /// The group the mount sits in, `None` under a `routing { }` block.
    group: Option<String>,
    prefix: Option<String>,
}

/// One file's routing facts awaiting the directory join.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    routes: Vec<Route>,
    mounts: Vec<Mount>,
    /// `Route` extension functions this file declares, by name.
    groups: Vec<String>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.routes.extend(other.routes);
        self.mounts.extend(other.mounts);
        self.groups.extend(other.groups);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for route in &mut self.routes {
            route.evidence.commit_sha = commit.into();
        }
    }
}

fn verb(name: &str) -> Option<&'static str> {
    Some(match name {
        "get" => "GET",
        "post" => "POST",
        "put" => "PUT",
        "delete" => "DELETE",
        "patch" => "PATCH",
        "head" => "HEAD",
        "options" => "OPTIONS",
        _ => return None,
    })
}

/// How `name` resolves to the routing package in this file, if it does.
fn proof(
    imports: &Imports,
    declared: &std::collections::HashSet<String>,
    name: &str,
) -> Option<NameProof> {
    if declared.contains(name) {
        return None;
    }
    if let Some(target) = imports.bindings.get(name) {
        return PACKAGES
            .iter()
            .any(|package| {
                target
                    .strip_prefix(package)
                    .and_then(|rest| rest.strip_prefix('.'))
                    .is_some_and(|rest| !rest.contains('.'))
            })
            .then_some(NameProof::Named);
    }
    imports
        .wildcard_packages
        .iter()
        .any(|package| PACKAGES.contains(&package.as_str()))
        .then_some(NameProof::Star)
}

/// The name, positional arguments and trailing lambda of a DSL call:
/// `name { }`, `name(args) { }` or `name(args)`.
fn dsl_call<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
) -> Option<(String, Option<Vec<TsNode<'t>>>, Option<TsNode<'t>>)> {
    let children = named_children(call);
    match children[..] {
        [callee, lambda] if lambda.kind() == "annotated_lambda" => match callee.kind() {
            "identifier" => Some((cx.text(&callee).to_string(), Some(Vec::new()), Some(lambda))),
            "call_expression" => {
                let [name, arguments] = named_children(callee)[..] else {
                    return None;
                };
                (name.kind() == "identifier" && arguments.kind() == "value_arguments").then(|| {
                    (
                        cx.text(&name).to_string(),
                        positional(arguments),
                        Some(lambda),
                    )
                })
            }
            _ => None,
        },
        [name, arguments]
            if name.kind() == "identifier" && arguments.kind() == "value_arguments" =>
        {
            Some((cx.text(&name).to_string(), positional(arguments), None))
        }
        _ => None,
    }
}

/// The argument expressions, or `None` when any argument is named or
/// spread (`path = …`, `*parts`).
fn positional(arguments: TsNode<'_>) -> Option<Vec<TsNode<'_>>> {
    let mut values = Vec::new();
    for argument in named_children(arguments) {
        let parts = named_children(argument);
        let [value] = parts[..] else {
            return None;
        };
        values.push(value);
    }
    Some(values)
}

/// The path a DSL call's arguments designate: `""` with none, the literal
/// with one pure string literal, `None` otherwise.
fn path_argument(cx: &FileCx<'_>, arguments: &Option<Vec<TsNode<'_>>>) -> Option<String> {
    match arguments.as_deref()? {
        [] => Some(String::new()),
        [value] => pure_string_literal(cx, *value),
        _ => None,
    }
}

#[derive(Clone)]
struct Scope {
    group: Option<String>,
    prefix: Option<String>,
    star: BTreeSet<String>,
}

/// This file's routing facts, and the handler lambdas by start byte (their
/// calls are their own).
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> (Pending, HashMap<usize, String>) {
    let mut pending = Pending::default();
    let mut handlers = HashMap::new();
    if !cx.text(&root).contains("ktor") {
        return (pending, handlers);
    }
    let declared = super::rest_template::declared_functions(cx, root);
    // Work items: a node and the routing scope it is in, if any.
    let mut stack: Vec<(TsNode<'_>, Option<Scope>)> = vec![(root, None)];
    while let Some((node, scope)) = stack.pop() {
        if node.kind() == "function_declaration"
            && let Some(name_node) = node.child_by_field_name("name")
            && let Some(receiver) = extension_receiver(cx, node, name_node)
            && matches!(receiver.as_str(), "Route" | "Routing")
            && let Some(receiver_proof) = proof(imports, &declared, &receiver)
        {
            let name = cx.text(&name_node).to_string();
            pending.groups.push(name.clone());
            let mut star = BTreeSet::new();
            if receiver_proof == NameProof::Star {
                star.insert(receiver);
            }
            let scope = Scope {
                group: Some(name),
                prefix: Some(String::new()),
                star,
            };
            for child in named_children(node) {
                stack.push((child, Some(scope.clone())));
            }
            continue;
        }
        if node.kind() == "call_expression"
            && let Some((name, arguments, lambda)) = dsl_call(cx, node)
        {
            let named = proof(imports, &declared, &name);
            match (name.as_str(), lambda, &scope, named) {
                ("routing", Some(lambda), _, Some(named)) => {
                    let mut star = BTreeSet::new();
                    if named == NameProof::Star {
                        star.insert(name.clone());
                    }
                    let inner = Scope {
                        group: None,
                        prefix: Some(String::new()),
                        star,
                    };
                    stack.push((lambda, Some(inner)));
                    continue;
                }
                ("route", Some(lambda), Some(outer), Some(named)) => {
                    let mut inner = outer.clone();
                    if named == NameProof::Star {
                        inner.star.insert(name.clone());
                    }
                    inner.prefix = match (&outer.prefix, path_argument(cx, &arguments)) {
                        (Some(base), Some(tail)) => Some(join_route(base, &tail)),
                        _ => None,
                    };
                    stack.push((lambda, Some(inner)));
                    continue;
                }
                (_, Some(lambda), Some(outer), Some(named)) if verb(&name).is_some() => {
                    let http_method = verb(&name).expect("checked");
                    let owner = owner_name(cx, node, functions_by_start);
                    let handler = symbol_id(
                        cx.id.repo,
                        cx.path,
                        &format!("{owner}/{name}@{}", lambda.start_byte()),
                    );
                    handlers.insert(lambda.start_byte(), handler.clone());
                    nodes.push(Node {
                        id: handler.clone(),
                        label: "Symbol".into(),
                        props: serde_json::json!({
                            "name": format!("{owner}/{name}@{}", lambda.start_byte()),
                            "kind": "route handler",
                            "language": "kotlin",
                            "prov": cx.prov(&lambda, &format!("Symbol {handler}")),
                        }),
                    });
                    edges.push(Edge {
                        src: handler.clone(),
                        dst: super::file_id(cx.id.repo, cx.path),
                        label: "DEFINED_IN".into(),
                        props: serde_json::json!({
                            "prov": cx.prov(&lambda, &format!("DEFINED_IN {handler}")),
                        }),
                    });
                    let mut star = outer.star.clone();
                    if named == NameProof::Star {
                        star.insert(name.clone());
                    }
                    let route = match (&outer.prefix, path_argument(cx, &arguments)) {
                        (Some(base), Some(tail)) => Some(join_route(base, &tail)),
                        _ => None,
                    };
                    pending.routes.push(Route {
                        path: cx.path.to_string(),
                        package: package.map(str::to_string),
                        star,
                        group: outer.group.clone(),
                        route,
                        verb: http_method,
                        handler,
                        evidence: EvidenceRef {
                            repo: cx.id.repo.into(),
                            path: cx.path.into(),
                            byte_start: node.start_byte() as u64,
                            byte_end: node.end_byte() as u64,
                            commit_sha: cx.id.commit.into(),
                        },
                    });
                    // The handler is not routing: a verb inside it is not
                    // a route.
                    continue;
                }
                (_, None, Some(outer), _)
                    if arguments.as_ref().is_some_and(Vec::is_empty)
                        && verb(&name).is_none()
                        && !matches!(name.as_str(), "routing" | "route") =>
                {
                    pending.mounts.push(Mount {
                        name,
                        group: outer.group.clone(),
                        prefix: outer.prefix.clone(),
                    });
                    continue;
                }
                // An unproven `route` may still prefix what it wraps, so
                // nothing inside it has a known path.
                ("route", Some(lambda), Some(outer), None) => {
                    let mut inner = outer.clone();
                    inner.prefix = None;
                    stack.push((lambda, Some(inner)));
                    continue;
                }
                _ => {}
            }
        }
        for child in named_children(node) {
            stack.push((child, scope.clone()));
        }
    }
    (pending, handlers)
}

/// The qualified name of the function that encloses `node`, or `file`.
fn owner_name(cx: &FileCx<'_>, node: TsNode<'_>, by_start: &HashMap<usize, String>) -> String {
    let prefix = format!("sym:{}@{}#", cx.id.repo, cx.path);
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "function_declaration"
            && let Some(symbol) = by_start.get(&parent.start_byte())
        {
            return symbol.strip_prefix(&prefix).unwrap_or(symbol).to_string();
        }
        current = parent;
    }
    "file".into()
}

/// The prefixes a group is mounted under across the repository: `None`
/// for a computed one. Cycles and depth beyond 16 yield nothing.
fn mounted(
    group: &str,
    mounts: &[Mount],
    unique: &BTreeMap<&str, bool>,
    depth: usize,
    seen: &mut Vec<String>,
) -> Vec<Option<String>> {
    if depth > 16 || seen.iter().any(|name| name == group) {
        return Vec::new();
    }
    seen.push(group.to_string());
    let mut prefixes = Vec::new();
    for mount in mounts.iter().filter(|mount| mount.name == group) {
        match &mount.group {
            None => prefixes.push(mount.prefix.clone()),
            Some(outer) if unique.get(outer.as_str()) == Some(&true) => {
                for base in mounted(outer, mounts, unique, depth + 1, seen) {
                    prefixes.push(match (base, &mount.prefix) {
                        (Some(base), Some(tail)) => Some(join_route(&base, tail)),
                        _ => None,
                    });
                }
            }
            Some(_) => prefixes.push(None),
        }
    }
    seen.pop();
    prefixes
}

fn prov(evidence: &EvidenceRef, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![evidence.clone()],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn gap(repo: &str, route: &Route, reason: &str) -> (Node, Edge) {
    let gap_id = format!(
        "gap:route:{repo}@{}@{}",
        route.path, route.evidence.byte_start
    );
    let handler = &route.handler;
    (
        Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "method": route.verb,
                "handler_sym": handler,
                "framework": FRAMEWORK,
                "language": "kotlin",
                "reason": reason,
                "attempted_tiers": ["T0"],
                "prov": prov(&route.evidence, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
            }),
        },
        Edge {
            src: gap_id.clone(),
            dst: handler.clone(),
            label: "HANDLES".into(),
            props: serde_json::json!({
                "reason": reason,
                "attempted_resolution": "literal-path",
                "prov": prov(
                    &route.evidence,
                    ConfidenceTier::Gap,
                    &format!("HANDLES {gap_id} -> {handler}"),
                ),
            }),
        },
    )
}

/// Endpoints and Gaps for every pending route. `declared_function(fqn)`
/// says whether this repository declares a top-level function of that
/// fully qualified name, which obscures a star-imported DSL name.
pub(crate) fn resolve(
    repo: &str,
    pending: Pending,
    declared_function: impl Fn(&str) -> bool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mut unique: BTreeMap<&str, bool> = BTreeMap::new();
    for group in &pending.groups {
        unique
            .entry(group.as_str())
            .and_modify(|once| *once = false)
            .or_insert(true);
    }
    let mut emitted_gaps = BTreeSet::new();
    let mut emitted = BTreeSet::new();
    for route in &pending.routes {
        let shadowed = route.star.iter().any(|name| {
            declared_function(&match &route.package {
                Some(package) => format!("{package}.{name}"),
                None => name.clone(),
            })
        });
        let prefixes: Result<Vec<Option<String>>, &str> = if shadowed {
            Err("Ktor routing name obscured by a same-package declaration")
        } else {
            match &route.group {
                None => Ok(vec![Some(String::new())]),
                Some(group) if unique.get(group.as_str()) != Some(&true) => {
                    Err("Ktor route group declared more than once")
                }
                Some(group) => {
                    let found = mounted(group, &pending.mounts, &unique, 0, &mut Vec::new());
                    if found.is_empty() {
                        Err("Ktor route group not mounted in any routing block")
                    } else {
                        Ok(found)
                    }
                }
            }
        };
        let prefixes = match prefixes {
            Ok(prefixes) => prefixes,
            Err(reason) => {
                let (node, edge) = gap(repo, route, reason);
                if emitted_gaps.insert(node.id.clone()) {
                    nodes.push(node);
                    edges.push(edge);
                }
                continue;
            }
        };
        let mut computed = false;
        for prefix in prefixes {
            let (Some(prefix), Some(tail)) = (prefix, route.route.as_ref()) else {
                computed = true;
                continue;
            };
            let path = join_route(&prefix, tail);
            let endpoint = format!("ep:{repo}@{}:{path}", route.verb);
            if !emitted.insert((endpoint.clone(), route.handler.clone())) {
                continue;
            }
            nodes.push(Node {
                id: endpoint.clone(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": route.verb,
                    "path": path,
                    "handler_sym": route.handler,
                    "framework": FRAMEWORK,
                    "language": "kotlin",
                    "prov": prov(
                        &route.evidence,
                        ConfidenceTier::Confirmed,
                        &format!("Endpoint {endpoint}"),
                    ),
                }),
            });
            edges.push(Edge {
                src: endpoint.clone(),
                dst: route.handler.clone(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": prov(
                        &route.evidence,
                        ConfidenceTier::Confirmed,
                        &format!("HANDLES {endpoint} -> {}", route.handler),
                    ),
                }),
            });
        }
        if computed {
            let (node, edge) = gap(repo, route, "dynamic Ktor route path");
            if emitted_gaps.insert(node.id.clone()) {
                nodes.push(node);
                edges.push(edge);
            }
        }
    }
}
