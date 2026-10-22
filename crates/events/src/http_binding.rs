//! Cross-repo HTTP stitching on binding evidence (#589, AC-0240/AC-0241).
//!
//! Each repo stitches its own client calls first (`stitch_fetches`). After
//! every repo of a system is ingested, a fetch Gap whose absolute URL is
//! covered by author-declared binding evidence — a service's base URL or a
//! gateway route in the system manifest — is joined to the one endpoint it
//! reaches in the serving repo, and the edge cites that evidence. Without
//! binding evidence, or with several equally supported targets, the call
//! stays a Gap whose reason says why. No new tier, no network probing.

use super::{route_matches, url_path};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use std::collections::BTreeMap;

/// Extractor id of a cross-repo `FETCHES` edge.
pub const EXTRACTOR_ID: &str = "t0.http-cross-repo";

/// Stop reason of an absolute-URL call whose origin the system manifest
/// binds: the per-repo pass defers it instead of matching a same-repo route
/// by path alone, and the system pass decides it on the binding.
pub const DEFERRED_REASON_PREFIX: &str = "deferred to system binding:";

/// Origins (`scheme://host`) the bindings cover, for the per-repo pass to
/// defer (`ConfigIndex::defer_origins`).
pub fn binding_origins(bindings: &[HttpBinding]) -> std::collections::BTreeSet<String> {
    bindings
        .iter()
        .filter_map(|binding| origin(&binding.base_url))
        .collect()
}

/// `scheme://host` of an absolute URL, lowercased, when it has one.
pub fn url_origin(url: &str) -> Option<String> {
    origin(url)
}

/// How a binding reaches its service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingKind {
    /// A declared service's own base URL.
    DeclaredService,
    /// A gateway route forwarding `prefix` to the service.
    GatewayRoute {
        /// Routed path prefix.
        prefix: String,
        /// Whether the gateway removes the prefix before forwarding.
        strip_prefix: bool,
    },
}

/// One piece of author-declared binding evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpBinding {
    /// Declared service name.
    pub service: String,
    /// Repo key of the repo serving it.
    pub repo: String,
    /// Absolute base URL the evidence covers.
    pub base_url: String,
    /// Direct service URL or gateway route.
    pub kind: BindingKind,
    /// Where it was declared, e.g. `cartograph.system.toml`.
    pub declared_in: String,
}

impl HttpBinding {
    /// The citation recorded on a bound edge or a candidate.
    fn citation(&self) -> serde_json::Value {
        match &self.kind {
            BindingKind::DeclaredService => serde_json::json!({
                "kind": "declared-service",
                "service": self.service,
                "repo": self.repo,
                "base_url": self.base_url,
                "declared_in": self.declared_in,
            }),
            BindingKind::GatewayRoute {
                prefix,
                strip_prefix,
            } => serde_json::json!({
                "kind": "gateway-route",
                "service": self.service,
                "repo": self.repo,
                "base_url": self.base_url,
                "prefix": prefix,
                "strip_prefix": strip_prefix,
                "declared_in": self.declared_in,
            }),
        }
    }

    /// The paths the serving repo may receive for `url`, empty when this
    /// binding does not cover it. A declared service whose base URL has a
    /// path may serve routes with or without that path, so both are offered;
    /// if they reach different endpoints the call is ambiguous, never guessed.
    fn forwarded_paths(&self, url: &str) -> Vec<String> {
        let base = normalize(self.base_url.trim_end_matches('/'));
        let url = normalize(url.split(['?', '#']).next().unwrap_or(url));
        let Some(rest) = url.strip_prefix(&base) else {
            return Vec::new();
        };
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Vec::new();
        }
        let path = url_path(&url);
        let relative = if rest.is_empty() { "/" } else { rest };
        match &self.kind {
            BindingKind::DeclaredService => {
                let mut paths = vec![path];
                if paths[0] != relative {
                    paths.push(relative.to_string());
                }
                paths
            }
            BindingKind::GatewayRoute {
                prefix,
                strip_prefix,
            } => {
                // A root prefix covers every path under the base URL.
                let segment = prefix.trim_matches('/');
                let routed = if segment.is_empty() {
                    rest
                } else {
                    let Some(routed) = rest.strip_prefix(format!("/{segment}").as_str()) else {
                        return Vec::new();
                    };
                    if !(routed.is_empty() || routed.starts_with('/')) {
                        return Vec::new();
                    }
                    routed
                };
                vec![if *strip_prefix {
                    if routed.is_empty() {
                        "/".into()
                    } else {
                        routed.into()
                    }
                } else {
                    path
                }]
            }
        }
    }
}

/// Lowercase the scheme and host of an absolute URL; the path keeps case.
fn normalize(url: &str) -> String {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            let host_end = rest.find('/').unwrap_or(rest.len());
            format!(
                "{}://{}{}",
                url[..i].to_ascii_lowercase(),
                rest[..host_end].to_ascii_lowercase(),
                &rest[host_end..]
            )
        }
        None => url.to_string(),
    }
}

/// `scheme://host` of an absolute URL.
fn origin(url: &str) -> Option<String> {
    let i = url.find("://")?;
    let rest = &url[i + 3..];
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| normalize(&format!("{}://{host}", &url[..i])))
}

/// A fetch Gap joined to its endpoint.
#[derive(Debug, Clone)]
pub struct JoinedFetch {
    /// The Gap node the join replaces (with its incident Gap edge).
    pub gap_id: String,
    /// The bound `FETCHES` edge.
    pub edge: Edge,
}

/// The outcome of one system-wide pass.
#[derive(Debug, Default)]
pub struct CrossRepoStitch {
    /// Gaps replaced by a bound edge.
    pub joined: Vec<JoinedFetch>,
    /// Gap nodes kept with a cross-repo reason.
    pub kept_nodes: Vec<Node>,
    /// Their Gap edges, with the same reason.
    pub kept_edges: Vec<Edge>,
}

/// Join absolute-URL fetch Gaps across repos on binding evidence. Gaps
/// without a recorded absolute request, a known method, or an unmatched
/// same-repo stop reason are left untouched. Deterministic in Gap order;
/// work is proportional to call sites × bindings for their origin.
pub fn stitch_cross_repo(
    gaps: &[Node],
    fetch_edges: &[Edge],
    endpoint_ids: &[String],
    bindings: &[HttpBinding],
) -> CrossRepoStitch {
    let mut out = CrossRepoStitch::default();
    if bindings.is_empty() {
        return out;
    }
    // ep:{repo}@VERB:route, indexed by (repo, verb).
    let mut endpoints: BTreeMap<(&str, &str), Vec<(&str, &str)>> = BTreeMap::new();
    for id in endpoint_ids {
        let Some(rest) = id.strip_prefix("ep:") else {
            continue;
        };
        let Some((repo, verb_route)) = rest.split_once('@') else {
            continue;
        };
        let Some((verb, route)) = verb_route.split_once(':') else {
            continue;
        };
        endpoints
            .entry((repo, verb))
            .or_default()
            .push((id.as_str(), route));
    }
    let mut by_origin: BTreeMap<String, Vec<&HttpBinding>> = BTreeMap::new();
    for binding in bindings {
        if let Some(origin) = origin(&binding.base_url) {
            by_origin.entry(origin).or_default().push(binding);
        }
    }
    let mut gap_edges: BTreeMap<&str, &Edge> = BTreeMap::new();
    for edge in fetch_edges {
        gap_edges.entry(edge.dst.as_str()).or_insert(edge);
    }

    let mut gaps: Vec<&Node> = gaps
        .iter()
        .filter(|gap| gap.label == "Gap" && gap.id.starts_with("gap:fetch:"))
        .collect();
    gaps.sort_by(|left, right| left.id.cmp(&right.id));
    for gap in gaps {
        let request = &gap.props["request"];
        let (Some(method), Some(url)) = (request["method"].as_str(), request["url"].as_str())
        else {
            continue;
        };
        let reason = gap.props["reason"].as_str().unwrap_or_default();
        let unmatched = reason.starts_with("no recovered endpoint matches")
            || reason.starts_with(DEFERRED_REASON_PREFIX)
            || gap.props["t0_reason"].is_string();
        let (Some(origin), Some(gap_edge)) = (origin(url), gap_edges.get(gap.id.as_str())) else {
            continue;
        };
        if method == "?" || !unmatched {
            continue;
        }
        let url_source = request["url_source"].as_str().unwrap_or("literal");

        let mut covering = Vec::new();
        let mut targets: BTreeMap<&str, Vec<&HttpBinding>> = BTreeMap::new();
        for binding in by_origin.get(&origin).into_iter().flatten() {
            let paths = binding.forwarded_paths(url);
            if paths.is_empty() {
                continue;
            }
            covering.push(*binding);
            for (endpoint, route) in endpoints
                .get(&(binding.repo.as_str(), method))
                .into_iter()
                .flatten()
            {
                if paths.iter().any(|path| route_matches(route, path)) {
                    let supporting = targets.entry(endpoint).or_default();
                    if !supporting.contains(binding) {
                        supporting.push(binding);
                    }
                }
            }
        }

        if let [(endpoint, supporting)] = targets.iter().collect::<Vec<_>>().as_slice() {
            let citations: Vec<serde_json::Value> = supporting
                .iter()
                .map(|binding| binding.citation())
                .collect();
            let evidence = serde_json::from_value::<Provenance>(gap_edge.props["prov"].clone())
                .map(|provenance| provenance.evidence)
                .unwrap_or_default();
            let fact = serde_json::to_vec(&("FETCHES", &gap_edge.src, endpoint, &citations))
                .expect("cross-repo binding serializes");
            let Ok(provenance) = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                evidence,
                EXTRACTOR_ID,
                &fact,
            ) else {
                continue;
            };
            let mut props = serde_json::json!({
                "method": method,
                "resolver": url_source,
                "url": url,
                "binding": citations,
                "prov": provenance,
            });
            // A joined client call (#737) keeps its entry and join citation.
            if let Some(join) = gap.props.get("join") {
                props["join"] = join.clone();
            }
            out.joined.push(JoinedFetch {
                gap_id: gap.id.clone(),
                edge: Edge {
                    src: gap_edge.src.clone(),
                    dst: (*endpoint).to_string(),
                    label: "FETCHES".into(),
                    props,
                },
            });
            continue;
        }

        let path = url_path(url);
        let (kept_reason, candidates) = if covering.is_empty() {
            (
                format!("no binding evidence: no declared service or gateway route covers {url}"),
                Vec::new(),
            )
        } else if targets.is_empty() {
            let mut services: Vec<String> = covering
                .iter()
                .map(|binding| format!("{} ({})", binding.service, binding.repo))
                .collect();
            services.sort();
            services.dedup();
            (
                format!(
                    "binding evidence names {}, but no recovered endpoint there matches {method} {path}",
                    services.join(", ")
                ),
                Vec::new(),
            )
        } else {
            let candidates: Vec<serde_json::Value> = targets
                .iter()
                .map(|(endpoint, supporting)| {
                    serde_json::json!({
                        "endpoint": endpoint,
                        "binding": supporting.iter().map(|binding| binding.citation()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            (
                format!(
                    "ambiguous cross-repo binding for {method} {url}: {} equally supported targets",
                    targets.len()
                ),
                candidates,
            )
        };
        let mut node = gap.clone();
        let original = gap.props["t0_reason"]
            .as_str()
            .unwrap_or(reason)
            .to_string();
        node.props["t0_reason"] = original.clone().into();
        node.props["reason"] = kept_reason.clone().into();
        if candidates.is_empty() {
            if let Some(props) = node.props.as_object_mut() {
                props.remove("candidates");
            }
        } else {
            node.props["candidates"] = candidates.into();
        }
        let mut edge = (*gap_edge).clone();
        edge.props["t0_reason"] = original.into();
        edge.props["reason"] = kept_reason.into();
        out.kept_nodes.push(node);
        out.kept_edges.push(edge);
    }
    out
}
