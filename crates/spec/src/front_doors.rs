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
//! segment (a root catch-all is a single-page app's shell, not proof), and
//! no other function's path is more specific for it. Its endpoints are left out of Capability grouping
//! and stated, with the routes they serve, in D1.

use core_graph::Node;
use std::collections::BTreeMap;

/// The repository an endpoint id names (`ep:<repo>@<METHOD>:<path>`).
fn repo(endpoint: &Node) -> Option<&str> {
    endpoint
        .id
        .strip_prefix("ep:")?
        .split_once('@')
        .map(|(r, _)| r)
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
/// function serves behind it.
pub(crate) fn vercel_front_doors<'a>(
    endpoints: impl IntoIterator<Item = &'a Node>,
) -> BTreeMap<&'a str, usize> {
    // One function's endpoints, keyed by repository and handler.
    let mut functions: BTreeMap<(&str, &str), Vec<&Node>> = BTreeMap::new();
    let mut routes: Vec<(&str, &str)> = Vec::new();
    for endpoint in endpoints.into_iter().filter(|n| n.label == "Endpoint") {
        let (Some(repo), Some(path)) = (repo(endpoint), endpoint.props["path"].as_str()) else {
            continue;
        };
        if endpoint.props["framework"] == "vercel" {
            if let Some(handler) = endpoint.props["handler_sym"].as_str() {
                functions.entry((repo, handler)).or_default().push(endpoint);
            }
        } else {
            routes.push((repo, path));
        }
    }
    let mut served: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (repo, path) in routes {
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
        if let Some((_, key)) = best {
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

    fn endpoint(method: &str, path: &str, framework: &str, handler: &str) -> Node {
        Node {
            id: format!("ep:org/bot@{method}:{path}"),
            label: "Endpoint".into(),
            props: json!({
                "method": method,
                "path": path,
                "framework": framework,
                "handler_sym": handler,
            }),
        }
    }

    // AC-0926: a function whose paths hold mux routes fronts them; the most
    // specific function takes a route; a root catch-all and a function with
    // no route behind it are operations of their own.
    #[test]
    fn a_vercel_function_with_routes_behind_it_is_a_front_door() {
        let nodes = [
            endpoint("ANY", "/api/web", "vercel", "sym:web#Handler"),
            endpoint("ANY", "/api/web/{path...}", "vercel", "sym:web#Handler"),
            endpoint("ANY", "/{path...}", "vercel", "sym:shell#Handler"),
            endpoint("ANY", "/api/auth/login", "vercel", "sym:login#Handler"),
            endpoint("GET", "/api/web/session", "net/http", "sym:a"),
            endpoint(
                "POST",
                "/api/web/apps/{harness}/screens",
                "net/http",
                "sym:b",
            ),
            endpoint("GET", "/healthz", "net/http", "sym:c"),
        ];
        let doors = vercel_front_doors(&nodes);
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
