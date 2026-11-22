//! Vercel functions that only pass requests on to the routes behind them
//! (#2010, AC-0926).
//!
//! A Vercel Go function such as `api/web/index.go` is often a front door: its
//! one `Handler` builds a `net/http` mux, maybe behind a session gate, and
//! serves it, so every operation is a mux route under the function's paths
//! (`ANY /api/web`, `ANY /api/web/{path...}` from a rewrite). The function's
//! own endpoints are then not operations of their own, and grouping them
//! into a Capability ("Manage webs") repeats the routes as noise.
//!
//! A function is a front door here when some other HTTP endpoint of the same
//! repository lies under one of its paths, below at least one literal
//! segment (a root catch-all is a single-page app's shell, not proof), no
//! other function's path is more specific for it, and the endpoint is
//! registered in the function's own directory or in a package the
//! function's file imports: a path alone may be a page the function serves
//! itself (`/devices/{id}` from a shell that renders the app). Its endpoints are left out of Capability grouping
//! and stated, with the routes they serve, in D1.

use core_graph::{Edge, Node};
use std::collections::{BTreeMap, BTreeSet};

/// The repository an endpoint id names (`ep:<repo>@<METHOD>:<path>`).
fn repo(endpoint: &Node) -> Option<&str> {
    endpoint
        .id
        .strip_prefix("ep:")?
        .split_once('@')
        .map(|(r, _)| r)
}

/// The directory of the file an endpoint is registered in, from its
/// provenance.
fn registered_in(endpoint: &Node) -> Option<&str> {
    let path = crate::workbench::stored_spans(&endpoint.props)
        .first()?
        .path?;
    Some(path.rsplit_once('/').map_or("", |(dir, _)| dir))
}

/// How specific `pattern` is for `path` (its literal segments) when it
/// matches: `{name}` is one segment, a final `{name...}` any rest, and a
/// literal must equal the path's segment (a path's own `{param}` is matched
/// only by a pattern parameter or the rest).
fn covers(pattern: &str, path: &str) -> Option<usize> {
    let pattern: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut literal = 0;
    for (i, segment) in pattern.iter().enumerate() {
        if segment.starts_with('{') && segment.ends_with("...}") {
            return (i + 1 == pattern.len()).then_some(literal);
        }
        let given = path.get(i)?;
        if segment.starts_with('{') && segment.ends_with('}') {
            continue;
        }
        if segment != given {
            return None;
        }
        literal += 1;
    }
    (pattern.len() == path.len()).then_some(literal)
}

/// Each front-door function endpoint's id, with the number of routes its
/// function serves behind it. `edges` gives the `IMPORTS` of each file.
pub(crate) fn vercel_front_doors<'a>(
    endpoints: impl IntoIterator<Item = &'a Node>,
    edges: impl IntoIterator<Item = &'a Edge>,
) -> BTreeMap<&'a str, usize> {
    // One function's endpoints, keyed by repository and handler.
    let mut functions: BTreeMap<(&str, &str), Vec<&Node>> = BTreeMap::new();
    let mut routes: Vec<(&str, &str, &str)> = Vec::new();
    for endpoint in endpoints.into_iter().filter(|n| n.label == "Endpoint") {
        let (Some(repo), Some(path)) = (repo(endpoint), endpoint.props["path"].as_str()) else {
            continue;
        };
        if endpoint.props["framework"] == "vercel" {
            if let Some(handler) = endpoint.props["handler_sym"].as_str() {
                functions.entry((repo, handler)).or_default().push(endpoint);
            }
        } else if let Some(dir) = registered_in(endpoint) {
            routes.push((repo, path, dir));
        }
    }
    if functions.is_empty() {
        return BTreeMap::new();
    }
    // The repository directories each file imports, as the Go adapter
    // resolved them against the module's own path (`package_dir`).
    let imports: BTreeSet<(&str, &str)> = edges
        .into_iter()
        .filter(|e| e.label == "IMPORTS")
        .filter_map(|e| {
            Some((
                e.src.strip_prefix("file:")?,
                e.props["package_dir"].as_str()?,
            ))
        })
        .collect();
    // The function's file (`sym:<repo>@<file>#<name>`) is in `dir`'s package
    // or imports it.
    let reaches = |handler: &str, repo: &str, dir: &str| {
        let Some(file) = handler
            .strip_prefix("sym:")
            .and_then(|s| s.split_once('#'))
            .map(|(file, _)| file)
        else {
            return false;
        };
        let own = file
            .strip_prefix(repo)
            .and_then(|f| f.strip_prefix('@'))
            .map(|f| f.rsplit_once('/').map_or("", |(d, _)| d));
        own == Some(dir) || imports.contains(&(file, dir))
    };
    let mut served: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (repo, path, dir) in routes {
        let best = functions
            .iter()
            .filter(|((r, _), _)| *r == repo)
            .filter_map(|(key, endpoints)| {
                endpoints
                    .iter()
                    .filter_map(|e| covers(e.props["path"].as_str()?, path))
                    // A catch-all at the root (`/{path...}`, a single-page
                    // app's shell) says nothing about what serves a route.
                    .filter(|specific| *specific > 0)
                    .max()
                    .map(|specific| (specific, *key))
            })
            .max_by_key(|(specific, _)| *specific);
        if let Some((_, key)) = best
            && reaches(key.1, repo, dir)
        {
            *served.entry(key).or_default() += 1;
        }
    }
    served
        .into_iter()
        .flat_map(|(key, n)| functions[&key].iter().map(move |e| (e.id.as_str(), n)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn endpoint(method: &str, path: &str, framework: &str, handler: &str, file: &str) -> Node {
        Node {
            id: format!("ep:org/bot@{method}:{path}"),
            label: "Endpoint".into(),
            props: json!({
                "method": method,
                "path": path,
                "framework": framework,
                "handler_sym": handler,
                "prov": {"evidence": [{"path": file}]},
            }),
        }
    }

    // AC-0926: a function whose paths hold routes registered in a package
    // its file imports fronts them; the most specific function takes a
    // route; a root catch-all, a function that does not import the routes'
    // package and one with no route behind it are operations of their own.
    #[test]
    fn a_vercel_function_with_routes_behind_it_is_a_front_door() {
        let web = "sym:org/bot@api/web/index.go#Handler";
        let shell = "sym:org/bot@api/shell/index.go#Handler";
        let nodes = [
            endpoint("ANY", "/api/web", "vercel", web, "api/web/index.go"),
            endpoint("ANY", "/api/web/{path...}", "vercel", web, "vercel.json"),
            endpoint("ANY", "/{path...}", "vercel", shell, "vercel.json"),
            endpoint("ANY", "/devices/{id}", "vercel", shell, "vercel.json"),
            endpoint(
                "ANY",
                "/api/auth/login",
                "vercel",
                "sym:login",
                "api/auth/login/index.go",
            ),
            endpoint(
                "GET",
                "/api/web/session",
                "net/http",
                "sym:a",
                "pkg/device/web.go",
            ),
            endpoint(
                "POST",
                "/api/web/apps/{harness}/screens",
                "net/http",
                "sym:b",
                "pkg/device/web.go",
            ),
            endpoint(
                "GET",
                "/devices/{id}",
                "net/http",
                "sym:c",
                "pkg/device/pages.go",
            ),
            endpoint("GET", "/healthz", "net/http", "sym:d", "pkg/device/web.go"),
            endpoint(
                "GET",
                "/api/web/pages",
                "net/http",
                "sym:e",
                "pkg/pages/web.go",
            ),
        ];
        let import = |dst: &str, dir: Option<&str>| Edge {
            src: "file:org/bot@api/web/index.go".into(),
            dst: dst.into(),
            label: "IMPORTS".into(),
            props: dir.map_or(json!({}), |d| json!({"package_dir": d})),
        };
        // An outside package whose path ends like a local one proves nothing.
        let imports = [
            import("mod:example.com/bot/pkg/device", Some("pkg/device")),
            import("mod:github.com/vendor/pkg/pages", None),
        ];
        let doors = vercel_front_doors(&nodes, &imports);
        assert_eq!(
            doors.into_iter().collect::<Vec<_>>(),
            vec![
                ("ep:org/bot@ANY:/api/web", 2),
                ("ep:org/bot@ANY:/api/web/{path...}", 2),
            ]
        );
        assert_eq!(covers("/api/web/{path...}", "/api/web"), Some(2));
        assert_eq!(covers("/devices/{id}", "/devices/{id}"), Some(1));
        assert_eq!(covers("/devices/manage", "/devices/{id}"), None);
        assert_eq!(covers("/api/web", "/api/web/session"), None);
    }
}
