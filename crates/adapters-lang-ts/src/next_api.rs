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
use crate::{EXTRACTOR_ID, FileCx, SourceId, file_id, resolution::ResolutionIndex};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::HashSet;
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
fn candidate(path: &str) -> (bool, bool) {
    let Some(stem) = path.rsplit('/').next().and_then(stem_of) else {
        return (false, false);
    };
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len() - 1];
    let app = stem == "route" && dirs.contains(&"app");
    let pages = dirs.windows(2).any(|w| w == ["pages", "api"]);
    (app, pages)
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
    let (app, pages) = candidate(cx.path);
    let mut out = Vec::new();
    if !app && !pages {
        return out;
    }
    let mut push = |app: bool, method: Option<&str>, handler: Option<String>, node: TsNode<'_>| {
        out.push(Site {
            file: cx.path.to_string(),
            app,
            method: method.map(str::to_string),
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

/// Emit every proven route site as an Endpoint, and every unproven one under
/// a proven package as a Gap.
pub(crate) fn complete(
    sites: Vec<Site>,
    index: &ResolutionIndex,
    id: &SourceId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mut seen: HashSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
    let files: Vec<String> = nodes
        .iter()
        .filter(|node| node.label == "File")
        .filter_map(|node| node.props["path"].as_str().map(str::to_string))
        .collect();
    let root_router = |dir: &str, app: bool| {
        let base = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        files.iter().any(|file| {
            file.strip_prefix(&base)
                .is_some_and(|rel| rel.starts_with(if app { "app/" } else { "pages/" }))
        })
    };
    for site in sites {
        let Some((dir, manifest, span)) = index.declaring_manifest(&site.file, "next") else {
            continue;
        };
        let rel = if dir.is_empty() {
            site.file.as_str()
        } else {
            &site.file[dir.len() + 1..]
        };
        let app = site.app;
        let Some((path, prefix)) = route(rel, app, root_router(dir, app)) else {
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
            // Producers that disagree on one endpoint's matching (overlapping
            // catch-alls, or a node-http exact or prefix route) leave it
            // matching nothing.
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
        edges.push(Edge {
            src: ep,
            dst: handler,
            label: "HANDLES".into(),
            props: serde_json::json!({
                "prov": prov(ConfidenceTier::Confirmed, &format!("HANDLES {method} {path}")),
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::route;
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
}
