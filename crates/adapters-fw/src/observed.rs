//! Language-neutral assembly of observed infrastructure (#734): the
//! `Resource` nodes, `USES` edges and Gaps an adapter's proven client sites
//! become. Python uses it; the TypeScript producer keeps an equivalent copy
//! until it moves here, so both languages must keep this shape identical:
//! one resource per provider and instance identity, `props.observed = true`,
//! `kind` = the family, a URL kept as scheme and authority only, an env
//! reference by name only, and a runtime-computed identity as a Gap.

use crate::events::{IdentityExpr, ObservedFamily, RegistryIdentity};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeMap;

/// Longest literal identity kept on a resource id.
pub const MAX_IDENTITY: usize = 128;

/// One proven client site before identities are grouped into resources.
#[derive(Debug, Clone)]
pub struct ObservedSite {
    /// Infrastructure family.
    pub family: ObservedFamily,
    /// Provider recorded as the resource type.
    pub provider: String,
    /// Module the client was imported from, when recorded.
    pub module: Option<String>,
    /// Instance identity, when the client declares one.
    pub identity: Option<IdentityExpr>,
    /// Where a followed identity is defined, when outside the site (path
    /// and span): cited beside the site on the resource and `USES` edge.
    pub identity_source: Option<(String, u64, u64)>,
    /// An assumption the identity rests on (a marker name): the resource
    /// and `USES` edge are then `InferredStrong`, marked, not `Confirmed`.
    pub identity_assumption: Option<&'static str>,
    /// Registry file that declared the client, when the site is table-driven.
    pub registry: Option<RegistryIdentity>,
    /// Enclosing symbol id; the file when the site is at module level.
    pub symbol: Option<String>,
    /// Repo-relative file path.
    pub path: String,
    /// Site span.
    pub byte_start: u64,
    /// Site span end.
    pub byte_end: u64,
}

/// Where the sites were recovered from.
pub struct ObservedSource<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir`.
    pub commit: &'a str,
    /// Extractor recorded on every provenance.
    pub extractor: &'a str,
}

fn prov_value(
    source: &ObservedSource,
    path: &str,
    span: (u64, u64),
    also: Option<&(String, u64, u64)>,
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let evidence = |path: &str, start: u64, end: u64| EvidenceRef {
        repo: source.repo.into(),
        path: path.into(),
        byte_start: start,
        byte_end: end,
        commit_sha: source.commit.into(),
    };
    let mut spans = vec![evidence(path, span.0, span.1)];
    spans.extend(also.map(|(path, start, end)| evidence(path, *start, *end)));
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        spans,
        source.extractor,
        fact.as_bytes(),
    )
    .expect("Deterministic within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// A computed identity's source text as a Gap may keep it: URL userinfo
/// removed, recognized secrets redacted, at most [`MAX_IDENTITY`] bytes.
pub fn withheld_raw(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#' | '"' | '\'' | '`') || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(authority.rsplit('@').next().unwrap_or(authority));
        rest = &tail[end..];
    }
    out.push_str(rest);
    let (mut out, _) = core_redact::redact_text(&out);
    if out.len() > MAX_IDENTITY {
        let mut cut = MAX_IDENTITY;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// A literal identity as recorded: a URL keeps its scheme and authority
/// without credentials; anything else is kept up to [`MAX_IDENTITY`].
/// `None` for a value that cannot be recorded safely (empty, too long,
/// token-shaped or a recognized secret): the Gap then withholds it whole.
pub fn recorded_literal(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !core_redact::recognized_secrets(value).is_empty() {
        return None;
    }
    let recorded = match value.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            let host = authority.rsplit('@').next().unwrap_or(authority);
            if host.is_empty() {
                return None;
            }
            format!("{scheme}://{host}")
        }
        None if core_redact::is_token_shaped(value) => return None,
        None => value.to_string(),
    };
    (recorded.len() <= MAX_IDENTITY).then_some(recorded)
}

/// Resources, `USES` edges and Gaps for the collected sites.
pub fn assemble(source: &ObservedSource, sites: &[ObservedSite]) -> (Vec<Node>, Vec<Edge>) {
    let repo = source.repo;
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut resources: BTreeMap<String, Node> = BTreeMap::new();
    for site in sites {
        let span = (site.byte_start, site.byte_end);
        let src = site
            .symbol
            .clone()
            .unwrap_or_else(|| format!("file:{repo}@{}", site.path));
        let mut edge_props = serde_json::Map::new();
        if let Some(registry) = &site.registry {
            edge_props.insert("registry".into(), serde_json::json!(registry.name));
        }
        // Instance identity: `None` = the provider alone, `Err` = a Gap
        // (reason, raw text a Gap may keep). A literal that cannot be
        // recorded is withheld whole, never kept as raw.
        let identity = match &site.identity {
            None => Ok(None),
            Some(IdentityExpr::Literal(value)) => match recorded_literal(value) {
                Some(value) => Ok(Some((
                    value.clone(),
                    serde_json::json!({ "identity": value }),
                ))),
                None => Err((
                    format!(
                        "{} identity withheld: literal is not recordable",
                        site.provider
                    ),
                    None,
                )),
            },
            Some(IdentityExpr::EnvRef(key)) => Ok(Some((
                format!("env:{key}"),
                serde_json::json!({ "env_key": key }),
            ))),
            Some(IdentityExpr::Computed(raw)) => Err((
                format!("runtime-computed {} identity", site.provider),
                Some(withheld_raw(raw)),
            )),
        };
        match identity {
            Ok(instance) => {
                let confidence = match site.identity_assumption {
                    Some(_) => ConfidenceTier::InferredStrong,
                    None => ConfidenceTier::Confirmed,
                };
                let logical_id = match &instance {
                    Some((name, _)) => format!("observed:{}:{name}", site.provider),
                    None => format!("observed:{}", site.provider),
                };
                let resource_id = format!("res:{repo}@{logical_id}");
                resources.entry(resource_id.clone()).or_insert_with(|| {
                    let mut props = serde_json::json!({
                        "type": site.provider,
                        "provider": site.provider,
                        "kind": site.family.as_str(),
                        "observed": true,
                        "logical_id": logical_id,
                        "prov": prov_value(
                            source, &site.path, span, site.identity_source.as_ref(),
                            confidence, &format!("Resource {resource_id}"),
                        ),
                    });
                    if let Some(module) = &site.module {
                        props["module"] = serde_json::json!(module);
                    }
                    if let Some((_, extra)) = &instance
                        && let (Some(props), Some(extra)) =
                            (props.as_object_mut(), extra.as_object())
                    {
                        props.extend(extra.clone());
                    }
                    Node {
                        id: resource_id.clone(),
                        label: "Resource".into(),
                        props,
                    }
                });
                if let Some((path, _, _)) = &site.identity_source {
                    edge_props.insert("identity_from".into(), serde_json::json!(path));
                }
                if let Some(marker) = site.identity_assumption {
                    edge_props.insert(marker.into(), serde_json::json!(true));
                }
                edge_props.insert(
                    "prov".into(),
                    prov_value(
                        source,
                        &site.path,
                        span,
                        site.identity_source.as_ref(),
                        confidence,
                        &format!("USES {src} -> {resource_id}"),
                    ),
                );
                edges.push(Edge {
                    src,
                    dst: resource_id,
                    label: "USES".into(),
                    props: serde_json::Value::Object(edge_props),
                });
            }
            Err((reason, raw)) => {
                let gap_id = format!("gap:observed:{repo}@{}@{}", site.path, site.byte_start);
                nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "raw": raw,
                        "kind": site.family.as_str(),
                        "provider": site.provider,
                        "attempted_tiers": ["T0"],
                        "prov": prov_value(
                            source, &site.path, span, None, ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                edge_props.insert("reason".into(), serde_json::json!(reason));
                edge_props.insert(
                    "prov".into(),
                    prov_value(
                        source,
                        &site.path,
                        span,
                        None,
                        ConfidenceTier::Gap,
                        &format!("USES {src} -> {gap_id}"),
                    ),
                );
                edges.push(Edge {
                    src,
                    dst: gap_id,
                    label: "USES".into(),
                    props: serde_json::Value::Object(edge_props),
                });
            }
        }
    }
    nodes.extend(resources.into_values());
    (nodes, edges)
}
