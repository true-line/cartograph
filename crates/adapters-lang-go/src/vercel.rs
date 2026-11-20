//! Vercel Go functions and `vercel.json` rewrites as routes (#2010).
//!
//! On Vercel, a Go file under the repository's top-level `api/` directory
//! that exports one function with the `http.HandlerFunc` signature is served
//! at its path: `api/auth/login/index.go` at `/api/auth/login`, `api/x.go`
//! at `/api/x`. Each such function is an `Endpoint` (method `ANY`,
//! framework `vercel`) that `HANDLES` it, recovered only when the repository
//! root has a `vercel.json`. Each rewrite in that file whose destination is
//! one of these functions is an `Endpoint` at its source path handled by the
//! same function, marked with the path it rewrites to; `(a|b)` alternatives
//! are one path each and `:name` / `(.*)` segments are wildcards. A rewrite
//! onto `/api/…` that no function serves, or whose source cannot be read,
//! is a Gap; a rewrite onto a static file is not a route.

use super::{Extraction, FileCx, ImportBinding};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use tree_sitter::Node as TsNode;

/// A top-level function with the `http.HandlerFunc` signature.
#[derive(Debug, Clone)]
pub(super) struct HandlerFunction {
    pub(super) symbol: String,
    pub(super) path: String,
    pub(super) exported: bool,
    pub(super) prov: serde_json::Value,
}

/// `function` when it is a plain function taking `(http.ResponseWriter,
/// *http.Request)` through an import of `net/http`.
pub(super) fn handler_function(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    simple: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<HandlerFunction> {
    if function.kind() != "function_declaration" || function.child_by_field_name("result").is_some()
    {
        return None;
    }
    let http = bindings
        .iter()
        .find(|(_, binding)| binding.path == "net/http")
        .map(|(local, _)| local.as_str())?;
    let parameters = function.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let types: Vec<String> = parameters
        .named_children(&mut cursor)
        .filter(|p| p.kind() == "parameter_declaration")
        .flat_map(|declaration| {
            // `w, x T` declares two parameters of one type.
            let mut cursor = declaration.walk();
            let names = declaration
                .children_by_field_name("name", &mut cursor)
                .count()
                .max(1);
            let kind = declaration
                .child_by_field_name("type")
                .map(|t| cx.text(&t).split_whitespace().collect::<String>())
                .unwrap_or_default();
            std::iter::repeat_n(kind, names)
        })
        .collect();
    let expected = [format!("{http}.ResponseWriter"), format!("*{http}.Request")];
    (types[..] == expected[..]).then(|| HandlerFunction {
        symbol: symbol.to_owned(),
        path: cx.path.to_owned(),
        exported: simple.starts_with(|c: char| c.is_ascii_uppercase()),
        prov: cx.prov(&function, &format!("Vercel function {symbol}")),
    })
}

/// The path Vercel serves a function file at: `api/a/index.go` at
/// `/api/a`, `api/a/b.go` at `/api/a/b`. `None` outside `api/`.
fn served_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("api/")?;
    let rest = rest.strip_suffix(".go")?;
    if rest.ends_with("_test")
        || rest
            .split('/')
            .any(|s| s.is_empty() || s.starts_with(['_', '.']))
    {
        return None;
    }
    let route = match rest.strip_suffix("index") {
        Some("") => String::new(),
        Some(dir) if dir.ends_with('/') => dir.trim_end_matches('/').to_owned(),
        _ => rest.to_owned(),
    };
    Some(if route.is_empty() {
        "/api".to_owned()
    } else {
        format!("/api/{route}")
    })
}

fn provenance(evidence: EvidenceRef, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            confidence,
            vec![evidence],
            super::EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("Deterministic confidence is within its ceiling"),
    )
    .expect("provenance serializes")
}

/// The function's own evidence, cited again for a fact derived from it.
fn cited(function: &HandlerFunction) -> Option<EvidenceRef> {
    serde_json::from_value::<Provenance>(function.prov.clone())
        .ok()?
        .evidence
        .into_iter()
        .next()
}

fn endpoint(
    repo: &str,
    path: &str,
    symbol: &str,
    prov: serde_json::Value,
    extra: serde_json::Value,
) -> (Node, Edge) {
    let id = format!("ep:{repo}@ANY:{path}");
    let mut props = serde_json::json!({
        "method": "ANY",
        "path": path,
        "framework": "vercel",
        "language": "go",
        "handler_sym": symbol,
        "prov": prov.clone(),
    });
    if let (Some(props), Some(extra)) = (props.as_object_mut(), extra.as_object()) {
        props.extend(extra.clone());
    }
    (
        Node {
            id: id.clone(),
            label: "Endpoint".into(),
            props,
        },
        Edge {
            src: id,
            dst: symbol.to_owned(),
            label: "HANDLES".into(),
            props: serde_json::json!({ "prov": prov }),
        },
    )
}

/// The root `vercel.json`, read only when it is a regular, non-ignored file
/// of the captured source (ADR-0030, ADR-0034).
fn read_config(root: &Path) -> Option<Vec<u8>> {
    source_walk::root_file(root, CONFIG)
}

const CONFIG: &str = "vercel.json";

/// The functions' and rewrites' Endpoints, when `root` is a Vercel project.
pub(super) fn routes(
    root: &Path,
    repo: &str,
    commit: &str,
    functions: &[HandlerFunction],
    out: &mut Extraction,
) {
    let Some(config) = read_config(root) else {
        return;
    };
    // One exported handler per function file is the function Vercel serves.
    let mut by_file: BTreeMap<&str, Vec<&HandlerFunction>> = BTreeMap::new();
    for function in functions.iter().filter(|f| f.exported) {
        by_file.entry(&function.path).or_default().push(function);
    }
    let mut served: BTreeMap<String, &HandlerFunction> = BTreeMap::new();
    for (path, candidates) in by_file {
        let (Some(route), [function]) = (served_path(path), &candidates[..]) else {
            continue;
        };
        let Some(evidence) = cited(function) else {
            continue;
        };
        let (node, edge) = endpoint(
            repo,
            &route,
            &function.symbol,
            provenance(
                evidence,
                ConfidenceTier::Confirmed,
                &format!("Endpoint ANY {route}"),
            ),
            serde_json::json!({}),
        );
        out.nodes.push(node);
        out.edges.push(edge);
        served.insert(route, function);
    }
    for rewrite in adapters_fw::vercel::rewrites(&config) {
        let (source, destination, target) = (
            rewrite.source.as_str(),
            rewrite.destination.as_str(),
            rewrite.target.as_str(),
        );
        let start = rewrite.start;
        let evidence = EvidenceRef {
            repo: repo.into(),
            path: CONFIG.into(),
            byte_start: rewrite.start as u64,
            byte_end: rewrite.end as u64,
            commit_sha: commit.into(),
        };
        let function = served.get(target).copied();
        let paths = adapters_fw::vercel::source_paths(source);
        match (function, paths) {
            (Some(function), Some(paths)) => {
                for path in paths {
                    if served.contains_key(&path) {
                        continue;
                    }
                    let (node, edge) = endpoint(
                        repo,
                        &path,
                        &function.symbol,
                        provenance(
                            evidence.clone(),
                            ConfidenceTier::Confirmed,
                            &format!("Endpoint ANY {path} rewrites to {target}"),
                        ),
                        serde_json::json!({ "rewrite_of": target }),
                    );
                    out.nodes.push(node);
                    out.edges.push(edge);
                }
            }
            (None, _) if !target.starts_with("/api") => {}
            // A JavaScript or TypeScript function's rewrite is that adapter's.
            (None, _) if adapters_fw::vercel::node_function_serves(root, target) => {}
            (function, _) => {
                let reason = if function.is_none() {
                    "Vercel rewrite destination is not a recovered function"
                } else {
                    "Vercel rewrite source pattern is not read"
                };
                let id = format!("gap:vercel-rewrite:{repo}@vercel.json@{start}");
                out.nodes.push(Node {
                    id: id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "source": source,
                        "destination": destination,
                        "attempted_tiers": ["T0"],
                        "prov": provenance(evidence, ConfidenceTier::Gap, &format!("Gap {id}")),
                    }),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IncrementalCache, SourceId, extract_dir, extract_dir_incremental};

    const LOGIN: &str = r#"package handler

import (
	"net/http"

	"example.com/bot/pkg/auth"
)

// Handler serves GET /api/auth/login.
func Handler(w http.ResponseWriter, r *http.Request) { serve(w, r) }

func serve(w http.ResponseWriter, r *http.Request) { auth.Login(w, r) }
"#;

    const DOCS: &str = r#"package handler

import web "net/http"

func Docs(w web.ResponseWriter, r *web.Request) {}

func helper(x int) int { return x }
"#;

    // AC-0924 (#2010): Vercel Go functions are Endpoints at their path, and
    // `vercel.json` rewrites onto them are Endpoints at their sources.
    #[test]
    fn vercel_functions_and_rewrites_are_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        };
        write("go.mod", "module example.com/bot\n\ngo 1.22\n");
        write("api/auth/login/index.go", LOGIN);
        write("api/docs/index.go", DOCS);
        write(
            "api/docs/index_test.go",
            DOCS.replace("Docs", "TestDocs").as_str(),
        );
        write("pkg/web/server.go", DOCS);
        // A TypeScript function's rewrite is the TS adapter's (AC-0927).
        write("api/hooks.ts", "export default function h(req, res) {}\n");
        write(
            "vercel.json",
            r#"{"rewrites": [
  {"source": "/hooks", "destination": "/api/hooks"},
  {"source": "/", "destination": "/api/docs"},
  {"source": "/docs", "destination": "/api/docs"},
  {"source": "/docs/(.*)", "destination": "/api/docs?x=1"},
  {"source": "/devices/(revoke|admin/migrate)", "destination": "/api/devices"},
  {"source": "/(.*)", "destination": "/index.html"},
  {"source": "/a/(?!b).*", "destination": "/api/docs"}
]}"#,
        );
        let id = SourceId {
            repo: "example/bot",
            commit: "c0ffee",
        };
        let extraction = extract_dir(dir.path(), &id).unwrap();
        let mut routes: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|n| n.label == "Endpoint" && n.props["framework"] == "vercel")
            .map(|n| {
                let handler = extraction
                    .edges
                    .iter()
                    .find(|e| e.label == "HANDLES" && e.src == n.id)
                    .map(|e| e.dst.rsplit('#').next().unwrap().to_owned())
                    .unwrap();
                format!(
                    "{} {} -> {handler}{}",
                    n.props["method"].as_str().unwrap(),
                    n.props["path"].as_str().unwrap(),
                    n.props["rewrite_of"]
                        .as_str()
                        .map(|r| format!(" (rewrites to {r})"))
                        .unwrap_or_default()
                )
            })
            .collect();
        routes.sort();
        assert_eq!(
            routes,
            [
                "ANY / -> Docs (rewrites to /api/docs)",
                "ANY /api/auth/login -> Handler",
                "ANY /api/docs -> Docs",
                "ANY /docs -> Docs (rewrites to /api/docs)",
                "ANY /docs/{path...} -> Docs (rewrites to /api/docs)",
            ]
        );
        let mut gaps: Vec<&str> = extraction
            .nodes
            .iter()
            .filter(|n| n.label == "Gap" && n.id.starts_with("gap:vercel-rewrite:"))
            .map(|n| n.props["reason"].as_str().unwrap())
            .collect();
        gaps.sort();
        assert_eq!(
            gaps,
            [
                "Vercel rewrite destination is not a recovered function",
                "Vercel rewrite source pattern is not read",
            ],
            "the missing /api/devices function and the lookahead source are Gaps; the static fallback is not"
        );

        // A reused file's function still routes, at the new commit.
        let mut cache = IncrementalCache::default();
        extract_dir_incremental(dir.path(), &id, &mut cache).unwrap();
        let next = SourceId {
            repo: "example/bot",
            commit: "beef",
        };
        let (again, stats) = extract_dir_incremental(dir.path(), &next, &mut cache).unwrap();
        assert!(stats.reused_files > 0);
        let login = again
            .nodes
            .iter()
            .find(|n| n.id == "ep:example/bot@ANY:/api/auth/login")
            .expect("the reused function routes");
        assert_eq!(login.props["prov"]["evidence"][0]["commit_sha"], "beef");

        // An ignored or symlinked vercel.json is not read: only the
        // captured source's own file makes a Vercel project.
        let vercel = |root: &Path| {
            extract_dir(root, &id)
                .unwrap()
                .nodes
                .iter()
                .any(|n| n.props["framework"] == "vercel")
        };
        write(".gitignore", "vercel.json\n");
        assert!(!vercel(dir.path()));
        std::fs::remove_file(dir.path().join(".gitignore")).unwrap();
        #[cfg(unix)]
        {
            let config = std::fs::read(dir.path().join("vercel.json")).unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("vercel.json"), &config).unwrap();
            std::fs::remove_file(dir.path().join("vercel.json")).unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("vercel.json"),
                dir.path().join("vercel.json"),
            )
            .unwrap();
            assert!(!vercel(dir.path()));
        }

        // Without vercel.json there are no Vercel routes.
        std::fs::remove_file(dir.path().join("vercel.json")).unwrap();
        assert!(!vercel(dir.path()));
    }

    #[test]
    fn function_files_are_served_at_their_directory() {
        assert_eq!(
            served_path("api/auth/login/index.go").as_deref(),
            Some("/api/auth/login")
        );
        assert_eq!(served_path("api/index.go").as_deref(), Some("/api"));
        assert_eq!(
            served_path("api/hooks/batch.go").as_deref(),
            Some("/api/hooks/batch")
        );
        assert_eq!(served_path("api/hooks/index_test.go"), None);
        assert_eq!(served_path("api/_lib/util.go"), None);
        assert_eq!(served_path("pkg/api/index.go"), None);
    }
}
