//! Browser service-worker proof and readable callback names (#1078, AC-0365).
//!
//! A file is a service worker only on deterministic proof: it calls
//! `self.addEventListener('install' | 'activate' | 'fetch', …)` with `self`
//! the unshadowed global, or some file of the same repository passes its
//! path as the literal argument of `navigator.serviceWorker.register(…)`.
//! The proof is a `runtime: "service-worker"` prop on the File node and on
//! every Symbol or Component defined in it, so the Atlas can place browser
//! code on the client side instead of under Server.
//!
//! Anonymous callables keep their `anon@<byte>` ids; `display_name` only
//! gives them a label derived from the call they are passed to.

use crate::{FileCx, TsNode, callable};
use core_graph::{Edge, Node};
use std::collections::BTreeSet;

/// The `runtime` prop value that marks a proven service-worker file.
pub(crate) const RUNTIME: &str = "service-worker";

/// Service-worker lifecycle events whose listener on `self` proves a worker.
const LIFECYCLE_EVENTS: [&str; 3] = ["install", "activate", "fetch"];

/// The call's argument nodes, in order.
fn arguments<'tree>(call: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = args.walk();
    args.named_children(&mut walk).collect()
}

/// A plain string literal's text; escapes, templates and concatenations are
/// not literals here.
fn plain_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut walk = node.walk();
    let mut fragments = node.named_children(&mut walk);
    let fragment = fragments.next()?;
    (fragment.kind() == "string_fragment" && fragments.next().is_none())
        .then(|| cx.text(&fragment).to_string())
}

/// Whether the file calls `self.addEventListener(<lifecycle event>, …)` with
/// `self` the unshadowed global.
pub(crate) fn proves_worker(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    bindings: &callable::Bindings<'_>,
) -> bool {
    nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "call_expression")
        .any(|call| {
            let Some(callee) = call.child_by_field_name("function") else {
                return false;
            };
            let (Some(object), Some(property)) = (
                callee.child_by_field_name("object"),
                callee.child_by_field_name("property"),
            ) else {
                return false;
            };
            callee.kind() == "member_expression"
                && object.kind() == "identifier"
                && cx.text(&object) == "self"
                && cx.text(&property) == "addEventListener"
                && arguments(call)
                    .first()
                    .and_then(|event| plain_string(cx, *event))
                    .is_some_and(|event| LIFECYCLE_EVENTS.contains(&event.as_str()))
                && bindings.global_unshadowed(call, "self")
        })
}

/// Literal paths passed to `navigator.serviceWorker.register(…)` with
/// `navigator` the unshadowed global.
pub(crate) fn registered_paths(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    bindings: &callable::Bindings<'_>,
) -> Vec<String> {
    nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "call_expression")
        .filter_map(|call| {
            let callee = call.child_by_field_name("function")?;
            (callee.kind() == "member_expression"
                && cx.text(&callee) == "navigator.serviceWorker.register"
                && bindings.global_unshadowed(call, "navigator"))
            .then(|| plain_string(cx, *arguments(call).first()?))
            .flatten()
        })
        .collect()
}

/// The repository file a registration literal names, only when exactly one
/// file can be it. `/sw.js` is relative to the site root, which the
/// repository does not state, so the literal matches a file whose path equals
/// it or ends with `/<literal>` (the root sits in a subdirectory such as
/// `web/`). A relative, escaping, query or scheme-carrying literal, or a
/// literal that fits several files, proves nothing.
fn resolve<'a>(literal: &str, files: &'a BTreeSet<String>) -> Option<&'a str> {
    let path = literal.strip_prefix('/').unwrap_or(literal);
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty()
        || path.contains(['?', '#', ':', '\\'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    let suffix = format!("/{path}");
    let mut candidates = files
        .iter()
        .filter(|file| file.as_str() == path || file.ends_with(&suffix));
    let found = candidates.next()?;
    candidates.next().is_none().then_some(found.as_str())
}

/// Mark each registered worker file and everything defined in it.
pub(crate) fn mark_registered(
    literals: &[String],
    nodes: &mut [Node],
    edges: &[Edge],
    file_prefix: &str,
) {
    let files: BTreeSet<String> = nodes
        .iter()
        .filter(|node| node.label == "File")
        .filter_map(|node| node.props["path"].as_str().map(str::to_string))
        .collect();
    let workers: BTreeSet<String> = literals
        .iter()
        .filter_map(|literal| resolve(literal, &files))
        .map(|path| format!("{file_prefix}{path}"))
        .collect();
    mark(&workers, nodes, edges);
}

/// Mark `runtime` on the File nodes named by `files` and on the Symbols and
/// Components DEFINED_IN them.
pub(crate) fn mark(files: &BTreeSet<String>, nodes: &mut [Node], edges: &[Edge]) {
    if files.is_empty() {
        return;
    }
    let defined: BTreeSet<&str> = edges
        .iter()
        .filter(|edge| edge.label == "DEFINED_IN" && files.contains(&edge.dst))
        .map(|edge| edge.src.as_str())
        .collect();
    for node in nodes {
        let marked = match node.label.as_str() {
            "File" => files.contains(&node.id),
            "Symbol" | "Component" => defined.contains(node.id.as_str()),
            _ => false,
        };
        if marked {
            node.props["runtime"] = serde_json::json!(RUNTIME);
        }
    }
}

/// Last path segment, for display names.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A call target shortened to its member chain, `fetch().then`: arguments
/// become `()`. `None` for any shape beyond identifiers, `this`, member
/// access and calls, so nothing is guessed.
fn abbreviate(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "identifier" | "property_identifier" | "this" => Some(cx.text(&node).to_string()),
        "member_expression" => Some(format!(
            "{}.{}",
            abbreviate(cx, node.child_by_field_name("object")?)?,
            abbreviate(cx, node.child_by_field_name("property")?)?
        )),
        "call_expression" => Some(format!(
            "{}()",
            abbreviate(cx, node.child_by_field_name("function")?)?
        )),
        _ => None,
    }
}

/// A label for an anonymous callable passed as an argument of a method call:
/// `<file> · <event> listener` for `X.addEventListener('<event>', fn)`, else
/// `<file> · <callee> callback` for `recv.method(fn)`. The symbol id is not
/// touched.
pub(crate) fn display_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let args = function
        .parent()
        .filter(|node| node.kind() == "arguments")?;
    let call = args
        .parent()
        .filter(|node| node.kind() == "call_expression")?;
    let callee = call.child_by_field_name("function")?;
    if callee.kind() != "member_expression" {
        return None;
    }
    let file = basename(cx.path);
    let property = cx.text(&callee.child_by_field_name("property")?);
    let passed = arguments(call);
    if property == "addEventListener"
        && passed.get(1).is_some_and(|arg| arg.id() == function.id())
        && let Some(event) = passed.first().and_then(|arg| plain_string(cx, *arg))
    {
        return Some(format!("{file} · {event} listener"));
    }
    let callee = abbreviate(cx, callee).filter(|text| text.len() <= 64)?;
    Some(format!("{file} · {callee} callback"))
}
