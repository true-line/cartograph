//! JAX-RS resource endpoints (#1257, AC-0428): a class with an import-proven
//! `@Path` is a root resource, and each of its methods with a proven HTTP
//! method annotation (`@GET`, `@POST`, …) becomes an Endpoint whose route
//! joins the class path with the method's own `@Path`. Both the `jakarta`
//! and the legacy `javax` packages prove an annotation.
//!
//! What T0 cannot place fails closed to an explicit route Gap naming its
//! handler, never a guessed route:
//! - a resource method in a class without a class-level `@Path` (an inherited
//!   base or a sub-resource class);
//! - a resource method declared on an interface, which serves only if some
//!   resource class implements it;
//! - a sub-resource locator (`@Path` with no HTTP method), routed at runtime;
//! - a path that is not a provable string literal.
//!
//! Only public methods are resource methods (Jakarta REST 3.1 §3.3.1), and a
//! method of an anonymous class is never a direct method of the resource that
//! encloses it. A simple annotation name is unproven when the file declares a
//! type of that name, which shadows any import (JLS §6.4.1); an endpoint proven
//! only by a wildcard import waits for the directory join, where a same-package
//! type of that name anywhere in the repository turns it into a route Gap.

use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use std::collections::BTreeSet;
use tree_sitter::Node as TsNode;

use super::{FileCx, Imports, annotation_path, declaration_annotations};

const PACKAGES: [&str; 2] = ["jakarta.ws.rs", "javax.ws.rs"];

/// The framework name endpoints and Gaps carry.
const FRAMEWORK: &str = "jax-rs";

fn verb(simple: &str) -> Option<&'static str> {
    match simple {
        "GET" => Some("GET"),
        "POST" => Some("POST"),
        "PUT" => Some("PUT"),
        "PATCH" => Some("PATCH"),
        "DELETE" => Some("DELETE"),
        "HEAD" => Some("HEAD"),
        "OPTIONS" => Some("OPTIONS"),
        _ => None,
    }
}

/// How an annotation is proven to be the JAX-RS one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proof {
    /// A fully qualified use or a named import: nothing can shadow it.
    Direct,
    /// Only a wildcard import, which a same-package type of the same simple
    /// name shadows (JLS §6.4.1); settled at the directory join.
    Wildcard,
}

/// Proven by a fully qualified use, a named import of exactly
/// `<package>.<Name>`, or a wildcard of exactly that package, for either
/// JAX-RS package. A same-named annotation from anywhere else, or a simple
/// name shadowed by a type this file declares, proves nothing.
fn proof(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    simple: &str,
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> Option<Proof> {
    let spelled = annotation
        .child_by_field_name("name")
        .map(|name| cx.text(&name).to_string())
        .unwrap_or_default();
    if PACKAGES
        .iter()
        .any(|package| spelled == format!("{package}.{simple}"))
    {
        return Some(Proof::Direct);
    }
    if spelled != simple || declared.contains(simple) {
        return None;
    }
    if let Some(fqn) = imports.bindings.get(simple) {
        return PACKAGES
            .iter()
            .any(|package| fqn == &format!("{package}.{simple}"))
            .then_some(Proof::Direct);
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| PACKAGES.contains(&wildcard.as_str()))
        .then_some(Proof::Wildcard)
}

/// The proven `@Path` among `annotations`, if any, with how it is proven.
fn path_annotation<'t>(
    cx: &FileCx<'_>,
    annotations: &[(String, TsNode<'t>)],
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> Option<(TsNode<'t>, Proof)> {
    annotations.iter().find_map(|(name, node)| {
        (name == "Path")
            .then(|| proof(cx, *node, name, imports, declared))
            .flatten()
            .map(|proof| (*node, proof))
    })
}

/// Join a resource path with a method path. JAX-RS ignores a leading `/` on
/// either, so segments meet at exactly one `/` under a leading `/`; a trailing
/// slash spelled on the last segment is kept, as the Spring join keeps it,
/// including a method path that is only `/`.
fn join_route(base: &str, spelled_tail: &str) -> String {
    let base = base.trim_start_matches('/');
    let tail = spelled_tail.trim_start_matches('/');
    if tail.is_empty() {
        if spelled_tail.is_empty() {
            return format!("/{base}");
        }
        let base = base.trim_end_matches('/');
        return if base.is_empty() {
            "/".to_string()
        } else {
            format!("/{base}/")
        };
    }
    if base.trim_end_matches('/').is_empty() {
        return format!("/{tail}");
    }
    format!("/{}/{tail}", base.trim_end_matches('/'))
}

/// The nearest enclosing type declaration of `method`, or `None` when the
/// nearest type is anonymous (an instance creation or enum constant body).
fn enclosing_type(method: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut node = method;
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "object_creation_expression" | "enum_constant"
        ) {
            return None;
        }
        if matches!(
            parent.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) {
            return Some(parent);
        }
        node = parent;
    }
    None
}

/// Whether a method is public: spelled `public` in a class, or any method of
/// an interface that is not declared `private`.
fn is_public(method: TsNode<'_>, owner: TsNode<'_>) -> bool {
    let mut walk = method.walk();
    let keywords: Vec<&str> = method
        .children(&mut walk)
        .filter(|child| child.kind() == "modifiers")
        .flat_map(|modifiers| {
            let mut inner = modifiers.walk();
            modifiers
                .children(&mut inner)
                .map(|keyword| keyword.kind())
                .collect::<Vec<_>>()
        })
        .collect();
    if keywords.contains(&"public") {
        return true;
    }
    owner.kind() == "interface_declaration"
        && !keywords
            .iter()
            .any(|kind| matches!(*kind, "private" | "protected"))
}

/// Simple names of every type this file declares, at any depth.
fn declared_types(cx: &FileCx<'_>, any: TsNode<'_>) -> BTreeSet<String> {
    let mut root = any;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut names = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
        ) && let Some(name) = node.child_by_field_name("name")
        {
            names.insert(cx.text(&name).to_string());
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    names
}

/// A route Gap and its `HANDLES` edge, citing `evidence` and anchored on its
/// last node.
fn route_gap(
    cx: &FileCx<'_>,
    evidence: &[TsNode<'_>],
    handler: &str,
    http_method: Option<&str>,
    reason: &str,
) -> (Node, Edge) {
    let anchor = evidence.last().expect("a Gap cites at least one node");
    let gap_id = format!(
        "gap:route:{}@{}@{}",
        cx.id.repo,
        cx.path,
        anchor.start_byte()
    );
    let node = Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "method": http_method,
            "handler_sym": handler,
            "framework": FRAMEWORK,
            "language": "java",
            "reason": reason,
            "attempted_tiers": ["T0"],
            "prov": cx.prov_spanning(evidence, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
        }),
    };
    let edge = Edge {
        src: gap_id.clone(),
        dst: handler.to_string(),
        label: "HANDLES".into(),
        props: serde_json::json!({
            "reason": reason,
            "attempted_resolution": "literal-path",
            "prov": cx.prov_spanning(
                evidence,
                ConfidenceTier::Gap,
                &format!("HANDLES {gap_id} -> {handler}"),
            ),
        }),
    };
    (node, edge)
}

/// An Endpoint whose annotations are proven only by a wildcard import. The
/// directory join emits `endpoint` unless the repository declares a type
/// named in `shadowed_by`, in which case it emits `gap` instead.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    /// `<file package>.<Name>` for each wildcard-proven annotation.
    shadowed_by: Vec<String>,
    endpoint: (Node, Edge),
    gap: (Node, Edge),
}

impl Pending {
    /// Spring endpoints proven only by a wildcard import settle the same way.
    pub(crate) fn new(shadowed_by: Vec<String>, endpoint: (Node, Edge), gap: (Node, Edge)) -> Self {
        Self {
            shadowed_by,
            endpoint,
            gap,
        }
    }
    /// Apply `retarget` to the props of all four held facts, so a cached
    /// file's deferred evidence follows the commit being extracted.
    pub(crate) fn retarget(&mut self, mut retarget: impl FnMut(&mut serde_json::Value)) {
        retarget(&mut self.endpoint.0.props);
        retarget(&mut self.endpoint.1.props);
        retarget(&mut self.gap.0.props);
        retarget(&mut self.gap.1.props);
    }
}

/// Settle wildcard-proven endpoints against every type the repository
/// declares (`declared` takes a fully qualified name). A shadowing type in
/// another module of the same package is still a Gap, never a silent pick.
pub(crate) fn resolve(
    pending: Vec<Pending>,
    declared: impl Fn(&str) -> bool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mut gaps = BTreeSet::new();
    for pending in pending {
        let (node, edge) = if pending.shadowed_by.iter().any(|fqn| declared(fqn)) {
            if !gaps.insert(pending.gap.0.id.clone()) {
                continue;
            }
            pending.gap
        } else {
            pending.endpoint
        };
        nodes.push(node);
        edges.push(edge);
    }
}

/// `<package>.<Name>` for each top-level type `root` declares, of any kind.
pub(crate) fn top_level_types(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
) -> Vec<String> {
    let mut walk = root.walk();
    root.named_children(&mut walk)
        .filter(|node| {
            matches!(
                node.kind(),
                "class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "record_declaration"
                    | "annotation_type_declaration"
            )
        })
        .filter_map(|node| node.child_by_field_name("name"))
        .map(|name| match package {
            Some(package) => format!("{package}.{}", cx.text(&name)),
            None => cx.text(&name).to_string(),
        })
        .collect()
}

/// Endpoints, route Gaps and their `HANDLES` edges for this file's JAX-RS
/// resource methods, plus the endpoints that wait on the directory join.
pub(crate) fn endpoints(
    cx: &FileCx<'_>,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    imports: &Imports,
    package: Option<&str>,
) -> (Vec<Node>, Vec<Edge>, Vec<Pending>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut pending = Vec::new();
    let mut gap =
        |evidence: &[TsNode<'_>], handler: &str, http_method: Option<&str>, reason: &str| {
            let (node, edge) = route_gap(cx, evidence, handler, http_method, reason);
            nodes.push(node);
            edges.push(edge);
        };
    let declared = methods
        .first()
        .map(|(method, ..)| declared_types(cx, *method))
        .unwrap_or_default();
    let mut endpoints = Vec::new();
    for (method, handler, _, _) in methods {
        if method.kind() != "method_declaration" {
            continue;
        }
        let annotations = declaration_annotations(cx, *method);
        let http_method = annotations.iter().find_map(|(name, node)| {
            let verb = verb(name)?;
            let proof = proof(cx, *node, name, imports, &declared)?;
            Some((verb, *node, proof))
        });
        let method_path = path_annotation(cx, &annotations, imports, &declared);
        if http_method.is_none() && method_path.is_none() {
            continue;
        }
        let Some(owner) = enclosing_type(*method) else {
            continue;
        };
        if !is_public(*method, owner) {
            continue;
        }
        let owner_path =
            path_annotation(cx, &declaration_annotations(cx, owner), imports, &declared);
        let mut evidence: Vec<TsNode<'_>> = owner_path.map(|(node, _)| node).into_iter().collect();
        evidence.extend(method_path.map(|(node, _)| node));
        evidence.extend(http_method.map(|(_, node, _)| node));
        let Some((http_method, verb_node, verb_proof)) = http_method else {
            // A sub-resource locator returns the object that routes the rest
            // of the request; only a resource class's locator is in scope.
            if owner_path.is_some() {
                gap(
                    &evidence,
                    handler,
                    None,
                    "JAX-RS sub-resource locator: routes are resolved at runtime",
                );
            }
            continue;
        };
        if owner.kind() == "interface_declaration" {
            gap(
                &evidence,
                handler,
                Some(http_method),
                "JAX-RS resource method on an interface: served only by an implementing resource class",
            );
            continue;
        }
        let Some((owner_path, owner_proof)) = owner_path else {
            gap(
                &evidence,
                handler,
                Some(http_method),
                "JAX-RS resource method without a class-level @Path (inherited or sub-resource)",
            );
            continue;
        };
        let (Some(bases), Some(tails)) = (
            annotation_path(cx, owner_path).segments(),
            method_path.map_or(Some(vec![String::new()]), |(node, _)| {
                annotation_path(cx, node).segments()
            }),
        ) else {
            gap(&evidence, handler, Some(http_method), "dynamic JAX-RS path");
            continue;
        };
        let simple = |node: TsNode<'_>| {
            let name = node
                .child_by_field_name("name")
                .map(|name| cx.text(&name))
                .unwrap_or_default();
            name.rsplit('.').next().unwrap_or(name).to_string()
        };
        let wildcard: BTreeSet<String> = [
            Some((verb_node, verb_proof)),
            Some((owner_path, owner_proof)),
            method_path,
        ]
        .into_iter()
        .flatten()
        .filter(|(_, proof)| *proof == Proof::Wildcard)
        .map(|(node, _)| match package {
            Some(package) => format!("{package}.{}", simple(node)),
            None => simple(node),
        })
        .collect();
        // Sorted and de-duplicated so emission never depends on source order.
        let routes: BTreeSet<String> = bases
            .iter()
            .flat_map(|base| tails.iter().map(move |tail| join_route(base, tail)))
            .collect();
        for route in routes {
            endpoints.push((
                route,
                http_method,
                handler.clone(),
                evidence.clone(),
                wildcard.clone(),
            ));
        }
    }
    for (route, http_method, handler, evidence, wildcard) in endpoints {
        let endpoint = format!("ep:{}@{http_method}:{route}", cx.id.repo);
        let node = Node {
            id: endpoint.clone(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": http_method,
                "path": route,
                "handler_sym": handler,
                "framework": FRAMEWORK,
                "language": "java",
                "prov": cx.prov_spanning(
                    &evidence,
                    ConfidenceTier::Confirmed,
                    &format!("Endpoint {endpoint}"),
                ),
            }),
        };
        let edge = Edge {
            src: endpoint.clone(),
            dst: handler.clone(),
            label: "HANDLES".into(),
            props: serde_json::json!({
                "prov": cx.prov_spanning(
                    &evidence,
                    ConfidenceTier::Confirmed,
                    &format!("HANDLES {endpoint} -> {handler}"),
                ),
            }),
        };
        if wildcard.is_empty() {
            nodes.push(node);
            edges.push(edge);
        } else {
            pending.push(Pending {
                shadowed_by: wildcard.into_iter().collect(),
                endpoint: (node, edge),
                gap: route_gap(
                    cx,
                    &evidence,
                    &handler,
                    Some(http_method),
                    "JAX-RS annotation proven only by a wildcard import is shadowed by a same-package type",
                ),
            });
        }
    }
    (nodes, edges, pending)
}
