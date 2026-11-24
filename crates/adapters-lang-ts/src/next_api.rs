//! Next.js API route handlers (#1325).
//!
//! A file is routed only when the nearest `package.json` at or above it
//! declares `next`, and only by Next's file conventions relative to that
//! package directory:
//!
//! - pages router: `pages/api/**` or `src/pages/api/**`; the default-exported
//!   function serves every method (`ALL`, as Express `all`);
//! - app router: `app/**/route.<ext>` or `src/app/**/route.<ext>`; each
//!   exported `GET`/`POST`/`PUT`/`PATCH`/`DELETE`/`HEAD`/`OPTIONS` function
//!   serves that method.
//!
//! A root `app/` makes `src/app` inactive and a root `pages/` makes
//! `src/pages` inactive, each independently (Next 15 `findPagesDir`). Only
//! Next's default page extensions (`.tsx`/`.ts`/`.jsx`/`.js`) are routed, as
//! `pageExtensions` is not read.
//!
//! Each becomes an `Endpoint` (`framework: "next"`) that the handler
//! `HANDLES`, citing the export and the manifest's `next` declaration.
//! `index` collapses to its parent, `[id]` is `:id`, `[...x]`/`[[...x]]` is a
//! prefix route (`/a/*`, `match: "prefix"`) whose `prefix_kind` is
//! `segments+` (one or more segments) or `segments*` (zero or more, #1329)
//! and is `conflict` where producers disagree on one endpoint, app `(group)`
//! segments drop, and
//! `_private`, `@slot` and `(.)` intercept segments are not routed. A method
//! or default export bound any other way (a wrapper call, a re-export, or a
//! binding written after its declaration, since Next reads its final value)
//! is a cited Gap, never a guess. `next.config` `basePath`/i18n is not applied.

use crate::callable::{self, Bindings};
use crate::{EXTRACTOR_ID, FileCx, Lookup, Probes, SourceId, file_id, resolution::ResolutionIndex};
use core_graph::facts::FactAccess;
use core_graph::{Edge, GraphError, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::Node as TsNode;

/// Framework label on every endpoint this module emits.
pub const FRAMEWORK: &str = "next";
/// Gap reason for a route export this module cannot bind to a function.
pub const UNPROVEN_REASON: &str = "Next.js route handler export is not a local function";

const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// Next's default `pageExtensions`, which also govern route handlers; a
/// configured list is not read, so other extensions are not routed.
const EXTENSIONS: &[&str] = &[".tsx", ".ts", ".jsx", ".js"];

/// One route export awaiting the directory's manifest proof.
#[derive(Debug, Clone)]
pub(crate) struct Site {
    file: String,
    /// An app-router `route` export, else a pages-router default export.
    app: bool,
    /// `None` for the pages-router default export (every method).
    method: Option<String>,
    /// A Vercel function under the repository's top-level `api/` (#2010),
    /// routed by `vercel.json`'s presence rather than a Next manifest.
    vercel: bool,
    /// The handler symbol, or `None` for an unproven binding (a Gap).
    handler: Option<String>,
    evidence: EvidenceRef,
}

impl Site {
    /// Cite `commit`, as a cache reuse at a new commit retargets every fact.
    pub(crate) fn retarget(&mut self, commit: &str) {
        self.evidence.commit_sha = commit.to_string();
    }
}

fn stem_of(name: &str) -> Option<&str> {
    EXTENSIONS.iter().find_map(|ext| name.strip_suffix(ext))
}

/// Whether the path could be an app-router route file and whether it could
/// be a pages-router API file; which one it is (if either) is decided
/// against the package directory at completion.
fn candidate(path: &str) -> (bool, bool, bool) {
    let Some(stem) = path.rsplit('/').next().and_then(stem_of) else {
        return (false, false, false);
    };
    // A Vercel function file (`api/**` at the repository root) exports a
    // default handler or per-method handlers, like a pages- and an
    // app-router file at once.
    if path.starts_with("api/") {
        let vercel = adapters_fw::vercel::node_function_route(path).is_some();
        return (false, false, vercel);
    }
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len() - 1];
    let app = stem == "route" && dirs.contains(&"app");
    let pages = dirs.windows(2).any(|w| w == ["pages", "api"]);
    (app, pages, false)
}

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

/// The function a route export binds: an inline callable, or an identifier
/// naming a same-file function. `name` is the exported binding, if it has
/// one: any write to it voids the proof, as Next reads its final value.
fn bound(
    cx: &FileCx<'_>,
    bindings: &Bindings<'_>,
    name: Option<TsNode<'_>>,
    value: Option<TsNode<'_>>,
) -> Option<String> {
    if let Some(name) = name
        && bindings.stable_declaration(name, cx.text(&name)).is_none()
    {
        return None;
    }
    let value = value?;
    if callable::is_callable(value) {
        return Some(callable::id(cx, value));
    }
    (value.kind() == "identifier")
        .then(|| bindings.local_target(value, cx.text(&value)))
        .flatten()
}

/// Every route export in this file, when its path is a route candidate.
pub(crate) fn sites(cx: &FileCx<'_>, root: TsNode<'_>, bindings: &Bindings<'_>) -> Vec<Site> {
    let (app, pages, vercel) = candidate(cx.path);
    let mut out = Vec::new();
    if !app && !pages && !vercel {
        return out;
    }
    let (app, pages) = (app || vercel, pages || vercel);
    let mut push = |app: bool, method: Option<&str>, handler: Option<String>, node: TsNode<'_>| {
        out.push(Site {
            file: cx.path.to_string(),
            app,
            method: method.map(str::to_string),
            vercel,
            handler,
            evidence: evidence(cx, node),
        });
    };
    let mut walk = root.walk();
    for stmt in root
        .children(&mut walk)
        .filter(|c| c.kind() == "export_statement")
    {
        let declaration = stmt.child_by_field_name("declaration");
        let mut w = stmt.walk();
        let default = stmt.children(&mut w).any(|c| c.kind() == "default");
        if default {
            if pages {
                let value = declaration.or_else(|| stmt.child_by_field_name("value"));
                let name = declaration.and_then(|d| d.child_by_field_name("name"));
                push(false, None, bound(cx, bindings, name, value), stmt);
            }
            continue;
        }
        match declaration {
            Some(decl) if app && decl.kind().contains("function_declaration") => {
                let name = decl.child_by_field_name("name");
                if let Some(method) = name.map(|n| cx.text(&n)).filter(|n| METHODS.contains(n)) {
                    push(
                        true,
                        Some(method),
                        bound(cx, bindings, name, Some(decl)),
                        stmt,
                    );
                }
            }
            Some(decl)
                if app && matches!(decl.kind(), "lexical_declaration" | "variable_declaration") =>
            {
                let mut dw = decl.walk();
                for declarator in decl
                    .named_children(&mut dw)
                    .filter(|d| d.kind() == "variable_declarator")
                {
                    let name = declarator
                        .child_by_field_name("name")
                        .filter(|n| n.kind() == "identifier");
                    if let Some(method) = name.map(|n| cx.text(&n)).filter(|n| METHODS.contains(n))
                    {
                        let value = declarator.child_by_field_name("value");
                        push(
                            true,
                            Some(method),
                            bound(cx, bindings, name, value),
                            declarator,
                        );
                    }
                }
            }
            Some(_) => {}
            None => {
                // `export { h as GET }`, `export { GET } from './x'` or
                // `export { h as default }`: the export is not bound to an
                // inline local function here, so it is a Gap.
                let mut cw = stmt.walk();
                for clause in stmt
                    .named_children(&mut cw)
                    .filter(|c| c.kind() == "export_clause")
                {
                    let mut sw = clause.walk();
                    for spec in clause
                        .named_children(&mut sw)
                        .filter(|s| s.kind() == "export_specifier")
                    {
                        let exported = spec
                            .child_by_field_name("alias")
                            .or_else(|| spec.child_by_field_name("name"))
                            .map(|n| cx.text(&n));
                        match exported {
                            Some(method) if app && METHODS.contains(&method) => {
                                push(true, Some(method), None, spec)
                            }
                            Some("default") if pages => push(false, None, None, spec),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    out
}

/// The route a file serves, relative to its Next package directory:
/// `(path, prefix_kind)`, or `None` when the conventions do not route it.
/// A root `app/` makes `src/app` inactive and a root `pages/` makes
/// `src/pages` inactive, each independently (Next 15 `findPagesDir`), so
/// `root_router` (the package has this router at its root) declines the
/// matching `src/` route.
fn route(rel: &str, app: bool, root_router: bool) -> Option<(String, Option<&'static str>)> {
    let rel = match rel.strip_prefix("src/") {
        Some(_) if root_router => return None,
        Some(inner) => inner,
        None => rel,
    };
    let mut segments: Vec<&str> = if app {
        let inner = rel.strip_prefix("app/")?;
        let (dirs, name) = inner.rsplit_once('/').unwrap_or(("", inner));
        (stem_of(name)? == "route").then_some(())?;
        dirs.split('/').filter(|s| !s.is_empty()).collect()
    } else {
        let inner = rel.strip_prefix("pages/")?;
        inner.starts_with("api/").then_some(())?;
        let (dirs, name) = inner.rsplit_once('/')?;
        let mut segments: Vec<&str> = dirs.split('/').collect();
        let stem = stem_of(name)?;
        if stem != "index" {
            segments.push(stem);
        }
        segments
    };
    if app {
        if segments
            .iter()
            .any(|s| s.starts_with('_') || s.starts_with('@') || s.starts_with("(."))
        {
            return None;
        }
        segments.retain(|s| !(s.starts_with('(') && s.ends_with(')')));
    }
    let mut parts = Vec::new();
    let mut prefix = None;
    for (i, segment) in segments.iter().enumerate() {
        let inner = segment
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .or_else(|| segment.strip_prefix('[').and_then(|s| s.strip_suffix(']')));
        match inner {
            Some(param) if param.starts_with("...") => {
                // A catch-all must be the last segment.
                (i + 1 == segments.len()).then_some(())?;
                prefix = Some(if segment.starts_with("[[") {
                    "segments*"
                } else {
                    "segments+"
                });
            }
            Some(param) if !param.is_empty() && !param.contains(['[', ']']) => {
                parts.push(format!(":{param}"));
            }
            Some(_) => return None,
            None if segment.contains(['[', ']']) || segment.is_empty() => return None,
            None => parts.push((*segment).to_string()),
        }
    }
    let base = format!("/{}", parts.join("/"));
    Some(match (prefix, base.as_str()) {
        (Some(kind), "/") => ("/*".into(), Some(kind)),
        (Some(kind), _) => (format!("{base}/*"), Some(kind)),
        (None, _) => (base, None),
    })
}

/// Whether the Next package at `dir` has `router` (`app` or `pages`) at its
/// root. Next selects the root directory by its existence alone, so one
/// holding only assets (`app/globals.css`) still makes `src/<router>`
/// inactive; walked source files cannot tell.
/// The lookup is recorded in `probes` (ADR-0072 slice 6).
fn root_router(root: &Path, dir: &str, router: &str, probes: &mut Probes) -> bool {
    let found = root.join(dir).join(router).is_dir();
    let path = if dir.is_empty() {
        router.to_string()
    } else {
        format!("{dir}/{router}")
    };
    probes.exists.insert(path, (Lookup::Directory, found));
    found
}

/// Emit every proven route site as an Endpoint, and every unproven one under
/// a proven package as a Gap.
pub(crate) fn complete(
    sites: Vec<Site>,
    root: &Path,
    index: &ResolutionIndex,
    id: &SourceId,
    facts: &mut impl FactAccess,
    probes: &mut Probes,
) -> Result<(), GraphError> {
    let mut seen: HashSet<String> = facts.node_keys().map(|(id, _)| id.to_string()).collect();
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    // Endpoints already held that another site also routes: each with the
    // site's matching, checked in site order once they are decoded.
    let mut held = Vec::new();
    let vercel_config = sites
        .iter()
        .any(|site| site.vercel)
        .then(|| {
            let bytes = source_walk::root_file(root, VERCEL_CONFIG);
            probes.read.insert(VERCEL_CONFIG.to_string(), bytes.clone());
            bytes
        })
        .flatten();
    // Each Vercel function's path, for the rewrites onto it.
    let mut functions: Vec<(String, String, String)> = Vec::new();
    for site in sites {
        if site.vercel {
            // Vercel serves `api/` itself only outside a Next project, whose
            // own `pages/api` is the API.
            if vercel_config.is_none() || index.declaring_manifest(&site.file, "next").is_some() {
                continue;
            }
            vercel_site(site, id, &mut seen, &mut nodes, &mut edges, &mut functions);
            continue;
        }
        let Some((dir, manifest, span)) = index.declaring_manifest(&site.file, "next") else {
            continue;
        };
        let rel = if dir.is_empty() {
            site.file.as_str()
        } else {
            &site.file[dir.len() + 1..]
        };
        let app = site.app;
        let router = if app { "app" } else { "pages" };
        let Some((path, prefix)) = route(rel, app, root_router(root, dir, router, probes)) else {
            continue;
        };
        let declaration = EvidenceRef {
            repo: id.repo.into(),
            path: manifest.to_string(),
            byte_start: span.0,
            byte_end: span.1,
            commit_sha: site.evidence.commit_sha.clone(),
        };
        let evidence = vec![site.evidence.clone(), declaration];
        let prov = |confidence, fact: &str| {
            let prov = Provenance::new(
                Tier::Deterministic,
                confidence,
                evidence.clone(),
                EXTRACTOR_ID,
                fact.as_bytes(),
            )
            .expect("Deterministic within ceiling");
            serde_json::to_value(prov).expect("provenance serializes")
        };
        let method = site.method.clone().unwrap_or_else(|| "ALL".into());
        let Some(handler) = site.handler else {
            let gap = format!(
                "gap:next-route:{}@{}@{}",
                id.repo, site.file, site.evidence.byte_start
            );
            if !seen.insert(gap.clone()) {
                continue;
            }
            nodes.push(Node {
                id: gap.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "next-route-handler-unproven",
                    "reason": UNPROVEN_REASON,
                    "method": method,
                    "path": path,
                    "attempted_tiers": ["T0"],
                    "prov": prov(ConfidenceTier::Gap, &format!("Gap {gap}")),
                }),
            });
            edges.push(Edge {
                src: file_id(id.repo, &site.file),
                dst: gap.clone(),
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({
                    "reason": UNPROVEN_REASON,
                    "prov": prov(ConfidenceTier::Gap, &format!("DEPENDS_ON {gap}")),
                }),
            });
            continue;
        };
        let ep = format!("ep:{}@{method}:{path}", id.repo);
        if seen.insert(ep.clone()) {
            let mut props = serde_json::json!({
                "method": method,
                "path": path,
                "framework": FRAMEWORK,
                "router": if app { "app" } else { "pages" },
                "prov": prov(ConfidenceTier::Confirmed, &format!("Endpoint {method} {path}")),
            });
            if let Some(kind) = prefix {
                props["match"] = serde_json::json!("prefix");
                props["prefix_kind"] = serde_json::json!(kind);
            }
            nodes.push(Node {
                id: ep.clone(),
                label: "Endpoint".into(),
                props,
            });
        } else if let Some(node) = nodes.iter_mut().find(|node| node.id == ep) {
            reconcile_match(node, prefix);
        } else {
            held.push((ep.clone(), prefix));
        }
        edges.push(Edge {
            src: ep,
            dst: handler,
            label: "HANDLES".into(),
            props: serde_json::json!({
                "prov": prov(ConfidenceTier::Confirmed, &format!("HANDLES {method} {path}")),
            }),
        });
    }
    if let Some(config) = &vercel_config {
        vercel_rewrites(config, &functions, id, &mut seen, &mut nodes, &mut edges);
    }
    let ids: HashSet<&str> = held.iter().map(|(ep, _)| ep.as_str()).collect();
    facts.edit_nodes(
        |id, _| ids.contains(id),
        |selected, added| {
            for (ep, prefix) in &held {
                if let Some(node) = selected.iter_mut().find(|node| node.id == *ep) {
                    reconcile_match(node, *prefix);
                }
            }
            added.append(&mut nodes);
        },
    )?;
    facts.append(Vec::new(), edges)
}

/// Framework label on a Vercel function's endpoints (#2010).
pub const VERCEL_FRAMEWORK: &str = "vercel";
/// Gap reason for a Vercel function export this module cannot bind.
pub const VERCEL_UNPROVEN_REASON: &str = "Vercel function export is not a local function";
const VERCEL_CONFIG: &str = "vercel.json";

fn provenance(
    evidence: Vec<EvidenceRef>,
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let prov = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic within ceiling");
    serde_json::to_value(prov).expect("provenance serializes")
}

/// The language a function file is written in, by its extension.
fn language(file: &str) -> &'static str {
    if file.ends_with(".ts") || file.ends_with(".tsx") {
        "typescript"
    } else {
        "javascript"
    }
}

/// One Vercel function export as an Endpoint at the path Vercel serves its
/// file at (`api/users/[id].ts` at `/api/users/:id`, `index` at its
/// directory), or a Gap when the export binds no local function.
fn vercel_site(
    site: Site,
    id: &SourceId,
    seen: &mut HashSet<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    functions: &mut Vec<(String, String, String)>,
) {
    // Vercel's file routing is the pages router's, rooted at `api/`.
    let Some((path, prefix)) = route(&format!("pages/{}", site.file), false, false) else {
        return;
    };
    let evidence = vec![site.evidence.clone()];
    let method = site.method.clone().unwrap_or_else(|| "ALL".into());
    let Some(handler) = site.handler else {
        let gap = format!(
            "gap:vercel-route:{}@{}@{}",
            id.repo, site.file, site.evidence.byte_start
        );
        if seen.insert(gap.clone()) {
            nodes.push(Node {
                id: gap.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "vercel-function-handler-unproven",
                    "reason": VERCEL_UNPROVEN_REASON,
                    "method": method,
                    "path": path,
                    "attempted_tiers": ["T0"],
                    "prov": provenance(evidence.clone(), ConfidenceTier::Gap, &format!("Gap {gap}")),
                }),
            });
            edges.push(Edge {
                src: file_id(id.repo, &site.file),
                dst: gap.clone(),
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({
                    "reason": VERCEL_UNPROVEN_REASON,
                    "prov": provenance(evidence, ConfidenceTier::Gap, &format!("DEPENDS_ON {gap}")),
                }),
            });
        }
        return;
    };
    if prefix.is_none() {
        functions.push((path.clone(), method.clone(), handler.clone()));
    }
    let props = vercel_props(
        &method,
        &path,
        prefix,
        &site.file,
        &handler,
        provenance(
            evidence.clone(),
            ConfidenceTier::Confirmed,
            &format!("Endpoint {method} {path}"),
        ),
    );
    push_endpoint(
        id, &method, &path, &handler, props, evidence, seen, nodes, edges,
    );
}

fn vercel_props(
    method: &str,
    path: &str,
    prefix: Option<&str>,
    file: &str,
    handler: &str,
    prov: serde_json::Value,
) -> serde_json::Value {
    let mut props = serde_json::json!({
        "method": method,
        "path": path,
        "framework": VERCEL_FRAMEWORK,
        "language": language(file),
        "handler_sym": handler,
        "prov": prov,
    });
    if let Some(kind) = prefix {
        props["match"] = serde_json::json!("prefix");
        props["prefix_kind"] = serde_json::json!(kind);
    }
    props
}

#[allow(clippy::too_many_arguments)]
fn push_endpoint(
    id: &SourceId,
    method: &str,
    path: &str,
    handler: &str,
    props: serde_json::Value,
    evidence: Vec<EvidenceRef>,
    seen: &mut HashSet<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let ep = format!("ep:{}@{method}:{path}", id.repo);
    // An endpoint another route already holds keeps its node, and the
    // function still handles it.
    if seen.insert(ep.clone()) {
        nodes.push(Node {
            id: ep.clone(),
            label: "Endpoint".into(),
            props,
        });
    }
    edges.push(Edge {
        src: ep,
        dst: handler.to_string(),
        label: "HANDLES".into(),
        props: serde_json::json!({
            "prov": provenance(evidence, ConfidenceTier::Confirmed, &format!("HANDLES {method} {path}")),
        }),
    });
}

/// A rewrite source path in this module's route syntax: `{name}` is
/// `:name`, a final `{name...}` a zero-or-more prefix (`/*`).
fn rewrite_route(path: &str) -> (String, Option<&'static str>) {
    let mut parts = Vec::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) if name.ends_with("...") => {
                return (
                    format!("/{}", [parts, vec!["*".to_string()]].concat().join("/")),
                    Some("segments*"),
                );
            }
            Some(name) => parts.push(format!(":{name}")),
            None => parts.push(segment.to_string()),
        }
    }
    (format!("/{}", parts.join("/")), None)
}

/// Each `vercel.json` rewrite onto a function served at a fixed path is an
/// Endpoint at its source handled by the same function, marked
/// `rewrite_of`, citing the rewrite. A rewrite onto anything else is left to
/// the language adapter whose function it names, or to none.
fn vercel_rewrites(
    config: &[u8],
    functions: &[(String, String, String)],
    id: &SourceId,
    seen: &mut HashSet<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for rewrite in adapters_fw::vercel::rewrites(config) {
        let Some(paths) = adapters_fw::vercel::source_paths(&rewrite.source) else {
            continue;
        };
        let evidence = vec![EvidenceRef {
            repo: id.repo.into(),
            path: VERCEL_CONFIG.into(),
            byte_start: rewrite.start as u64,
            byte_end: rewrite.end as u64,
            commit_sha: id.commit.into(),
        }];
        for (served, method, handler) in functions.iter().filter(|(p, _, _)| *p == rewrite.target) {
            for source in &paths {
                let (path, prefix) = rewrite_route(source);
                if path == *served {
                    continue;
                }
                let mut props = vercel_props(
                    method,
                    &path,
                    prefix,
                    "",
                    handler,
                    provenance(
                        evidence.clone(),
                        ConfidenceTier::Confirmed,
                        &format!("Endpoint {method} {path} rewrites to {served}"),
                    ),
                );
                props["rewrite_of"] = serde_json::json!(served);
                props["language"] = serde_json::json!(
                    nodes
                        .iter()
                        .find(|n| n.props["handler_sym"] == handler.as_str())
                        .and_then(|n| n.props["language"].as_str())
                        .unwrap_or("javascript")
                );
                push_endpoint(
                    id,
                    method,
                    &path,
                    handler,
                    props,
                    evidence.clone(),
                    seen,
                    nodes,
                    edges,
                );
            }
        }
    }
}

/// Producers that disagree on one endpoint's matching (overlapping
/// catch-alls, or a node-http exact or prefix route) leave it matching
/// nothing.
fn reconcile_match(node: &mut Node, prefix: Option<&str>) {
    let kind = (node.props["match"] == "prefix").then(|| {
        node.props["prefix_kind"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    });
    if kind.as_deref() != prefix {
        node.props["match"] = serde_json::json!("prefix");
        node.props["prefix_kind"] = serde_json::json!("conflict");
    }
}

/// Screen router label for app-router pages.
pub const APP_ROUTER: &str = "next-app";

/// The screen route of an app-router `page` file relative to its package,
/// or `None` when the conventions do not route it. Segments keep their
/// file spelling, as pages-router screens do (`/users/[id]`); `(group)`
/// segments drop, and `_private`, `@slot` and `(.)` intercept segments are
/// not routed. A root `app/` makes `src/app` inactive.
fn app_screen_route(rel: &str, root_router: bool) -> Option<String> {
    let rel = match rel.strip_prefix("src/") {
        Some(_) if root_router => return None,
        Some(inner) => inner,
        None => rel,
    };
    let inner = rel.strip_prefix("app/")?;
    let (dirs, name) = inner.rsplit_once('/').unwrap_or(("", inner));
    (stem_of(name)? == "page").then_some(())?;
    let mut segments = Vec::new();
    for segment in dirs.split('/').filter(|s| !s.is_empty()) {
        if segment.starts_with('_') || segment.starts_with('@') || segment.starts_with("(.") {
            return None;
        }
        if !(segment.starts_with('(') && segment.ends_with(')')) {
            segments.push(segment);
        }
    }
    Some(format!("/{}", segments.join("/")))
}

/// Next.js app-router pages (#1364): under a package that declares `next`,
/// `app/**/page.<ext>` (or `src/app/**`) is a `Screen` that `RENDERS` the
/// file's default export, citing the file and the manifest's `next`
/// declaration. The screen records the file convention, not a verified
/// page: a page without a recovered default export (including any `.ts`
/// page, as default exports are read from component files only) is a
/// screen with no `RENDERS`. A screen id another router
/// already emitted is kept.
pub(crate) fn app_screens(
    root: &Path,
    index: &ResolutionIndex,
    id: &SourceId,
    default_exports: &HashMap<String, String>,
    facts: &mut impl FactAccess,
    probes: &mut Probes,
) -> Result<(), GraphError> {
    let mut seen: HashSet<String> = facts.node_keys().map(|(id, _)| id.to_string()).collect();
    let files: Vec<String> = facts
        .decode_nodes(|_, label| label == "File")?
        .into_iter()
        .filter_map(|node| node.props["path"].as_str().map(str::to_string))
        .collect();
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    for file in &files {
        let Some((dir, manifest, span)) = index.declaring_manifest(file, "next") else {
            continue;
        };
        let rel = if dir.is_empty() {
            file.as_str()
        } else {
            &file[dir.len() + 1..]
        };
        let Some(route) = app_screen_route(rel, root_router(root, dir, "app", probes)) else {
            continue;
        };
        let screen = format!("screen:{}@{route}", id.repo);
        if !seen.insert(screen.clone()) {
            continue;
        }
        let evidence = vec![
            EvidenceRef {
                repo: id.repo.into(),
                path: file.clone(),
                byte_start: 0,
                byte_end: 0,
                commit_sha: id.commit.into(),
            },
            EvidenceRef {
                repo: id.repo.into(),
                path: manifest.to_string(),
                byte_start: span.0,
                byte_end: span.1,
                commit_sha: id.commit.into(),
            },
        ];
        let prov = |fact: &str| {
            let prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                evidence.clone(),
                EXTRACTOR_ID,
                fact.as_bytes(),
            )
            .expect("Deterministic within ceiling");
            serde_json::to_value(prov).expect("provenance serializes")
        };
        nodes.push(Node {
            id: screen.clone(),
            label: "Screen".into(),
            props: serde_json::json!({
                "route": route,
                "router": APP_ROUTER,
                "prov": prov(&format!("Screen {route}")),
            }),
        });
        if let Some(component) = default_exports.get(file) {
            edges.push(Edge {
                src: screen.clone(),
                dst: component.clone(),
                label: "RENDERS".into(),
                props: serde_json::json!({
                    "prov": prov(&format!("RENDERS {screen} -> {component}")),
                }),
            });
        }
    }
    facts.append(nodes, edges)
}

#[cfg(test)]
mod tests {
    use super::{app_screen_route, route};
    use crate::{IncrementalCache, SourceId, extract_dir, extract_dir_incremental};
    use core_prov::Provenance;

    fn write(root: &std::path::Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn id(commit: &str) -> SourceId<'_> {
        SourceId {
            repo: "qwtm/example",
            commit,
        }
    }

    // AC-0451: a `next` package's pages/api default export and app-router
    // method exports become cited endpoints; other bindings are Gaps, and a
    // package without `next` routes nothing.
    #[test]
    fn next_route_handlers_become_endpoints_under_a_next_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "web/package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "web/pages/api/users/[id].ts",
            "export default async function handler(req, res) { res.json({}); }\n",
        );
        write(
            root,
            "web/src/app/(shop)/api/items/route.ts",
            "export async function GET() { return Response.json([]); }\n\
             export const POST = async (req) => Response.json({});\n\
             function remove() {}\n\
             export const PATCH = remove;\n\
             export const PUT = withAuth(remove);\n\
             export { remove as DELETE };\n",
        );
        write(
            root,
            "lib/package.json",
            r#"{ "dependencies": { "express": "4" } }"#,
        );
        write(
            root,
            "lib/pages/api/x.ts",
            "export default function x() {}\n",
        );

        let out = extract_dir(root, &id("abc123")).unwrap();
        let endpoints: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.label == "Endpoint" && n.props["framework"] == "next")
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            endpoints,
            [
                "ep:qwtm/example@ALL:/api/users/:id",
                "ep:qwtm/example@GET:/api/items",
                "ep:qwtm/example@POST:/api/items",
                "ep:qwtm/example@PATCH:/api/items",
            ]
        );
        let handles: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.label == "HANDLES" && e.src.contains("/api/"))
            .map(|e| (e.src.as_str(), e.dst.as_str()))
            .collect();
        let symbols: std::collections::HashSet<_> =
            out.nodes.iter().map(|n| n.id.as_str()).collect();
        for (_, handler) in &handles {
            assert!(symbols.contains(handler), "{handler} is an emitted symbol");
        }
        assert!(handles.contains(&(
            "ep:qwtm/example@ALL:/api/users/:id",
            "sym:qwtm/example@web/pages/api/users/[id].ts#handler"
        )));
        assert!(handles.contains(&(
            "ep:qwtm/example@PATCH:/api/items",
            "sym:qwtm/example@web/src/app/(shop)/api/items/route.ts#remove"
        )));
        let ep = out
            .nodes
            .iter()
            .find(|n| n.id == "ep:qwtm/example@GET:/api/items")
            .unwrap();
        let prov: Provenance = serde_json::from_value(ep.props["prov"].clone()).unwrap();
        assert!(
            prov.evidence
                .iter()
                .any(|e| e.path == "web/package.json" && e.byte_end > 0)
        );
        let gaps: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.props["kind"] == "next-route-handler-unproven")
            .map(|n| n.props["method"].as_str().unwrap())
            .collect();
        assert_eq!(gaps, ["PUT", "DELETE"]);
        assert!(
            !out.nodes
                .iter()
                .any(|n| n.id.contains("/x") && n.label == "Endpoint")
        );

        // Deterministic, and a cache reuse at a new commit cites that commit.
        assert_eq!(out.nodes, extract_dir(root, &id("abc123")).unwrap().nodes);
        let mut cache = IncrementalCache::default();
        extract_dir_incremental(root, &id("abc123"), &mut cache).unwrap();
        let (again, _) = extract_dir_incremental(root, &id("def456"), &mut cache).unwrap();
        let ep = again
            .nodes
            .iter()
            .find(|n| n.id == "ep:qwtm/example@GET:/api/items")
            .unwrap();
        let prov: Provenance = serde_json::from_value(ep.props["prov"].clone()).unwrap();
        assert!(prov.evidence.iter().all(|e| e.commit_sha == "def456"));
    }

    fn next_endpoints(out: &crate::Extraction) -> Vec<&str> {
        out.nodes
            .iter()
            .filter(|n| n.label == "Endpoint" && n.props["framework"] == "next")
            .map(|n| n.id.as_str())
            .collect()
    }

    // AC-0453: a catch-all declares how it matches: `[...x]` one or more
    // segments, `[[...x]]` zero or more.
    #[test]
    fn next_catch_all_endpoints_carry_their_prefix_kind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "pages/api/files/[...path].ts",
            "export default function files() {}\n",
        );
        write(
            root,
            "app/docs/[[...slug]]/route.ts",
            "export function GET() {}\n",
        );
        let out = extract_dir(root, &id("abc123")).unwrap();
        let kind = |ep: &str| {
            let node = out.nodes.iter().find(|n| n.id == ep).unwrap();
            assert_eq!(node.props["match"], "prefix", "{ep}");
            node.props["prefix_kind"].as_str().unwrap().to_string()
        };
        assert_eq!(kind("ep:qwtm/example@ALL:/api/files/*"), "segments+");
        assert_eq!(kind("ep:qwtm/example@GET:/docs/*"), "segments*");
    }

    // AC-0453 (#1339 review): overlapping catch-alls and a node-http prefix on
    // one endpoint disagree on its kind, whichever is read first.
    #[test]
    fn overlapping_prefix_producers_conflict_in_any_order() {
        for (first, second) in [("[...slug]", "[[...slug]]"), ("[[...slug]]", "[...slug]")] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            write(
                root,
                "package.json",
                r#"{ "dependencies": { "next": "15.0.0" } }"#,
            );
            for (group, segment) in [("(a)", first), ("(b)", second)] {
                write(
                    root,
                    &format!("app/{group}/docs/{segment}/route.ts"),
                    "export function GET() {}\n",
                );
            }
            write(
                root,
                "server.ts",
                "import http from 'node:http';\n\
                 http.createServer((req, res) => {\n\
                   if (req.method === 'GET' && req.url.startsWith('/raw/')) { res.end('r'); }\n\
                   if (req.method === 'GET' && req.url === '/exact/*') { res.end('e'); }\n\
                   if (req.method === 'GET' && req.url === '/lit/*') { res.end('l'); }\n\
                 }).listen(3000);\n",
            );
            for dir in ["raw", "exact"] {
                write(
                    root,
                    &format!("app/{dir}/[...rest]/route.ts"),
                    "export function GET() {}\n",
                );
            }
            let out = extract_dir(root, &id("abc123")).unwrap();
            let node = |ep: &str| {
                let nodes: Vec<_> = out.nodes.iter().filter(|n| n.id == ep).collect();
                assert_eq!(nodes.len(), 1, "{ep}");
                nodes[0].props.clone()
            };
            for ep in [
                "ep:qwtm/example@GET:/docs/*",
                "ep:qwtm/example@GET:/raw/*",
                "ep:qwtm/example@GET:/exact/*",
            ] {
                let props = node(ep);
                assert_eq!(props["match"], "prefix", "{first} {ep}");
                assert_eq!(props["prefix_kind"], "conflict", "{first} {ep}");
            }
            // A standalone exact route ending in `*` stays exact.
            let props = node("ep:qwtm/example@GET:/lit/*");
            assert!(props.get("match").is_none(), "{props}");
        }
    }

    // AC-0451 (#1326 review): a written exported binding is not proven, a root
    // router makes `src/` inactive, `.mjs` is not a default extension, and an
    // app route may sit under segments named `pages/api`.
    #[test]
    fn next_routes_follow_final_bindings_router_roots_and_default_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "app/api/live/route.ts",
            "export let GET = () => Response.json(1);\n\
             GET = () => Response.json(2);\n\
             export function POST() {}\n\
             POST = null;\n\
             export const PUT = () => Response.json(3);\n\
             export function DELETE() {}\n",
        );
        write(root, "app/pages/api/route.ts", "export function GET() {}\n");
        write(
            root,
            "src/app/api/ghost/route.ts",
            "export function GET() {}\n",
        );
        write(
            root,
            "pages/api/ghost.mjs",
            "export default function ghost() {}\n",
        );
        write(
            root,
            "pages/api/swapped.ts",
            "export default function handler() {}\nhandler = () => {};\n",
        );
        // Each router picks its root independently: a root app beside
        // src/pages routes both. A src-only package still routes its src/app.
        write(
            root,
            "mixed/package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "mixed/app/api/a/route.ts",
            "export function GET() {}\n",
        );
        write(
            root,
            "mixed/src/pages/api/b.ts",
            "export default function b() {}\n",
        );
        write(
            root,
            "other/package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "other/src/app/api/kept/route.ts",
            "export function GET() {}\n",
        );

        let out = extract_dir(root, &id("abc123")).unwrap();
        assert_eq!(
            next_endpoints(&out),
            [
                "ep:qwtm/example@PUT:/api/live",
                "ep:qwtm/example@DELETE:/api/live",
                "ep:qwtm/example@GET:/pages/api",
                "ep:qwtm/example@GET:/api/a",
                "ep:qwtm/example@ALL:/api/b",
                "ep:qwtm/example@GET:/api/kept",
            ]
        );
        let mut gaps: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.props["kind"] == "next-route-handler-unproven")
            .map(|n| {
                format!(
                    "{} {}",
                    n.props["method"].as_str().unwrap(),
                    n.props["path"]
                )
            })
            .collect();
        gaps.sort();
        assert_eq!(
            gaps,
            [
                "ALL \"/api/swapped\"",
                "GET \"/api/live\"",
                "POST \"/api/live\"",
            ]
        );
    }

    // AC-0451: Next's file conventions, relative to the package directory.
    #[test]
    fn next_file_conventions_map_to_routes() {
        let cases = [
            ("pages/api/users.ts", false, Some(("/api/users", None))),
            (
                "src/pages/api/users/index.js",
                false,
                Some(("/api/users", None)),
            ),
            ("pages/api/index.ts", false, Some(("/api", None))),
            (
                "pages/api/users/[id].ts",
                false,
                Some(("/api/users/:id", None)),
            ),
            (
                "pages/api/files/[...slug].ts",
                false,
                Some(("/api/files/*", Some("segments+"))),
            ),
            (
                "pages/api/[[...all]].ts",
                false,
                Some(("/api/*", Some("segments*"))),
            ),
            ("pages/users.tsx", false, None),
            ("app/route.ts", true, Some(("/", None))),
            ("app/api/users/route.ts", true, Some(("/api/users", None))),
            (
                "src/app/(admin)/api/[id]/route.js",
                true,
                Some(("/api/:id", None)),
            ),
            (
                "app/docs/[[...slug]]/route.ts",
                true,
                Some(("/docs/*", Some("segments*"))),
            ),
            ("app/_lib/route.ts", true, None),
            ("app/@modal/x/route.ts", true, None),
            ("app/(.)photo/route.ts", true, None),
            ("app/api/[...a]/b/route.ts", true, None),
            ("app/api/page.tsx", true, None),
            ("lib/app/api/route.ts", true, None),
            ("app/pages/api/route.ts", true, Some(("/pages/api", None))),
            ("pages/api/x.mjs", false, None),
        ];
        for (rel, app, expected) in cases {
            let got = route(rel, app, false);
            assert_eq!(
                got.as_ref().map(|(p, prefix)| (p.as_str(), *prefix)),
                expected,
                "{rel}"
            );
        }
        // A root router makes `src/` inactive.
        assert_eq!(route("src/app/api/x/route.ts", true, true), None);
        assert_eq!(route("app/api/x/route.ts", true, true).unwrap().0, "/api/x");
    }

    fn screens(out: &crate::Extraction, router: &str) -> Vec<(String, Option<String>)> {
        let mut screens: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.label == "Screen" && n.props["router"] == router)
            .map(|n| {
                let renders = out
                    .edges
                    .iter()
                    .find(|e| e.label == "RENDERS" && e.src == n.id)
                    .map(|e| e.dst.clone());
                (n.props["route"].as_str().unwrap().to_string(), renders)
            })
            .collect();
        screens.sort();
        screens
    }

    // AC-0467: a `next` package's app-router pages become screens that
    // render their default export, citing the page and the `next`
    // declaration; other packages, other file names and unrouted segments
    // yield none.
    #[test]
    fn next_app_router_pages_become_screens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "web/package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(
            root,
            "web/app/page.tsx",
            "export default function Home() { return <main />; }\n",
        );
        write(
            root,
            "web/app/(shop)/users/[id]/page.tsx",
            "export default function User() { return <div />; }\n",
        );
        write(
            root,
            "web/app/docs/[...slug]/page.js",
            "export default function Docs() { return null; }\n",
        );
        write(
            root,
            "web/app/empty/page.tsx",
            "export function NotDefault() { return null; }\n",
        );
        write(
            root,
            "web/app/users/layout.tsx",
            "export default function Layout({ children }) { return children; }\n",
        );
        for unrouted in [
            "web/app/_lib/page.tsx",
            "web/app/@modal/x/page.tsx",
            "web/app/(.)photo/page.tsx",
            "web/src/app/shadowed/page.tsx",
        ] {
            write(
                root,
                unrouted,
                "export default function P() { return null; }\n",
            );
        }
        write(
            root,
            "lib/package.json",
            r#"{ "dependencies": { "express": "4" } }"#,
        );
        write(
            root,
            "lib/app/page.tsx",
            "export default function Other() { return null; }\n",
        );

        let out = extract_dir(root, &id("abc123")).unwrap();
        let sym = |path: &str, name: &str| Some(format!("sym:qwtm/example@{path}#{name}"));
        assert_eq!(
            screens(&out, super::APP_ROUTER),
            [
                ("/".to_string(), sym("web/app/page.tsx", "Home")),
                (
                    "/docs/[...slug]".to_string(),
                    sym("web/app/docs/[...slug]/page.js", "Docs")
                ),
                ("/empty".to_string(), None),
                (
                    "/users/[id]".to_string(),
                    sym("web/app/(shop)/users/[id]/page.tsx", "User")
                ),
            ]
        );
        let symbols: std::collections::HashSet<_> =
            out.nodes.iter().map(|n| n.id.as_str()).collect();
        // A page without a recovered default export is still a screen by
        // file convention; it renders nothing recovered.
        for renders in screens(&out, super::APP_ROUTER)
            .into_iter()
            .filter_map(|(_, renders)| renders)
        {
            assert!(symbols.contains(renders.as_str()));
        }
        let screen = out
            .nodes
            .iter()
            .find(|n| n.id == "screen:qwtm/example@/users/[id]")
            .unwrap();
        let prov: Provenance = serde_json::from_value(screen.props["prov"].clone()).unwrap();
        assert!(
            prov.evidence
                .iter()
                .any(|e| e.path == "web/package.json" && e.byte_end > 0)
        );
        assert!(
            prov.evidence
                .iter()
                .any(|e| e.path == "web/app/(shop)/users/[id]/page.tsx")
        );

        // Deterministic, and a cache reuse at a new commit cites that commit.
        assert_eq!(out.nodes, extract_dir(root, &id("abc123")).unwrap().nodes);
        let mut cache = IncrementalCache::default();
        extract_dir_incremental(root, &id("abc123"), &mut cache).unwrap();
        let (again, _) = extract_dir_incremental(root, &id("def456"), &mut cache).unwrap();
        let screen = again
            .nodes
            .iter()
            .find(|n| n.id == "screen:qwtm/example@/")
            .unwrap();
        let prov: Provenance = serde_json::from_value(screen.props["prov"].clone()).unwrap();
        assert!(prov.evidence.iter().all(|e| e.commit_sha == "def456"));
    }

    // AC-0467 (#1365 review): a root `app/` holding only assets still makes
    // `src/app` inactive, for screens and route handlers alike, because Next
    // selects the root directory by its existence.
    #[test]
    fn next_asset_only_root_app_still_shadows_src_app() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        write(root, "app/globals.css", "body {}\n");
        write(
            root,
            "src/app/page.tsx",
            "export default function Home() { return null; }\n",
        );
        write(root, "src/app/api/x/route.ts", "export function GET() {}\n");
        let out = extract_dir(root, &id("abc123")).unwrap();
        assert_eq!(screens(&out, super::APP_ROUTER), []);
        assert!(next_endpoints(&out).is_empty());
    }

    // AC-0467: a pages-router screen of the same route keeps its id, and
    // without a root `app/` the `src/app` pages are routed. A `.ts` page
    // holds no JSX component, so it renders nothing recovered.
    #[test]
    fn next_app_screens_defer_to_existing_screens_and_route_src_app() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            r#"{ "dependencies": { "next": "14" } }"#,
        );
        write(
            root,
            "pages/about.tsx",
            "export default function About() { return null; }\n",
        );
        write(
            root,
            "src/app/about/page.tsx",
            "export default function AppAbout() { return null; }\n",
        );
        write(
            root,
            "src/app/(marketing)/pricing/page.ts",
            "export default function Pricing() { return null; }\n",
        );
        let out = extract_dir(root, &id("abc123")).unwrap();
        assert_eq!(
            screens(&out, super::APP_ROUTER),
            [("/pricing".to_string(), None)]
        );
        assert_eq!(
            screens(&out, "next-pages"),
            [(
                "/about".to_string(),
                Some("sym:qwtm/example@pages/about.tsx#About".into())
            )]
        );
    }

    // AC-0467: app-router page conventions relative to the package directory.
    #[test]
    fn next_app_page_conventions_map_to_screen_routes() {
        let cases = [
            ("app/page.tsx", false, Some("/")),
            ("app/a/(g)/b/page.jsx", false, Some("/a/b")),
            ("src/app/[[...x]]/page.ts", false, Some("/[[...x]]")),
            ("src/app/x/page.tsx", true, None),
            ("app/x/route.ts", false, None),
            ("app/x/page.mdx", false, None),
            ("app/x/_p/page.tsx", false, None),
            ("app/@s/page.tsx", false, None),
            ("app/(..)x/page.tsx", false, None),
            ("pages/x.tsx", false, None),
        ];
        for (rel, root_router, want) in cases {
            assert_eq!(app_screen_route(rel, root_router).as_deref(), want, "{rel}");
        }
    }

    // AC-0540: Next.js route and app-page completion over the packed store
    // leave exactly the facts they leave over plain lists: an endpoint
    // already held that a site routes with other matching is reconciled in
    // place, a repeated site edits its own new endpoint, an unbound export
    // is a Gap, and an app page already emitted as a screen is kept.
    #[test]
    fn next_completion_is_identical_over_lists_and_store() {
        use super::{Site, app_screens, complete};
        use crate::resolution::ResolutionIndex;
        use core_graph::facts::{FactLists, FactStore};
        use core_graph::{Edge, Node};
        use core_prov::EvidenceRef;
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "web/package.json",
            r#"{ "dependencies": { "next": "15.0.0" } }"#,
        );
        let index = ResolutionIndex::load(root).unwrap();
        let id = SourceId {
            repo: "r",
            commit: "c",
        };
        let node = |id: &str, label: &str, props: serde_json::Value| Node {
            id: id.into(),
            label: label.into(),
            props,
        };
        let site = |file: &str, method: &str, handler: Option<&str>, at: u64| Site {
            file: file.into(),
            app: true,
            method: Some(method.into()),
            vercel: false,
            handler: handler.map(str::to_string),
            evidence: EvidenceRef {
                repo: "r".into(),
                path: file.into(),
                byte_start: at,
                byte_end: at + 1,
                commit_sha: "c".into(),
            },
        };
        let items = "web/app/api/items/route.ts";
        let users = "web/app/api/users/route.ts";
        let sites = || {
            vec![
                site(items, "GET", Some("sym:list"), 1),
                site(items, "POST", None, 2),
                site(users, "GET", Some("sym:users"), 3),
                site(users, "GET", Some("sym:users2"), 4),
            ]
        };
        let held = node(
            "ep:r@GET:/api/items",
            "Endpoint",
            serde_json::json!({"match": "prefix", "prefix_kind": "catch-all"}),
        );
        let nodes = vec![
            held.clone(),
            node("screen:r@/about", "Screen", serde_json::json!({})),
            node(
                "file:r@web/app/page.tsx",
                "File",
                serde_json::json!({"path": "web/app/page.tsx"}),
            ),
            node(
                "file:r@web/app/about/page.tsx",
                "File",
                serde_json::json!({"path": "web/app/about/page.tsx"}),
            ),
        ];
        let exports = HashMap::from([("web/app/page.tsx".to_string(), "sym:home".to_string())]);
        let (mut listed, mut listed_edges): (Vec<Node>, Vec<Edge>) = (nodes.clone(), Vec::new());
        let mut lists = FactLists {
            nodes: &mut listed,
            edges: &mut listed_edges,
        };
        app_screens(
            root,
            &index,
            &id,
            &exports,
            &mut lists,
            &mut crate::Probes::default(),
        )
        .unwrap();
        complete(
            sites(),
            root,
            &index,
            &id,
            &mut lists,
            &mut crate::Probes::default(),
        )
        .unwrap();
        let mut store = FactStore::from_facts(nodes.clone(), Vec::new()).unwrap();
        app_screens(
            root,
            &index,
            &id,
            &exports,
            &mut store,
            &mut crate::Probes::default(),
        )
        .unwrap();
        complete(
            sites(),
            root,
            &index,
            &id,
            &mut store,
            &mut crate::Probes::default(),
        )
        .unwrap();
        assert_eq!(
            store.into_facts().unwrap(),
            (listed.clone(), listed_edges.clone())
        );
        assert_eq!(listed[0].props["prefix_kind"], "conflict");
        let ids: Vec<_> = listed[4..].iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "screen:r@/",
                "gap:next-route:r@web/app/api/items/route.ts@2",
                "ep:r@GET:/api/users",
            ]
        );
        let handles: Vec<_> = listed_edges
            .iter()
            .map(|e| (e.label.as_str(), e.dst.as_str()))
            .collect();
        assert_eq!(
            handles,
            [
                ("RENDERS", "sym:home"),
                ("HANDLES", "sym:list"),
                (
                    "DEPENDS_ON",
                    "gap:next-route:r@web/app/api/items/route.ts@2"
                ),
                ("HANDLES", "sym:users"),
                ("HANDLES", "sym:users2"),
            ]
        );
    }

    // AC-0927 (#2010): with a root `vercel.json`, `api/**` functions are
    // endpoints at their served path, rewrites onto them are endpoints at
    // their source, private and declaration files are not routed, an
    // unbound export is a Gap, and without `vercel.json` nothing is routed.
    #[test]
    fn vercel_functions_become_endpoints_with_their_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "vercel.json",
            r#"{"rewrites": [{"source": "/hooks/(.*)", "destination": "/api/hooks"}, {"source": "/u/:id", "destination": "/api/users/[id]"}]}"#,
        );
        write(
            root,
            "api/hooks.ts",
            "export default function handler(req, res) { res.status(200).end(); }\n",
        );
        write(
            root,
            "api/users/[id].js",
            "export function GET(request) { return new Response('ok'); }\n\
             export { other as POST } from './other';\n",
        );
        write(root, "api/_lib/db.ts", "export default function db() {}\n");
        write(
            root,
            "api/foo.bar.ts",
            "export default function handler(req, res) { res.end(); }\n",
        );
        // A second function at a path already held keeps the one node and
        // still handles it.
        write(
            root,
            "api/foo.bar/index.js",
            "export default function other(req, res) { res.end(); }\n",
        );
        let out = extract_dir(root, &id("abc123")).unwrap();
        let mut endpoints: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.label == "Endpoint" && n.props["framework"] == "vercel")
            .map(|n| {
                (
                    n.id.as_str(),
                    n.props["language"].as_str().unwrap(),
                    n.props["rewrite_of"].as_str(),
                )
            })
            .collect();
        endpoints.sort();
        assert_eq!(
            endpoints,
            [
                ("ep:qwtm/example@ALL:/api/foo.bar", "typescript", None),
                ("ep:qwtm/example@ALL:/api/hooks", "typescript", None),
                (
                    "ep:qwtm/example@ALL:/hooks/*",
                    "typescript",
                    Some("/api/hooks")
                ),
                ("ep:qwtm/example@GET:/api/users/:id", "javascript", None),
            ]
        );
        assert!(out.edges.iter().any(|e| e.label == "HANDLES"
            && e.src == "ep:qwtm/example@ALL:/api/hooks"
            && e.dst.contains("api/hooks.ts")));
        let held = out
            .edges
            .iter()
            .filter(|e| e.label == "HANDLES" && e.src == "ep:qwtm/example@ALL:/api/foo.bar")
            .count();
        assert_eq!(held, 2);
        assert!(out.nodes.iter().any(|n| n.label == "Gap"
            && n.props["kind"] == "vercel-function-handler-unproven"
            && n.props["method"] == "POST"));

        std::fs::remove_file(root.join("vercel.json")).unwrap();
        let out = extract_dir(root, &id("abc123")).unwrap();
        assert!(!out.nodes.iter().any(|n| n.props["framework"] == "vercel"));
    }
}
