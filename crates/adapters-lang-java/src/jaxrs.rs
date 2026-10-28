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
//! type of that name, which shadows any import (JLS §6.4.1); a same-package
//! type shadowing a wildcard import is outside this per-file view.

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

/// Proven by a fully qualified use, a named import of exactly
/// `<package>.<Name>`, or a wildcard of exactly that package, for either
/// JAX-RS package. A same-named annotation from anywhere else proves nothing.
fn proven(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    simple: &str,
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> bool {
    let spelled = annotation
        .child_by_field_name("name")
        .map(|name| cx.text(&name).to_string())
        .unwrap_or_default();
    if PACKAGES
        .iter()
        .any(|package| spelled == format!("{package}.{simple}"))
    {
        return true;
    }
    if spelled != simple || declared.contains(simple) {
        return false;
    }
    if let Some(fqn) = imports.bindings.get(simple) {
        return PACKAGES
            .iter()
            .any(|package| fqn == &format!("{package}.{simple}"));
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| PACKAGES.contains(&wildcard.as_str()))
}

/// The proven `@Path` among `annotations`, if any.
fn path_annotation<'t>(
    cx: &FileCx<'_>,
    annotations: &[(String, TsNode<'t>)],
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> Option<TsNode<'t>> {
    annotations
        .iter()
        .find(|(name, node)| name == "Path" && proven(cx, *node, name, imports, declared))
        .map(|(_, node)| *node)
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

/// Endpoints, route Gaps and their `HANDLES` edges for this file's JAX-RS
/// resource methods.
pub(crate) fn endpoints(
    cx: &FileCx<'_>,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    imports: &Imports,
) -> (Vec<Node>, Vec<Edge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut gap =
        |evidence: &[TsNode<'_>], handler: &str, http_method: Option<&str>, reason: &str| {
            let anchor = evidence.last().expect("a Gap cites at least one node");
            let gap_id = format!(
                "gap:route:{}@{}@{}",
                cx.id.repo,
                cx.path,
                anchor.start_byte()
            );
            nodes.push(Node {
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
        });
            edges.push(Edge {
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
            });
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
            verb(name)
                .filter(|_| proven(cx, *node, name, imports, &declared))
                .map(|verb| (verb, *node))
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
        let mut evidence: Vec<TsNode<'_>> = owner_path.into_iter().collect();
        evidence.extend(method_path);
        evidence.extend(http_method.map(|(_, node)| node));
        let Some((http_method, _)) = http_method else {
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
        let Some(owner_path) = owner_path else {
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
            method_path.map_or(Some(vec![String::new()]), |node| {
                annotation_path(cx, node).segments()
            }),
        ) else {
            gap(&evidence, handler, Some(http_method), "dynamic JAX-RS path");
            continue;
        };
        // Sorted and de-duplicated so emission never depends on source order.
        let routes: BTreeSet<String> = bases
            .iter()
            .flat_map(|base| tails.iter().map(move |tail| join_route(base, tail)))
            .collect();
        for route in routes {
            endpoints.push((route, http_method, handler.clone(), evidence.clone()));
        }
    }
    for (route, http_method, handler, evidence) in endpoints {
        let endpoint = format!("ep:{}@{http_method}:{route}", cx.id.repo);
        nodes.push(Node {
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
        });
        edges.push(Edge {
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
        });
    }
    (nodes, edges)
}
