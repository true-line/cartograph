//! EventBridge T1 matching (#573, SPEC-00 §3 T1).
//!
//! An EventBridge channel is a detail type on a bus within a deployment
//! scope. The T0 registry keys the channel on `DetailType`; an observed
//! span's messaging destination is the bus. The two identify different
//! things, so a bus-only observation never fills a detail-type Gap.
//!
//! A T1 observation fills an EventBridge Gap only when every required
//! component (bus, detail type) is established by literal static evidence
//! or by an observation attributed to the same source site, all known
//! values agree, and exactly one distinct scoped identity results. Each
//! span is evaluated on its own: a bus from one event is never joined with
//! a detail type from another. Unknown scope components are part of the
//! identity value (`?`), so an unscoped observation never merges with a
//! scoped one. Otherwise the Gap stays explicit and names the shortfall.

use super::{
    OTEL_EXTRACTOR_ID, ObservedSpan, ObservedTrace, attr_str, gap_source_path, paths_match,
    trace_prov,
};
use core_graph::{Edge, Node};
use std::collections::{BTreeMap, BTreeSet};

/// Gap/channel kind the T0 EventBridge registry entry emits.
pub(crate) const GAP_KIND: &str = "eventbridge-detail-type";

/// Span attribute carrying the PutEvents entry's detail type. No OTel
/// semantic convention names it yet; this is Cartograph's documented key.
pub const DETAIL_TYPE_ATTR: &str = "aws.eventbridge.detail_type";

/// Reserved id namespace for T1 EventBridge channels. No registry kind is
/// named `eventbridge-scoped`, so no `chan:<kind>:` id can fall inside it.
const COMPOSITE_PREFIX: &str = "chan:eventbridge-scoped:";

const CODE_PATH_ATTRS: &[&str] = &["code.file.path", "code.filepath", "code.file.name"];
const BUS_ATTRS: &[&str] = &[
    "messaging.destination.name",
    "messaging.destination_name",
    "messaging.destination",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Scope {
    account: Option<String>,
    region: Option<String>,
    environment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    scope: Scope,
    bus: String,
    detail_type: String,
}

/// What the source site's T0 facts establish.
struct StaticSite {
    path: String,
    raw: String,
    bus: Option<String>,
    detail_template: Option<Vec<String>>,
    batch: Option<String>,
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_string)
}

fn is_eventbridge(span: &ObservedSpan) -> bool {
    attr_str(span, &["messaging.system"]).is_some_and(|system| {
        matches!(
            system.to_ascii_lowercase().as_str(),
            "aws_eventbridge" | "eventbridge"
        )
    })
}

fn scope(span: &ObservedSpan) -> Scope {
    Scope {
        account: non_empty(attr_str(span, &["cloud.account.id"])),
        region: non_empty(attr_str(span, &["cloud.region"])),
        environment: non_empty(attr_str(
            span,
            &["deployment.environment.name", "deployment.environment"],
        )),
    }
}

/// Encode one identity component; `?` marks an unknown one.
fn enc(value: Option<&str>) -> String {
    match value {
        None => "?".into(),
        Some(v) => v
            .replace('%', "%25")
            .replace('|', "%7C")
            .replace(':', "%3A")
            .replace('?', "%3F"),
    }
}

fn scope_key(scope: &Scope) -> String {
    format!(
        "{}:{}:{}",
        enc(scope.account.as_deref()),
        enc(scope.region.as_deref()),
        enc(scope.environment.as_deref())
    )
}

impl Identity {
    /// Composite channel id for a T1-filled EventBridge Gap. It lives in
    /// its own reserved namespace with named fields, so no T0 id of the
    /// form `chan:<kind>:<identity>` can alias it, whatever a literal
    /// detail type contains. Stored T0 channel ids
    /// (`chan:eventbridge-detail-type:<DetailType>`) are not renamed;
    /// migrating them to the composite form is deferred.
    fn channel_id(&self) -> String {
        format!(
            "{COMPOSITE_PREFIX}detail={}|bus={}|scope={}",
            enc(Some(&self.detail_type)),
            enc(Some(&self.bus)),
            scope_key(&self.scope)
        )
    }
}

fn bus_channel_id(bus: &str, scope: &Scope) -> String {
    format!(
        "{COMPOSITE_PREFIX}bus={}|scope={}",
        enc(Some(bus)),
        scope_key(scope)
    )
}

fn scope_json(scope: &Scope, into: &mut serde_json::Map<String, serde_json::Value>) {
    for (key, value) in [
        ("account", &scope.account),
        ("region", &scope.region),
        ("environment", &scope.environment),
    ] {
        if let Some(value) = value {
            into.insert(key.into(), value.as_str().into());
        }
    }
}

/// Literal fragments of a template-literal expression (`` `order.${k}` ``
/// gives `["order.", ""]`); `None` when the raw text is not one.
fn template_fragments(raw: &str) -> Option<Vec<String>> {
    let inner = raw.strip_prefix('`')?.strip_suffix('`')?;
    let mut fragments = vec![String::new()];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                fragments.last_mut().expect("non-empty").push(next);
            }
        } else if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut depth = 1usize;
            for c in chars.by_ref() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            fragments.push(String::new());
        } else {
            fragments.last_mut().expect("non-empty").push(c);
        }
    }
    Some(fragments)
}

/// Whether `value` can be produced by the template's literal fragments.
fn fits_template(value: &str, fragments: &[String]) -> bool {
    let (Some(first), Some(last)) = (fragments.first(), fragments.last()) else {
        return true;
    };
    if fragments.len() == 1 {
        return value == first;
    }
    let Some(mut rest) = value.strip_prefix(first.as_str()) else {
        return false;
    };
    for middle in &fragments[1..fragments.len() - 1] {
        match rest.find(middle.as_str()) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    rest.ends_with(last.as_str())
}

fn static_site(node: &Node) -> Option<StaticSite> {
    let raw = node
        .props
        .get("raw")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(StaticSite {
        path: gap_source_path(node)?.to_string(),
        bus: node
            .props
            .pointer("/components/bus")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        detail_template: template_fragments(&raw),
        batch: node
            .props
            .get("batch")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        raw,
    })
}

/// One span with the trace file it came from.
#[derive(Clone, Copy)]
struct Seen<'a> {
    span: &'a ObservedSpan,
    trace_path: &'a str,
}

impl Seen<'_> {
    fn key(&self) -> (&str, &str, &str) {
        (self.trace_path, &self.span.trace_id, &self.span.span_id)
    }
}

/// Keep the least `(trace file, trace id, span id)` per key, so the cited
/// evidence does not depend on the order traces are applied in.
fn keep_least<'a, K: Ord>(map: &mut BTreeMap<K, Seen<'a>>, key: K, seen: Seen<'a>) {
    map.entry(key)
        .and_modify(|current| {
            if seen.key() < current.key() {
                *current = seen;
            }
        })
        .or_insert(seen);
}

struct Kept<'a> {
    reason: String,
    bus_observations: BTreeMap<(String, Scope), Seen<'a>>,
}

struct Filled<'a> {
    identity: Identity,
    seen: Seen<'a>,
    bus_from_static: bool,
}

fn decide<'a>(
    site: &StaticSite,
    spans: &[Seen<'a>],
    sites_in_path: usize,
) -> Result<Filled<'a>, Kept<'a>> {
    let kept = |reason: String| Kept {
        reason,
        bus_observations: BTreeMap::new(),
    };
    if spans.is_empty() {
        return Err(kept("no EventBridge observation in trace".into()));
    }
    let attributed: Vec<Seen<'a>> = spans
        .iter()
        .copied()
        .filter(|seen| {
            attr_str(seen.span, CODE_PATH_ATTRS).is_some_and(|path| paths_match(path, &site.path))
        })
        .collect();
    if attributed.is_empty() {
        return Err(kept("source attribution missing".into()));
    }
    if sites_in_path > 1 {
        return Err(kept(format!(
            "source attribution ambiguous: {sites_in_path} EventBridge sites in {}",
            site.path
        )));
    }
    // From here every attributed span belongs to this site, so its bus is
    // kept as its own fact whatever the outcome.
    let mut bus_observations = BTreeMap::<(String, Scope), Seen<'a>>::new();
    for seen in &attributed {
        if let Some(bus) = non_empty(attr_str(seen.span, BUS_ATTRS)) {
            keep_least(&mut bus_observations, (bus, scope(seen.span)), *seen);
        }
    }
    if let Some(batch) = &site.batch {
        return Err(Kept {
            reason: format!("batch correlation unsupported: {batch} in one PutEvents call"),
            bus_observations,
        });
    }

    let mut identities = BTreeMap::<Identity, Seen<'a>>::new();
    let mut contradictions = BTreeSet::<String>::new();
    let mut detail_missing = false;
    for seen in attributed {
        let span = seen.span;
        let scope = scope(span);
        let observed_bus = non_empty(attr_str(span, BUS_ATTRS));
        if let (Some(fixed), Some(observed)) = (&site.bus, &observed_bus)
            && fixed != observed
        {
            contradictions.insert(format!(
                "observed bus {observed} contradicts static bus {fixed}"
            ));
            continue;
        }
        let Some(detail_type) = non_empty(attr_str(span, &[DETAIL_TYPE_ATTR])) else {
            detail_missing = true;
            continue;
        };
        if let Some(fragments) = &site.detail_template
            && !fits_template(&detail_type, fragments)
        {
            contradictions.insert(format!(
                "observed detail type {detail_type} does not fit static {}",
                site.raw
            ));
            continue;
        }
        let Some(bus) = observed_bus.or_else(|| site.bus.clone()) else {
            continue;
        };
        keep_least(
            &mut identities,
            Identity {
                scope,
                bus,
                detail_type,
            },
            seen,
        );
    }

    let reason = if !contradictions.is_empty() {
        format!(
            "contradiction: {}",
            contradictions.into_iter().collect::<Vec<_>>().join("; ")
        )
    } else if identities.len() > 1 {
        format!("{} distinct identities", identities.len())
    } else if let Some((identity, seen)) = identities.into_iter().next() {
        let bus_from_static = site.bus.is_some() && attr_str(seen.span, BUS_ATTRS).is_none();
        return Ok(Filled {
            identity,
            seen,
            bus_from_static,
        });
    } else if detail_missing {
        "detail type unobserved".into()
    } else {
        "bus unobserved".into()
    };
    Err(Kept {
        reason,
        bus_observations,
    })
}

fn observed(span: &ObservedSpan) -> serde_json::Value {
    serde_json::json!({
        "trace_id": span.trace_id,
        "span_id": span.span_id,
        "span_name": span.name,
    })
}

fn evidence_site(props: &serde_json::Value) -> Option<(String, Option<u64>)> {
    let evidence = props.pointer("/prov/evidence/0")?;
    Some((
        evidence.get("path")?.as_str()?.to_string(),
        evidence
            .get("byte_start")
            .and_then(serde_json::Value::as_u64),
    ))
}

/// EventBridge sites per source file: the Gaps plus literal sites already
/// confirmed at T0, whose spans would otherwise look attributed to a
/// neighbouring Gap.
fn sites_per_path(
    nodes: &[Node],
    edges: &[Edge],
    gap_ids: &BTreeSet<&str>,
) -> BTreeMap<String, usize> {
    let t0_channels: BTreeSet<&str> = nodes
        .iter()
        .filter(|node| {
            node.label == "Channel"
                && node.props.get("kind").and_then(serde_json::Value::as_str) == Some(GAP_KIND)
        })
        .map(|node| node.id.as_str())
        .collect();
    let mut sites = BTreeSet::<(String, Option<u64>)>::new();
    for node in nodes
        .iter()
        .filter(|node| gap_ids.contains(node.id.as_str()))
    {
        sites.extend(evidence_site(&node.props));
    }
    for edge in edges.iter().filter(|edge| {
        (gap_ids.contains(edge.dst.as_str()) || t0_channels.contains(edge.dst.as_str()))
            && edge
                .props
                .pointer("/prov/tier")
                .and_then(serde_json::Value::as_str)
                == Some("Deterministic")
    }) {
        sites.extend(evidence_site(&edge.props));
    }
    let mut per_path = BTreeMap::<String, usize>::new();
    for (path, _) in sites {
        *per_path.entry(path).or_default() += 1;
    }
    per_path
}

/// Apply EventBridge observations from every trace file at once, so the
/// one-identity decision sees all of them. Returns `(resolved, unresolved)`
/// Gap counts.
pub(crate) fn apply(
    nodes: &mut Vec<Node>,
    edges: &mut [Edge],
    traces: &[(&ObservedTrace, &str)],
) -> (usize, usize) {
    let gaps: Vec<(String, StaticSite)> = nodes
        .iter()
        .filter(|node| {
            node.label == "Gap"
                && node.props.get("kind").and_then(serde_json::Value::as_str) == Some(GAP_KIND)
        })
        .filter_map(|node| Some((node.id.clone(), static_site(node)?)))
        .collect();
    if gaps.is_empty() {
        return (0, 0);
    }
    let gap_ids: BTreeSet<&str> = gaps.iter().map(|(id, _)| id.as_str()).collect();
    let per_path = sites_per_path(nodes, edges, &gap_ids);
    let spans: Vec<Seen> = traces
        .iter()
        .flat_map(|(trace, trace_path)| {
            trace
                .spans
                .iter()
                .map(move |span| Seen { span, trace_path })
        })
        .filter(|seen| is_eventbridge(seen.span))
        .collect();
    let (mut resolved, mut unresolved) = (0, 0);
    let mut resolved_ids = BTreeSet::new();
    let mut new_nodes = BTreeMap::<String, Node>::new();

    for (gap_id, site) in &gaps {
        let sites_in_path = per_path.get(&site.path).copied().unwrap_or(1);
        match decide(site, &spans, sites_in_path) {
            Ok(filled) => {
                let channel_id = filled.identity.channel_id();
                let seen = filled.seen;
                if !nodes.iter().any(|node| node.id == channel_id) {
                    new_nodes.entry(channel_id.clone()).or_insert_with(|| {
                        let mut components = serde_json::Map::new();
                        components.insert("bus".into(), filled.identity.bus.as_str().into());
                        scope_json(&filled.identity.scope, &mut components);
                        Node {
                            id: channel_id.clone(),
                            label: "Channel".into(),
                            props: serde_json::json!({
                                "kind": GAP_KIND,
                                "identity": filled.identity.detail_type,
                                "components": components,
                                "bus_source": if filled.bus_from_static { "static" } else { "trace" },
                                "observed": observed(seen.span),
                                "prov": trace_prov(
                                    seen.trace_path,
                                    seen.span,
                                    &format!("Observed Channel {channel_id}"),
                                ),
                            }),
                        }
                    });
                }
                for edge in edges.iter_mut().filter(|edge| &edge.dst == gap_id) {
                    edge.dst = channel_id.clone();
                    let fact = format!("{} {} -> {channel_id}", edge.label, edge.src);
                    edge.props = serde_json::json!({
                        "resolver": OTEL_EXTRACTOR_ID,
                        "prov": trace_prov(seen.trace_path, seen.span, &fact),
                    });
                }
                resolved_ids.insert(gap_id.clone());
                resolved += 1;
            }
            Err(kept) => {
                let mut bus_ids = Vec::new();
                for ((bus, scope), seen) in &kept.bus_observations {
                    let id = bus_channel_id(bus, scope);
                    bus_ids.push(id.clone());
                    if nodes.iter().any(|node| node.id == id) {
                        continue;
                    }
                    new_nodes.entry(id.clone()).or_insert_with(|| {
                        let mut components = serde_json::Map::new();
                        scope_json(scope, &mut components);
                        Node {
                            id: id.clone(),
                            label: "Channel".into(),
                            props: serde_json::json!({
                                "kind": "eventbridge-bus",
                                "identity": bus,
                                "components": components,
                                "observed": observed(seen.span),
                                "prov": trace_prov(
                                    seen.trace_path,
                                    seen.span,
                                    &format!("Observed Channel {id}"),
                                ),
                            }),
                        }
                    });
                }
                if let Some(props) = nodes
                    .iter_mut()
                    .find(|node| &node.id == gap_id)
                    .and_then(|gap| gap.props.as_object_mut())
                {
                    props.insert("attempted_tiers".into(), serde_json::json!(["T0", "T1"]));
                    props.insert("t1_reason".into(), kept.reason.into());
                    if !bus_ids.is_empty() {
                        props.insert("t1_observed_bus".into(), serde_json::json!(bus_ids));
                    }
                }
                unresolved += 1;
            }
        }
    }

    nodes.retain(|node| !resolved_ids.contains(&node.id));
    nodes.extend(new_nodes.into_values());
    (resolved, unresolved)
}
