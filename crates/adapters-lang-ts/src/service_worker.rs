//! Browser service-worker proof and readable callback names (#1078, AC-0365).
//!
//! A file is a service worker only on deterministic proof: it calls
//! `self.addEventListener('install' | 'activate' | 'fetch', …)` with `self`
//! the unshadowed global, or some file of the same repository passes its
//! site-root path (`/sw.js`) as the literal argument of
//! `navigator.serviceWorker.register(…)`. The proof is a
//! `runtime: "service-worker"` prop on the File node and on every Symbol or
//! Component defined in it, so the Atlas can place browser code on the
//! client side instead of under Server. The proving call's span joins each
//! marked node's evidence and its commit-free span joins the content hash.
//!
//! Anonymous callables keep their `anon@<byte>` ids; `display_name` only
//! gives them a label derived from the call they are passed to.

use crate::{FileCx, TsNode, callable};
use core_graph::facts::FactAccess;
use core_graph::{GraphError, Node};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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

/// A `navigator.serviceWorker.register(…)` literal and the call's evidence.
#[derive(Debug, Clone)]
pub(crate) struct Registration {
    literal: String,
    evidence: Value,
}

impl Registration {
    /// Cite `commit`, as a cache reuse at a new commit retargets every fact.
    pub(crate) fn retarget(&mut self, commit: &str) {
        self.evidence["commit_sha"] = serde_json::json!(commit);
    }
}

/// One evidence span for `call`, in the adapter's provenance shape.
fn call_evidence(cx: &FileCx<'_>, call: TsNode<'_>) -> Value {
    cx.prov(&call, "service-worker proof")["evidence"][0].clone()
}

/// The evidence of the file's first `self.addEventListener(<lifecycle
/// event>, …)` call with `self` the unshadowed global, if any.
pub(crate) fn proves_worker(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    bindings: &callable::Bindings<'_>,
) -> Option<Value> {
    nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "call_expression")
        .find(|&call| {
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
        .map(|call| call_evidence(cx, call))
}

/// Literal paths passed to `navigator.serviceWorker.register(…)` with
/// `navigator` the unshadowed global.
pub(crate) fn registered_paths(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    bindings: &callable::Bindings<'_>,
) -> Vec<Registration> {
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
            .map(|literal| Registration {
                literal,
                evidence: call_evidence(cx, call),
            })
        })
        .collect()
}

/// The repository file a registration literal names, only when exactly one
/// file can be it. `/sw.js` is relative to the site root, which the
/// repository does not state, so the literal matches a file whose path equals
/// it or ends with `/<literal>` (the root sits in a subdirectory such as
/// `web/`). A relative literal (`sw.js`, `./sw.js`) resolves against the
/// page's base URL, which the extractor does not know, so it proves nothing;
/// nor does a protocol-relative, escaping, query or scheme-carrying literal,
/// or one that fits several files.
fn resolve<'a>(literal: &str, files: &'a BTreeSet<String>) -> Option<&'a str> {
    let path = literal.strip_prefix('/')?;
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

/// Mark each registered worker file and everything defined in it, citing
/// every registration call that names it.
pub(crate) fn mark_registered(
    registrations: &[Registration],
    facts: &mut impl FactAccess,
    file_prefix: &str,
) -> Result<(), GraphError> {
    // Most repositories register no worker; then no File node is decoded.
    if registrations.is_empty() {
        return Ok(());
    }
    let files: BTreeSet<String> = facts
        .decode_nodes(|_, label| label == "File")?
        .iter()
        .filter_map(|node| node.props["path"].as_str().map(str::to_string))
        .collect();
    let mut workers: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for registration in registrations {
        if let Some(path) = resolve(&registration.literal, &files) {
            workers
                .entry(format!("{file_prefix}{path}"))
                .or_default()
                .push(registration.evidence.clone());
        }
    }
    mark(&workers, facts)
}

/// Mark `runtime` on the File nodes keyed in `files` and on the Symbols and
/// Components DEFINED_IN them. Each marked node's provenance gains the
/// proving evidence (sorted, deduplicated) and a content hash that covers
/// the classification and that evidence. Only the marked nodes are decoded.
pub(crate) fn mark(
    files: &BTreeMap<String, Vec<Value>>,
    facts: &mut impl FactAccess,
) -> Result<(), GraphError> {
    if files.is_empty() {
        return Ok(());
    }
    let defined: BTreeMap<String, String> = facts
        .edge_keys()
        .filter(|(_, dst, label)| *label == "DEFINED_IN" && files.contains_key(*dst))
        .map(|(src, dst, _)| (src.to_string(), dst.to_string()))
        .collect();
    let marked = |id: &str, label: &str| match label {
        "File" => files.contains_key(id),
        "Symbol" | "Component" => defined.contains_key(id),
        _ => false,
    };
    facts.edit_nodes(marked, |nodes, _| {
        for node in nodes {
            mark_node(files, &defined, node);
        }
    })
}

fn mark_node(
    files: &BTreeMap<String, Vec<Value>>,
    defined: &BTreeMap<String, String>,
    node: &mut Node,
) {
    let file = match node.label.as_str() {
        "File" => node.id.clone(),
        _ => defined[&node.id].clone(),
    };
    node.props["runtime"] = serde_json::json!(RUNTIME);
    let mut proof = files[&file].clone();
    proof.sort_by_key(Value::to_string);
    proof.dedup();
    let prov = &mut node.props["prov"];
    if let Some(evidence) = prov["evidence"].as_array_mut() {
        for item in &proof {
            if !evidence.contains(item) {
                evidence.push(item.clone());
            }
        }
    }
    if let Some(hash) = prov["content_hash"].as_str() {
        // The commit stays out of the hash, as in the base hash, so a
        // cache reuse at another commit hashes like a fresh extraction.
        let spans: Vec<Value> = proof
            .iter()
            .cloned()
            .map(|mut item| {
                if let Some(item) = item.as_object_mut() {
                    item.remove("commit_sha");
                }
                item
            })
            .collect();
        let cited = serde_json::to_string(&spans).expect("evidence serializes");
        prov["content_hash"] = serde_json::json!(core_prov::content_hash(
            format!("{hash} runtime {RUNTIME} {cited}").as_bytes()
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::Edge;
    use core_graph::facts::{FactLists, FactStore};
    use serde_json::json;

    fn node(id: &str, label: &str, path: Option<&str>) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"path": path, "prov": {"evidence": [{"path": "web/app.js"}], "content_hash": "h"}}),
        }
    }

    // AC-0536: marking over the packed store edits exactly what marking over
    // plain lists does, and no registration leaves every fact untouched.
    #[test]
    fn marking_is_identical_over_lists_and_store() {
        let nodes = vec![
            node("file:r@web/worker.js", "File", Some("web/worker.js")),
            node("sym:boot", "Symbol", None),
            node("file:r@web/app.js", "File", Some("web/app.js")),
            node("sym:main", "Symbol", None),
            node("cmp:card", "Component", None),
        ];
        let edges: Vec<Edge> = [
            ("sym:boot", "file:r@web/worker.js"),
            ("sym:main", "file:r@web/app.js"),
            ("cmp:card", "file:r@web/worker.js"),
        ]
        .into_iter()
        .map(|(src, dst)| Edge {
            src: src.into(),
            dst: dst.into(),
            label: "DEFINED_IN".into(),
            props: json!({}),
        })
        .collect();
        let registrations = [Registration {
            literal: "/worker.js".into(),
            evidence: json!({"path": "web/app.js", "commit_sha": "c"}),
        }];
        for registrations in [&registrations[..], &[]] {
            let (mut listed, mut listed_edges) = (nodes.clone(), edges.clone());
            mark_registered(
                registrations,
                &mut FactLists {
                    nodes: &mut listed,
                    edges: &mut listed_edges,
                },
                "file:r@",
            )
            .unwrap();
            let mut store = FactStore::from_facts(nodes.clone(), edges.clone()).unwrap();
            mark_registered(registrations, &mut store, "file:r@").unwrap();
            assert_eq!(store.into_facts().unwrap(), (listed.clone(), listed_edges));
            let marked: Vec<_> = listed
                .iter()
                .filter(|n| n.props["runtime"] == RUNTIME)
                .map(|n| n.id.as_str())
                .collect();
            if registrations.is_empty() {
                assert_eq!(listed, nodes);
            } else {
                assert_eq!(marked, ["file:r@web/worker.js", "sym:boot", "cmp:card"]);
            }
        }
    }
}
